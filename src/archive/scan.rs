//! 压缩包扫描功能域。
//!
//! 业务意图：
//! - 该模块集中处理 ZIP/RAR/TAR/TAR.GZ/GZ/7Z 的目录扫描、危险路径拒绝、嵌套压缩包展开和 7Z 加载阶段物化。
//! - 调用方只接收与 UI 无关的扫描树，随后再按自身展示模型适配，避免 `archive` 反向依赖日志目录树或 GPUI 状态。
//! - 扫描阶段只读取目录和必要的小型内层压缩包；日志正文读取、编码识别和分页物化仍由其它功能域负责。
//!
//! 关键约束：
//! - 压缩包成员路径必须统一安全归一化，拒绝绝对路径、盘符路径、空路径、NUL 和 `..`。
//! - 内层压缩包只在大小低于阈值时尝试展开，避免加载左侧树时占满内存。
//! - 7Z 顶层普通成员会物化到 session 临时目录，返回的临时路径必须由上层在重新加载或退出时清理。

use std::{
    error::Error,
    fmt::{self, Display},
    fs::{self, File},
    io::{self, BufReader, BufWriter, Cursor, Read},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use flate2::read::GzDecoder;
use tar::Archive as TarArchive;
use zip::ZipArchive;

use super::{
    ArchiveFormat, join_archive_segments, normalize_archive_member_path,
    single_gzip_member_display_name, single_gzip_member_path_for_archive, split_archive_entry_path,
};

/// 扫描嵌套压缩包目录时允许读入内存的最大压缩包成员大小。
///
/// 业务意图：
/// - 外层压缩包里的内层压缩包必须先拿到可 seek 的字节才能读取目录；限制大小可以避免加载树阶段因巨大内层压缩包占满内存。
/// - 超过该上限时仍保留内层压缩包本身作为普通文件节点，用户可以按既有分页/物化路径打开单文件压缩包。
pub(crate) const NESTED_ARCHIVE_SCAN_MAX_BYTES: u64 = 200 * 1024 * 1024;

/// 压缩包扫描阶段返回给日志加载器的完整结果。
///
/// 业务意图：
/// - `children` 表示压缩包根节点下的安全扫描树，不包含 UI 根节点，便于日志加载器按当前来源名称包装。
/// - `error_count` 只统计非致命条目错误，致命错误仍通过 `ArchiveScanError` 返回。
/// - `temporary_paths` 记录 7Z 或嵌套 RAR 扫描时创建的临时路径，由上层统一清理。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveScanResult {
    /// 压缩包根节点下的子节点。
    pub(crate) children: Vec<ArchiveScanNode>,
    /// 扫描过程中转成错误节点的非致命错误数。
    pub(crate) error_count: usize,
    /// 当前扫描创建的临时文件或目录。
    pub(crate) temporary_paths: Vec<PathBuf>,
}

/// 与 UI 无关的压缩包扫描树节点。
///
/// 业务意图：
/// - `archive` 功能域只表达压缩包内部结构和成员来源，不直接依赖日志目录树展示类型。
/// - 日志加载器可以把该节点适配为左侧目录树，也可以在测试中直接断言扫描语义。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveScanNode {
    /// 节点显示名称，已经经过安全路径拆分。
    pub(crate) label: String,
    /// 节点业务类型。
    pub(crate) kind: ArchiveScanEntryKind,
    /// 文件大小、GZIP 类型或错误摘要等补充信息。
    pub(crate) meta: Option<String>,
    /// 非致命错误节点的详细说明。
    pub(crate) error_message: Option<String>,
    /// 文件节点的压缩包来源描述，目录和错误节点为 `None`。
    pub(crate) source: Option<ArchiveMemberSource>,
    /// 子节点列表。
    pub(crate) children: Vec<ArchiveScanNode>,
}

/// 压缩包扫描节点类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArchiveScanEntryKind {
    /// 压缩包内部目录或被展开的内层压缩包。
    Directory,
    /// 可打开的压缩包成员文件。
    File,
    /// 加密条目、非法路径或局部读取失败。
    Error,
}

/// 与 UI 无关的压缩包成员来源描述。
///
/// 业务意图：
/// - 该类型只描述如何重新定位压缩包成员，日志加载器再转换为统一的 `LogFileSource`。
/// - 这样 `archive` 可以独立测试压缩包语义，而不依赖日志目录树或 tab 去重模型。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ArchiveMemberSource {
    /// 顶层压缩包内部成员。
    Direct {
        /// 压缩包文件路径。
        archive_path: PathBuf,
        /// 压缩包格式。
        archive_format: ArchiveFormat,
        /// 安全归一化后的成员路径。
        member_path: String,
    },
    /// 已物化到临时目录的顶层压缩包成员。
    Materialized {
        /// 原始压缩包文件路径。
        archive_path: PathBuf,
        /// 原始压缩包格式。
        archive_format: ArchiveFormat,
        /// 安全归一化后的成员路径。
        member_path: String,
        /// 物化后的本地临时文件。
        temp_path: PathBuf,
    },
    /// 外层压缩包内的内层压缩包成员。
    Nested {
        /// 外层压缩包路径。
        outer_archive_path: PathBuf,
        /// 外层压缩包格式。
        outer_archive_format: ArchiveFormat,
        /// 外层压缩包里的内层压缩包成员路径。
        archive_member_path: String,
        /// 内层压缩包格式。
        nested_archive_format: ArchiveFormat,
        /// 内层压缩包里的目标成员路径。
        nested_member_path: String,
    },
}

/// 压缩包扫描阶段的致命错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveScanError {
    /// 中文错误说明。
    message: String,
}

impl ArchiveScanError {
    /// 创建新的压缩包扫描错误。
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ArchiveScanError {
    /// 将压缩包扫描错误格式化为中文文本。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ArchiveScanError {}

impl ArchiveScanNode {
    /// 创建普通扫描节点。
    fn new(label: impl Into<String>, kind: ArchiveScanEntryKind) -> Self {
        Self {
            label: label.into(),
            kind,
            meta: None,
            error_message: None,
            source: None,
            children: Vec::new(),
        }
    }

    /// 创建错误扫描节点。
    fn error(
        label: impl Into<String>,
        meta: impl Into<String>,
        error_message: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            kind: ArchiveScanEntryKind::Error,
            meta: Some(meta.into()),
            error_message: Some(error_message.into()),
            source: None,
            children: Vec::new(),
        }
    }

    /// 添加普通错误子节点。
    fn add_error_child(
        &mut self,
        label: impl Into<String>,
        meta: impl Into<String>,
        error_message: impl Into<String>,
    ) {
        self.children
            .push(Self::error(label.into(), meta.into(), error_message.into()));
    }

    /// 添加与压缩包内部路径相关的错误节点。
    fn add_archive_error_entry(
        &mut self,
        raw_name: &str,
        meta: impl Into<String>,
        error_message: impl Into<String>,
    ) {
        let label = if raw_name.trim().is_empty() {
            "空压缩包路径".to_string()
        } else {
            format!("非法条目：{}", raw_name)
        };
        self.add_error_child(label, meta, error_message);
    }

    /// 根据安全路径片段添加叶子节点，并自动补齐中间目录。
    fn add_leaf_path(
        &mut self,
        segments: &[String],
        kind: ArchiveScanEntryKind,
        meta: Option<String>,
        source: Option<ArchiveMemberSource>,
        error_message: Option<String>,
    ) {
        if segments.is_empty() {
            return;
        }

        let mut current = self;
        for segment in &segments[..segments.len() - 1] {
            current = current.get_or_insert_child(segment, ArchiveScanEntryKind::Directory);
        }

        let leaf_label = &segments[segments.len() - 1];
        let leaf = current.get_or_insert_child(leaf_label, kind);
        leaf.meta = meta;
        leaf.source = source;
        leaf.error_message = error_message;
    }

    /// 在指定路径挂载已经构建好的内层压缩包子树。
    fn add_subtree_path(&mut self, segments: &[String], subtree: ArchiveScanNode) {
        if segments.is_empty() {
            return;
        }

        let mut current = self;
        for segment in &segments[..segments.len() - 1] {
            current = current.get_or_insert_child(segment, ArchiveScanEntryKind::Directory);
        }

        let leaf_label = &segments[segments.len() - 1];
        let leaf = current.get_or_insert_child(leaf_label, ArchiveScanEntryKind::Directory);
        leaf.meta = subtree.meta;
        leaf.source = None;
        leaf.error_message = subtree.error_message;
        leaf.children.extend(subtree.children);
    }

    /// 查找或插入一个同名同类型子节点。
    fn get_or_insert_child(
        &mut self,
        label: &str,
        kind: ArchiveScanEntryKind,
    ) -> &mut ArchiveScanNode {
        if let Some(index) = self
            .children
            .iter()
            .position(|child| child.label == label && child.kind == kind)
        {
            return &mut self.children[index];
        }

        self.children.push(Self::new(label.to_string(), kind));
        let index = self.children.len() - 1;
        &mut self.children[index]
    }

    /// 递归统计可打开文件节点数量。
    fn descendant_file_count(&self) -> usize {
        match self.kind {
            ArchiveScanEntryKind::File => 1,
            ArchiveScanEntryKind::Directory => {
                self.children.iter().map(Self::descendant_file_count).sum()
            }
            ArchiveScanEntryKind::Error => 0,
        }
    }
}

/// 扫描单个压缩包，返回与 UI 无关的扫描结果。
pub(crate) fn scan_archive(
    path: &Path,
    format: ArchiveFormat,
) -> Result<ArchiveScanResult, ArchiveScanError> {
    let mut root = ArchiveScanNode::new(String::new(), ArchiveScanEntryKind::Directory);
    let mut error_count = 0usize;
    let mut temporary_paths = Vec::new();
    scan_archive_into(
        path,
        format,
        &mut root,
        &mut error_count,
        &mut temporary_paths,
    )?;
    Ok(ArchiveScanResult {
        children: root.children,
        error_count,
        temporary_paths,
    })
}

/// 判断压缩包条目是否适合在加载树阶段尝试扫描为内层压缩包。
///
/// 业务意图：
/// - ZIP、TAR、TAR.GZ 和 7Z 内层压缩包可以基于内存字节读取目录，因此小文件候选项可以直接展开成目录。
/// - GZ 永远只有一个日志流，不会展开出多文件目录；RAR 只有在调用方可以先物化到本地路径时才会继续处理。
///
/// 边界条件：
/// - 目录条目和超过阈值的条目直接返回 `None`，调用方应把它们作为普通目录或文件节点展示。
pub(crate) fn nested_archive_scan_format(
    raw_name: &str,
    is_directory: bool,
    size: u64,
) -> Option<ArchiveFormat> {
    if is_directory || size > NESTED_ARCHIVE_SCAN_MAX_BYTES {
        return None;
    }
    ArchiveFormat::from_path(Path::new(raw_name)).filter(|format| *format != ArchiveFormat::Gzip)
}

/// 按压缩包格式分发扫描逻辑。
///
/// 业务意图：
/// - 格式相关 API 差异集中在本函数附近，调用方只关心压缩包根节点和错误处理。
/// - ZIP/RAR/TAR.GZ 默认读取条目列表，GZ 挂载唯一解压日志；遇到需要路径型 API 的内层压缩包时，会先物化到临时目录再扫描。
/// - 7Z 为改善点击内部小文件的速度，会在这里顺序物化成员到临时目录。
fn scan_archive_into(
    path: &Path,
    format: ArchiveFormat,
    root: &mut ArchiveScanNode,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<(), ArchiveScanError> {
    match format {
        ArchiveFormat::Zip => scan_zip_archive(path, root, error_count, temporary_paths),
        ArchiveFormat::Rar => scan_rar_archive(path, root, error_count, temporary_paths),
        ArchiveFormat::Tar => scan_tar_archive(path, root, error_count, temporary_paths),
        ArchiveFormat::TarGz => scan_tar_gz_archive(path, root, error_count, temporary_paths),
        ArchiveFormat::Gzip => scan_gzip_archive(path, root),
        ArchiveFormat::SevenZ => scan_7z_archive(path, root, error_count, temporary_paths),
    }
}

/// 扫描 ZIP 压缩包目录项。
///
/// 业务意图：
/// - `zip` crate 支持按索引读取条目元数据，适合当前只构建压缩包扫描树的需求。
/// - 读取条目名称、目录标记和未压缩大小即可，不读取正文内容。
///
/// 边界条件：
/// - ZIP 条目名可能包含不安全路径，必须交给 `add_archive_entry` 统一校验。
fn scan_zip_archive(
    path: &Path,
    root: &mut ArchiveScanNode,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<(), ArchiveScanError> {
    let file = File::open(path).map_err(|error| {
        ArchiveScanError::new(format!("无法打开 ZIP 压缩包 {}：{}", path.display(), error))
    })?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|error| ArchiveScanError::new(format!("无法读取 ZIP 目录：{}", error)))?;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| {
            ArchiveScanError::new(format!("无法读取 ZIP 条目 {}：{}", index, error))
        })?;
        let raw_name = entry.name().to_string();
        let is_directory = entry.is_dir();
        let size = entry.size();
        if !is_directory
            && let Some(nested_format) = nested_archive_scan_format(&raw_name, is_directory, size)
            && let Some(nested_tree) = read_nested_archive_tree_from_reader(
                &mut entry,
                Some(size),
                nested_format,
                NestedArchiveReaderContext {
                    outer_archive_path: path,
                    outer_archive_format: ArchiveFormat::Zip,
                    archive_member_path: &raw_name,
                    error_count,
                    temporary_paths,
                },
            )
        {
            add_nested_archive_tree(root, &raw_name, nested_tree, error_count);
            continue;
        }

        add_archive_entry(
            root,
            path,
            ArchiveFormat::Zip,
            &raw_name,
            is_directory,
            Some(size),
            error_count,
        );
    }

    Ok(())
}

/// 扫描 RAR 压缩包目录项。
///
/// 业务意图：
/// - `unrar` crate 封装 RARLAB unrar 库的列表能力，可以在不解压文件的前提下读取条目元数据。
/// - 普通条目只使用列表模式；当条目本身是压缩包时，为了判断是否应作为目录展开，会把该条目物化到临时文件再扫描。
///
/// 边界条件：
/// - 加密文件没有密码规则，当前以错误节点展示，后续需要用户确认密码输入和缓存策略后再支持。
/// - `unrar` 依赖底层 unrar 实现，跨平台构建如遇工具链问题需要在对应平台单独验收。
fn scan_rar_archive(
    path: &Path,
    root: &mut ArchiveScanNode,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<(), ArchiveScanError> {
    let archive = unrar::Archive::new(path)
        .open_for_listing()
        .map_err(|error| ArchiveScanError::new(format!("无法打开 RAR 目录：{}", error)))?;
    let mut materialized_root: Option<PathBuf> = None;

    for entry in archive {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                *error_count += 1;
                root.add_error_child("RAR 条目读取失败", "读取失败", error.to_string());
                continue;
            }
        };

        let raw_name = entry.filename.to_string_lossy();
        if entry.is_encrypted() {
            *error_count += 1;
            root.add_archive_error_entry(&raw_name, "加密条目", "加密 RAR 条目暂不支持读取");
            continue;
        }

        if let Some(nested_format) =
            nested_archive_scan_format(&raw_name, false, entry.unpacked_size)
        {
            let Some(nested_member_path) =
                normalized_rar_nested_archive_member_path(&raw_name, false, entry.unpacked_size)
            else {
                *error_count += 1;
                root.add_archive_error_entry(&raw_name, "非法路径", "压缩包成员路径非法");
                continue;
            };
            match materialize_rar_member_for_nested_scan(
                path,
                &nested_member_path,
                &mut materialized_root,
                temporary_paths,
            ) {
                Ok(temp_path) => {
                    if let Some(nested_tree) = read_nested_archive_tree_from_path(
                        &temp_path,
                        nested_format,
                        path,
                        ArchiveFormat::Rar,
                        &nested_member_path,
                        error_count,
                        temporary_paths,
                    ) {
                        add_nested_archive_tree(
                            root,
                            &nested_member_path,
                            nested_tree,
                            error_count,
                        );
                        continue;
                    }
                }
                Err(error) => {
                    *error_count += 1;
                    root.add_archive_error_entry(
                        &raw_name,
                        "内层压缩包读取失败",
                        error.to_string(),
                    );
                    continue;
                }
            }
        }

        add_archive_entry(
            root,
            path,
            ArchiveFormat::Rar,
            &raw_name,
            entry.is_directory(),
            Some(entry.unpacked_size),
            error_count,
        );
    }

    Ok(())
}

/// 扫描未压缩 TAR 归档目录项。
///
/// 业务意图：
/// - 兼容真实格式为 tar、但扩展名可能被写成 `.tar.gz` 的现场日志包。
/// - TAR 和 TAR.GZ 的目录模型相同，差异只在是否先经过 gzip 解码。
///
/// 边界条件：
/// - TAR 是顺序格式，扫描目录时仍需消费每个文件条目正文，才能继续读取后续条目。
/// - 嵌套压缩包会先尝试小文件内存扫描，多文件内层压缩包作为目录展开。
fn scan_tar_archive(
    path: &Path,
    root: &mut ArchiveScanNode,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<(), ArchiveScanError> {
    let file = File::open(path).map_err(|error| {
        ArchiveScanError::new(format!("无法打开 TAR 压缩包 {}：{}", path.display(), error))
    })?;
    let mut archive = TarArchive::new(BufReader::new(file));
    let entries = archive
        .entries()
        .map_err(|error| ArchiveScanError::new(format!("无法读取 TAR 目录：{}", error)))?;

    for entry in entries {
        let mut entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                *error_count += 1;
                root.add_error_child("TAR 条目读取失败", "读取失败", error.to_string());
                continue;
            }
        };

        let entry_path = match entry.path() {
            Ok(path) => path,
            Err(error) => {
                *error_count += 1;
                root.add_error_child("TAR 路径读取失败", "路径错误", error.to_string());
                continue;
            }
        };
        let raw_name = entry_path.to_string_lossy().to_string();
        let is_directory = entry.header().entry_type().is_dir();
        let size = entry.size();
        let mut replaced_with_nested_tree = false;
        let mut entry_drained = false;

        if !is_directory
            && let Some(nested_format) = nested_archive_scan_format(&raw_name, is_directory, size)
        {
            if let Some(nested_tree) = read_nested_archive_tree_from_reader(
                &mut entry,
                Some(size),
                nested_format,
                NestedArchiveReaderContext {
                    outer_archive_path: path,
                    outer_archive_format: ArchiveFormat::Tar,
                    archive_member_path: &raw_name,
                    error_count,
                    temporary_paths,
                },
            ) {
                add_nested_archive_tree(root, &raw_name, nested_tree, error_count);
                replaced_with_nested_tree = true;
            }
            if let Err(error) = drain_tar_entry(&mut entry) {
                *error_count += 1;
                root.add_error_child("TAR 条目跳过失败", "读取失败", error.to_string());
                continue;
            }
            entry_drained = true;
            if replaced_with_nested_tree {
                continue;
            }
        }

        if !is_directory
            && !entry_drained
            && let Err(error) = drain_tar_entry(&mut entry)
        {
            *error_count += 1;
            root.add_error_child("TAR 条目跳过失败", "读取失败", error.to_string());
            continue;
        }

        add_archive_entry(
            root,
            path,
            ArchiveFormat::Tar,
            &raw_name,
            is_directory,
            Some(size),
            error_count,
        );
    }

    Ok(())
}

/// 扫描 tar.gz 或 tgz 压缩包目录项。
///
/// 业务意图：
/// - `flate2` 负责 gzip 解码，`tar` 负责遍历 tar 头部条目，两者组合能流式读取归档目录。
/// - 当前读取条目头和路径，不把条目内容写入磁盘。
///
/// 边界条件：
/// - tar 路径可能包含平台分隔符或危险片段，必须交给 `add_archive_entry` 校验。
fn scan_tar_gz_archive(
    path: &Path,
    root: &mut ArchiveScanNode,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<(), ArchiveScanError> {
    let file = File::open(path).map_err(|error| {
        ArchiveScanError::new(format!(
            "无法打开 TAR.GZ 压缩包 {}：{}",
            path.display(),
            error
        ))
    })?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let mut archive = TarArchive::new(decoder);
    let entries = match archive.entries() {
        Ok(entries) => entries,
        Err(error) => {
            if add_single_gzip_payload_entry(path, ArchiveFormat::TarGz, root) {
                return Ok(());
            }
            return Err(ArchiveScanError::new(format!(
                "无法读取 TAR.GZ 目录：{}",
                error
            )));
        }
    };
    let mut valid_entry_count = 0usize;
    let mut deferred_entry_errors = Vec::new();

    for entry in entries {
        let mut entry = match entry {
            Ok(entry) => {
                valid_entry_count += 1;
                entry
            }
            Err(error) => {
                // tar crate 会在 gzip 内容不是 tar 头时从迭代器返回条目错误。
                // 这里先延迟展示错误，等循环结束后确认是否可以按“单文件 gzip 日志”降级处理。
                deferred_entry_errors.push(error.to_string());
                continue;
            }
        };

        let entry_path = match entry.path() {
            Ok(path) => path,
            Err(error) => {
                *error_count += 1;
                root.add_error_child("TAR.GZ 路径读取失败", "路径错误", error.to_string());
                continue;
            }
        };
        let raw_name = entry_path.to_string_lossy().to_string();
        let is_directory = entry.header().entry_type().is_dir();
        let size = entry.size();
        let mut replaced_with_nested_tree = false;
        let mut entry_drained = false;
        if !is_directory
            && let Some(nested_format) = nested_archive_scan_format(&raw_name, is_directory, size)
        {
            if let Some(nested_tree) = read_nested_archive_tree_from_reader(
                &mut entry,
                Some(size),
                nested_format,
                NestedArchiveReaderContext {
                    outer_archive_path: path,
                    outer_archive_format: ArchiveFormat::TarGz,
                    archive_member_path: &raw_name,
                    error_count,
                    temporary_paths,
                },
            ) {
                add_nested_archive_tree(root, &raw_name, nested_tree, error_count);
                replaced_with_nested_tree = true;
            }
            if let Err(error) = drain_tar_entry(&mut entry) {
                *error_count += 1;
                root.add_error_child("TAR.GZ 条目跳过失败", "读取失败", error.to_string());
                continue;
            }
            entry_drained = true;
            if replaced_with_nested_tree {
                continue;
            }
        }

        if !is_directory
            && !entry_drained
            && let Err(error) = drain_tar_entry(&mut entry)
        {
            *error_count += 1;
            root.add_error_child("TAR.GZ 条目跳过失败", "读取失败", error.to_string());
            continue;
        }

        add_archive_entry(
            root,
            path,
            ArchiveFormat::TarGz,
            &raw_name,
            is_directory,
            Some(size),
            error_count,
        );
    }

    if valid_entry_count == 0
        && !deferred_entry_errors.is_empty()
        && add_single_gzip_payload_entry(path, ArchiveFormat::TarGz, root)
    {
        return Ok(());
    }

    for error in deferred_entry_errors {
        *error_count += 1;
        root.add_error_child("TAR.GZ 条目读取失败", "读取失败", error);
    }

    Ok(())
}

/// 扫描单文件 GZIP 日志并挂载唯一可打开节点。
///
/// 业务意图：
/// - `.gz` 没有目录结构，只表示一个压缩后的日志流；压缩包扫描树仍按“压缩包根 + 内部文件”展示，保持和其它压缩格式一致。
/// - 扫描阶段只探测 gzip 能否开始解压，不把日志正文读入内存，避免大日志在加载树时阻塞 UI。
///
/// 边界条件：
/// - 损坏或非 gzip 内容不能降级为普通日志，否则用户会看到压缩二进制乱码。
/// - 空 gzip 文件允许作为空日志打开，探测读取到 0 字节但没有错误时视为有效。
fn scan_gzip_archive(path: &Path, root: &mut ArchiveScanNode) -> Result<(), ArchiveScanError> {
    if add_single_gzip_payload_entry(path, ArchiveFormat::Gzip, root) {
        return Ok(());
    }

    Err(ArchiveScanError::new(format!(
        "无法读取 GZIP 日志 {}：文件不是有效 gzip 内容或已损坏",
        path.display()
    )))
}

/// 把单个 gzip 日志挂载成压缩包根节点下的虚拟文件节点。
///
/// 业务意图：
/// - 真实世界中既有扩展名叫 `.tar.gz` 但内容只是 `gzip log` 的文件，也有标准 `.gz` 单文件压缩日志。
/// - 这类文件没有目录，确认 gzip 层可读后挂载一个虚拟文件节点，复用后续读取、搜索、另存为和分页物化逻辑。
///
/// 边界条件：
/// - `.tar.gz` fallback 只有在 tar 条目完全不可读时才调用；正常 TAR.GZ 不受影响。
/// - 这里最多读取解压后的 1 字节用于校验，不把日志内容加载到内存。
fn add_single_gzip_payload_entry(
    path: &Path,
    archive_format: ArchiveFormat,
    root: &mut ArchiveScanNode,
) -> bool {
    if !single_gzip_payload_is_readable(path) {
        return false;
    }

    let member_path = single_gzip_member_path_for_archive(path);
    let label = single_gzip_member_display_name(&member_path);
    let meta = fs::metadata(path)
        .ok()
        .map(|metadata| format!("GZIP {}", format_byte_size(metadata.len())))
        .or_else(|| Some("GZIP".to_string()));
    root.add_leaf_path(
        &[label],
        ArchiveScanEntryKind::File,
        meta,
        Some(ArchiveMemberSource::Direct {
            archive_path: path.to_path_buf(),
            archive_format,
            member_path,
        }),
        None,
    );
    true
}

/// 判断 gzip 层是否至少可以正常开始解压。
///
/// 业务意图：
/// - 该函数只用于区分“不是 tar 但 gzip 有效”和“文件损坏/不是 gzip”，避免把真正损坏的压缩包误展示成日志。
fn single_gzip_payload_is_readable(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut decoder = GzDecoder::new(BufReader::new(file));
    let mut probe = [0_u8; 1];
    decoder.read(&mut probe).is_ok()
}

/// 排空 TAR 条目正文，推进顺序流到下一个条目头。
///
/// 业务意图：
/// - TAR.GZ 没有 ZIP 那样的中央目录，遍历下一个条目前必须消费当前条目的正文。
/// - 压缩包扫描树扫描只需要路径和大小，但也要把正文读到 `io::sink()`，否则下一次迭代会把正文误读成 tar 头并显示“条目读取失败”。
///
/// 边界条件：
/// - 这里不把内容保存到内存或磁盘，只顺序丢弃，因此不会改变点击文件时的真实读取来源。
fn drain_tar_entry<R: Read>(entry: &mut tar::Entry<'_, R>) -> io::Result<()> {
    io::copy(entry, &mut io::sink()).map(|_| ())
}

/// 扫描 7z 压缩包目录项。
///
/// 业务意图：
/// - 7Z，尤其 solid 7Z，随机打开内部小文件时必须顺序解压前置条目，会导致每次点击都等待很久。
/// - 这里在加载压缩包扫描树阶段把普通成员顺序物化到 session 临时目录，后续左侧树点击直接读取本地临时文件。
///
/// 边界条件：
/// - 加密 7z 或损坏头部会返回读取错误，当前展示为压缩包根节点下的错误节点。
/// - 物化会占用磁盘空间；路径记录在 `temporary_paths` 中，由 UI 在重新加载或启动期过期清理时释放。
fn scan_7z_archive(
    path: &Path,
    root: &mut ArchiveScanNode,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<(), ArchiveScanError> {
    let mut reader = sevenz_rust::SevenZReader::open(path, sevenz_rust::Password::empty())
        .map_err(|error| ArchiveScanError::new(format!("无法读取 7Z 内容：{}", error)))?;
    let materialized_root = sevenz_materialized_root(path)?;
    fs::create_dir_all(&materialized_root).map_err(|error| {
        ArchiveScanError::new(format!(
            "无法创建 7Z 临时物化目录 {}：{}",
            materialized_root.display(),
            error
        ))
    })?;
    temporary_paths.push(materialized_root.clone());

    let scan_result = reader
        .for_each_entries(|entry, entry_reader| {
            let raw_name = entry.name().to_string();
            if entry.is_directory() {
                add_archive_directory_entry(root, &raw_name, error_count);
                return Ok(true);
            }

            let segments = match split_archive_entry_path(&raw_name) {
                Ok(segments) => segments,
                Err(reason) => {
                    // 7Z 条目即使路径非法也必须消费当前 reader；尤其是 solid archive，
                    // 不 drain 会破坏后续条目的顺序解压状态。该条目作为错误节点展示，其它合法文件继续加载。
                    *error_count += 1;
                    root.add_archive_error_entry(&raw_name, "非法路径", reason);
                    io::copy(entry_reader, &mut io::sink()).map_err(sevenz_rust::Error::io)?;
                    return Ok(true);
                }
            };
            let temp_path = materialized_7z_member_path(&materialized_root, &segments);
            if let Some(parent) = temp_path.parent() {
                fs::create_dir_all(parent).map_err(sevenz_rust::Error::io)?;
            }
            let mut writer =
                BufWriter::new(File::create(&temp_path).map_err(sevenz_rust::Error::io)?);
            io::copy(entry_reader, &mut writer).map_err(sevenz_rust::Error::io)?;

            if let Some(nested_format) = nested_archive_scan_format(&raw_name, false, entry.size)
                && let Some(nested_tree) = read_nested_archive_tree_from_path(
                    &temp_path,
                    nested_format,
                    path,
                    ArchiveFormat::SevenZ,
                    &raw_name,
                    error_count,
                    temporary_paths,
                )
            {
                add_nested_archive_tree(root, &raw_name, nested_tree, error_count);
                return Ok(true);
            }

            add_materialized_7z_file_entry(root, path, &segments, entry.size, temp_path);
            Ok(true)
        })
        .map_err(|error| ArchiveScanError::new(format!("读取 7Z 内容失败：{}", error)));
    if scan_result.is_err() {
        let _ = fs::remove_dir_all(&materialized_root);
    }
    scan_result?;

    Ok(())
}

/// 为扫描内层压缩包创建会话级临时根目录。
///
/// 业务意图：
/// - RAR 的目录扫描和读取 API 都需要真实文件路径，无法直接从内存 reader 扫描。
/// - 外层 RAR 或 ZIP/TAR.GZ 中的内层 RAR 需要先物化为临时文件，再复用 `scan_archive`。
///
/// 边界条件：
/// - 临时目录加入 `temporary_paths`，由 UI 在重新加载或退出时清理。
/// - 目录名包含时间戳和来源标签，避免同一进程多次加载同名归档互相覆盖。
fn nested_archive_scan_materialized_root(label: &str) -> Result<PathBuf, ArchiveScanError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ArchiveScanError::new(format!("无法生成内层压缩包临时时间戳：{}", error)))?
        .as_nanos();
    let safe_label = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    Ok(std::env::temp_dir()
        .join("LogClinic")
        .join("large-log-cache")
        .join(format!("session-{}", std::process::id()))
        .join(format!("nested-archive-{}-{}", nanos, safe_label)))
}

/// 计算物化后的内层压缩包文件路径。
fn materialized_nested_archive_path(
    root: &Path,
    raw_name: &str,
) -> Result<PathBuf, ArchiveScanError> {
    let segments = split_archive_entry_path(raw_name).map_err(|reason| {
        ArchiveScanError::new(format!("内层压缩包路径非法，无法物化：{}", reason))
    })?;
    let mut path = root.to_path_buf();
    for segment in segments {
        path.push(segment);
    }
    Ok(path)
}

/// 返回 RAR 嵌套压缩包扫描时使用的安全成员路径。
///
/// 业务意图：
/// - RAR 条目名可能来自 Windows 压缩工具并使用反斜杠；物化、读取和挂载树必须使用同一个 `/` 分隔路径。
/// - 该函数把“识别为内层压缩包”和“路径安全归一化”放在同一处，避免不同调用点使用原始路径导致匹配失败。
pub(crate) fn normalized_rar_nested_archive_member_path(
    raw_name: &str,
    is_directory: bool,
    size: u64,
) -> Option<String> {
    nested_archive_scan_format(raw_name, is_directory, size)?;
    normalize_archive_member_path(raw_name).ok()
}

/// 将 RAR 成员物化为本地文件，供内层压缩包目录扫描使用。
///
/// 业务意图：
/// - `unrar` 的列表接口只能告诉我们条目名和大小，不能直接把条目 reader 交给 ZIP/7Z/TAR 扫描。
/// - 只在条目扩展名已经确认是受支持压缩包时调用，避免对普通日志额外解压。
fn materialize_rar_member_for_nested_scan(
    archive_path: &Path,
    member_path: &str,
    materialized_root: &mut Option<PathBuf>,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<PathBuf, ArchiveScanError> {
    if materialized_root.is_none() {
        let label = archive_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive.rar".to_string());
        let root = nested_archive_scan_materialized_root(&label)?;
        fs::create_dir_all(&root).map_err(|error| {
            ArchiveScanError::new(format!(
                "无法创建内层压缩包临时目录 {}：{}",
                root.display(),
                error
            ))
        })?;
        temporary_paths.push(root.clone());
        *materialized_root = Some(root);
    }

    let root = materialized_root
        .as_ref()
        .ok_or_else(|| ArchiveScanError::new("内部错误：内层压缩包临时目录未初始化"))?;
    let temp_path = materialized_nested_archive_path(root, member_path)?;
    if let Some(parent) = temp_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            ArchiveScanError::new(format!(
                "无法创建内层压缩包父目录 {}：{}",
                parent.display(),
                error
            ))
        })?;
    }

    let mut archive = unrar::Archive::new(archive_path)
        .open_for_processing()
        .map_err(|error| ArchiveScanError::new(format!("无法打开 RAR 内容：{}", error)))?;
    while let Some(header) = archive
        .read_header()
        .map_err(|error| ArchiveScanError::new(format!("无法读取 RAR 条目：{}", error)))?
    {
        let raw_name = header.entry().filename.to_string_lossy();
        let normalized = match normalize_archive_member_path(&raw_name) {
            Ok(path) => path,
            Err(_) => {
                archive = header.skip().map_err(|error| {
                    ArchiveScanError::new(format!("无法跳过非法 RAR 条目：{}", error))
                })?;
                continue;
            }
        };
        if normalized != member_path {
            archive = header
                .skip()
                .map_err(|error| ArchiveScanError::new(format!("无法跳过 RAR 条目：{}", error)))?;
            continue;
        }
        if header.entry().is_encrypted() {
            return Err(ArchiveScanError::new(format!(
                "RAR 成员 {} 已加密，暂不支持展开为目录",
                member_path
            )));
        }
        if header.entry().is_directory() {
            return Err(ArchiveScanError::new(format!(
                "RAR 成员 {} 是目录，不能作为内层压缩包扫描",
                member_path
            )));
        }
        header.extract_to(&temp_path).map_err(|error| {
            ArchiveScanError::new(format!("无法物化 RAR 内层压缩包：{}", error))
        })?;
        return Ok(temp_path);
    }

    Err(ArchiveScanError::new(format!(
        "RAR 压缩包中未找到内层压缩包 {}",
        member_path
    )))
}

/// 为当前顶层 7Z 创建会话级物化根目录。
///
/// 业务意图：
/// - 7Z 内部成员被转成本地临时文件后，左侧树后续点击无需再次顺序解压整个归档。
/// - 目录名包含进程 ID、时间戳和压缩包文件名，降低同一会话重复加载同名压缩包时的冲突概率。
fn sevenz_materialized_root(path: &Path) -> Result<PathBuf, ArchiveScanError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ArchiveScanError::new(format!("无法生成 7Z 临时目录时间戳：{}", error)))?
        .as_nanos();
    let label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive.7z".to_string())
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    Ok(std::env::temp_dir()
        .join("LogClinic")
        .join("large-log-cache")
        .join(format!("session-{}", std::process::id()))
        .join(format!("sevenz-tree-{}-{}", nanos, label)))
}

/// 计算 7Z 成员的临时物化路径。
///
/// 边界条件：
/// - 成员名必须先经过压缩包路径安全归一化，拒绝绝对路径、盘符和 `..`，避免写出临时根目录。
fn materialized_7z_member_path(root: &Path, segments: &[String]) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in segments {
        path.push(segment);
    }
    path
}

/// 添加已经物化到本地临时目录的 7Z 普通文件节点。
///
/// 业务意图：
/// - 物化后的文件读取走普通本地临时路径，但来源语义仍保留原始 7Z 成员路径。
/// - 另存为和搜索结果需要内部路径层级，不能把临时文件名当作用户选择的真实来源。
fn add_materialized_7z_file_entry(
    root: &mut ArchiveScanNode,
    archive_path: &Path,
    segments: &[String],
    size: u64,
    temp_path: PathBuf,
) {
    let member_path = join_archive_segments(segments);
    root.add_leaf_path(
        segments,
        ArchiveScanEntryKind::File,
        Some(format_byte_size(size)),
        Some(ArchiveMemberSource::Materialized {
            archive_path: archive_path.to_path_buf(),
            archive_format: ArchiveFormat::SevenZ,
            member_path,
            temp_path,
        }),
        None,
    );
}

/// 添加 7Z 中显式出现的目录节点。
///
/// 边界条件：
/// - 一些 7Z 文件没有显式目录条目，只在文件路径中隐含目录；这种情况由 `add_leaf_path` 自动补齐。
fn add_archive_directory_entry(
    root: &mut ArchiveScanNode,
    raw_name: &str,
    error_count: &mut usize,
) {
    let segments = match split_archive_entry_path(raw_name) {
        Ok(segments) => segments,
        Err(reason) => {
            *error_count += 1;
            root.add_archive_error_entry(raw_name, "非法路径", reason);
            return;
        }
    };
    root.add_leaf_path(&segments, ArchiveScanEntryKind::Directory, None, None, None);
}

/// 从已经物化成本地文件的内层压缩包读取压缩包扫描树。
///
/// 业务意图：
/// - 顶层 7Z 成员已经落到临时目录后，扫描内层压缩包不再需要再次从顶层 7Z 顺序解压。
/// - 如果内层压缩包包含多个普通文件，继续作为目录挂载；单文件压缩包仍按既有规则作为文件本身打开。
fn read_nested_archive_tree_from_path(
    archive_path: &Path,
    nested_format: ArchiveFormat,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Option<ArchiveScanNode> {
    let nested_format = if nested_format == ArchiveFormat::TarGz {
        ArchiveFormat::from_file(archive_path).unwrap_or(nested_format)
    } else {
        nested_format
    };
    let segments = split_archive_entry_path(archive_member_path).ok()?;
    let label = segments.last()?.clone();
    let mut nested_root = ArchiveScanNode::new(label, ArchiveScanEntryKind::Directory);
    scan_archive_into(
        archive_path,
        nested_format,
        &mut nested_root,
        error_count,
        temporary_paths,
    )
    .ok()?;

    if nested_root.descendant_file_count() <= 1 {
        return None;
    }

    if outer_archive_format != ArchiveFormat::SevenZ {
        rewrite_nested_local_sources(
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
        );
    }
    Some(nested_root)
}

/// 将内层压缩包压缩包扫描树中的普通来源改写为嵌套来源。
///
/// 业务意图：
/// - ZIP/TAR.GZ 内层压缩包扫描会产生 `ArchiveMember`，需要保留“外层压缩包 + 内层压缩包路径 + 内层成员路径”的语义。
/// - 7Z 内层压缩包会先物化为本地文件，此时保留 `LocalFile` 来源即可避免再次随机读取 7Z。
fn rewrite_nested_local_sources(
    node: &mut ArchiveScanNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
) {
    if let Some(ArchiveMemberSource::Direct { member_path, .. }) = node.source.clone() {
        node.source = Some(ArchiveMemberSource::Nested {
            outer_archive_path: outer_archive_path.to_path_buf(),
            outer_archive_format,
            archive_member_path: archive_member_path.to_string(),
            nested_archive_format,
            nested_member_path: member_path,
        });
    }

    for child in &mut node.children {
        rewrite_nested_local_sources(
            child,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_archive_format,
        );
    }
}

/// 尝试从外层压缩包条目中读取内层压缩包压缩包扫描树。
///
/// 业务意图：
/// - 外层压缩包内如果包含多文件压缩包，用户希望把该内层压缩包当作目录展开并选择具体文件。
/// - 只有内层压缩包包含两个及以上普通文件时才替换成目录；单文件内层压缩包继续保留“当作文件本身打开”的既有行为。
///
/// 边界条件：
/// - 加载树阶段不能为巨大内层压缩包无上限占用内存，超过 `NESTED_ARCHIVE_SCAN_MAX_BYTES` 时直接回退为普通文件节点。
/// - RAR 内层压缩包需要路径型 API，因此会先写入临时文件，再复用普通压缩包扫描逻辑。
/// - 参数对象分别描述外层来源、错误计数和临时路径归属，避免嵌套扫描入口继续拉长签名。
struct NestedArchiveReaderContext<'a> {
    /// 外层压缩包路径。
    outer_archive_path: &'a Path,
    /// 外层压缩包格式。
    outer_archive_format: ArchiveFormat,
    /// 外层压缩包中内层压缩包成员路径。
    archive_member_path: &'a str,
    /// 当前压缩包扫描树加载的错误计数。
    error_count: &'a mut usize,
    /// 当前加载会话需要清理的临时路径。
    temporary_paths: &'a mut Vec<PathBuf>,
}

/// 尝试从外层压缩包条目中读取内层压缩包压缩包扫描树。
fn read_nested_archive_tree_from_reader(
    reader: &mut dyn Read,
    declared_size: Option<u64>,
    nested_format: ArchiveFormat,
    context: NestedArchiveReaderContext<'_>,
) -> Option<ArchiveScanNode> {
    let NestedArchiveReaderContext {
        outer_archive_path,
        outer_archive_format,
        archive_member_path,
        error_count,
        temporary_paths,
    } = context;

    let size = declared_size?;
    if size > NESTED_ARCHIVE_SCAN_MAX_BYTES {
        return None;
    }

    let mut bytes = Vec::with_capacity(size.min(1024 * 1024) as usize);
    let mut limited_reader = reader.take(NESTED_ARCHIVE_SCAN_MAX_BYTES + 1);
    if limited_reader.read_to_end(&mut bytes).is_err()
        || bytes.len() as u64 > NESTED_ARCHIVE_SCAN_MAX_BYTES
    {
        return None;
    }

    let nested_format = nested_format.resolve_from_bytes(&bytes);

    if nested_format == ArchiveFormat::Rar {
        let temp_path =
            materialize_nested_archive_bytes_for_scan(&bytes, archive_member_path, temporary_paths)
                .ok()?;
        return read_nested_archive_tree_from_path(
            &temp_path,
            nested_format,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            error_count,
            temporary_paths,
        );
    }

    let segments = split_archive_entry_path(archive_member_path).ok()?;
    let label = segments.last()?.clone();
    let mut nested_root = ArchiveScanNode::new(label, ArchiveScanEntryKind::Directory);
    match nested_format {
        ArchiveFormat::Zip => scan_nested_zip_archive(
            &bytes,
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
            error_count,
        ),
        ArchiveFormat::Tar => scan_nested_tar_archive(
            &bytes,
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
            error_count,
        ),
        ArchiveFormat::TarGz => scan_nested_tar_gz_archive(
            &bytes,
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
            error_count,
        ),
        ArchiveFormat::SevenZ => scan_nested_7z_archive(
            &bytes,
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
            error_count,
        ),
        ArchiveFormat::Gzip => return None,
        ArchiveFormat::Rar => return None,
    }
    .ok()?;

    (nested_root.descendant_file_count() > 1).then_some(nested_root)
}

/// 将内存中的内层压缩包字节写入临时文件。
///
/// 业务意图：
/// - RAR 只能从路径读取目录；当 RAR 位于 ZIP/TAR.GZ 这类 reader 型外层压缩包中时，需要先落盘。
/// - 只写入小于扫描阈值的内层压缩包，避免目录构建阶段消耗大量磁盘和内存。
fn materialize_nested_archive_bytes_for_scan(
    bytes: &[u8],
    archive_member_path: &str,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<PathBuf, ArchiveScanError> {
    let root = nested_archive_scan_materialized_root(archive_member_path)?;
    fs::create_dir_all(&root).map_err(|error| {
        ArchiveScanError::new(format!(
            "无法创建内存内层压缩包临时目录 {}：{}",
            root.display(),
            error
        ))
    })?;
    temporary_paths.push(root.clone());
    let temp_path = materialized_nested_archive_path(&root, archive_member_path)?;
    if let Some(parent) = temp_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            ArchiveScanError::new(format!(
                "无法创建内存内层压缩包父目录 {}：{}",
                parent.display(),
                error
            ))
        })?;
    }
    fs::write(&temp_path, bytes).map_err(|error| {
        ArchiveScanError::new(format!(
            "无法写入内存内层压缩包临时文件 {}：{}",
            temp_path.display(),
            error
        ))
    })?;
    Ok(temp_path)
}

/// 扫描内存中的 ZIP 内层压缩包。
fn scan_nested_zip_archive(
    archive_bytes: &[u8],
    root: &mut ArchiveScanNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    error_count: &mut usize,
) -> Result<(), ArchiveScanError> {
    let mut archive = ZipArchive::new(Cursor::new(archive_bytes))
        .map_err(|error| ArchiveScanError::new(format!("无法读取嵌套 ZIP 目录：{}", error)))?;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            ArchiveScanError::new(format!("无法读取嵌套 ZIP 条目 {}：{}", index, error))
        })?;
        add_nested_archive_entry(
            root,
            NestedArchiveEntryContext {
                outer_archive_path,
                outer_archive_format,
                archive_member_path,
                nested_archive_format,
            },
            NestedArchiveEntryInfo {
                raw_name: entry.name(),
                is_directory: entry.is_dir(),
                size: Some(entry.size()),
            },
            error_count,
        );
    }
    Ok(())
}

/// 扫描内存中的 TAR 内层压缩包。
///
/// 业务意图：
/// - 外层压缩包中的 `.tar` 或误命名 `.tar.gz` 成员，如果包含多个文件，需要在左侧树中作为目录展开。
/// - 该函数不经过 gzip 解码，专门处理纯 tar 字节。
fn scan_nested_tar_archive(
    archive_bytes: &[u8],
    root: &mut ArchiveScanNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    error_count: &mut usize,
) -> Result<(), ArchiveScanError> {
    let mut archive = TarArchive::new(Cursor::new(archive_bytes));
    let entries = archive
        .entries()
        .map_err(|error| ArchiveScanError::new(format!("无法读取嵌套 TAR 目录：{}", error)))?;
    for entry in entries {
        let mut entry = entry
            .map_err(|error| ArchiveScanError::new(format!("无法读取嵌套 TAR 条目：{}", error)))?;
        let entry_path = entry.path().map_err(|error| {
            ArchiveScanError::new(format!("无法读取嵌套 TAR 条目路径：{}", error))
        })?;
        let raw_name = entry_path.to_string_lossy().to_string();
        let is_directory = entry.header().entry_type().is_dir();
        let size = entry.size();
        if !is_directory {
            drain_tar_entry(&mut entry).map_err(|error| {
                ArchiveScanError::new(format!("无法跳过嵌套 TAR 条目内容：{}", error))
            })?;
        }
        add_nested_archive_entry(
            root,
            NestedArchiveEntryContext {
                outer_archive_path,
                outer_archive_format,
                archive_member_path,
                nested_archive_format,
            },
            NestedArchiveEntryInfo {
                raw_name: &raw_name,
                is_directory,
                size: Some(size),
            },
            error_count,
        );
    }
    Ok(())
}

/// 扫描内存中的 TAR.GZ 内层压缩包。
fn scan_nested_tar_gz_archive(
    archive_bytes: &[u8],
    root: &mut ArchiveScanNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    error_count: &mut usize,
) -> Result<(), ArchiveScanError> {
    let decoder = GzDecoder::new(Cursor::new(archive_bytes));
    let mut archive = TarArchive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| ArchiveScanError::new(format!("无法读取嵌套 TAR.GZ 目录：{}", error)))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| {
            ArchiveScanError::new(format!("无法读取嵌套 TAR.GZ 条目：{}", error))
        })?;
        let entry_path = entry.path().map_err(|error| {
            ArchiveScanError::new(format!("无法读取嵌套 TAR.GZ 条目路径：{}", error))
        })?;
        let raw_name = entry_path.to_string_lossy().to_string();
        let is_directory = entry.header().entry_type().is_dir();
        let size = entry.size();
        if !is_directory {
            drain_tar_entry(&mut entry).map_err(|error| {
                ArchiveScanError::new(format!("无法跳过嵌套 TAR.GZ 条目内容：{}", error))
            })?;
        }
        add_nested_archive_entry(
            root,
            NestedArchiveEntryContext {
                outer_archive_path,
                outer_archive_format,
                archive_member_path,
                nested_archive_format,
            },
            NestedArchiveEntryInfo {
                raw_name: &raw_name,
                is_directory,
                size: Some(size),
            },
            error_count,
        );
    }
    Ok(())
}

/// 扫描内存中的 7Z 内层压缩包。
fn scan_nested_7z_archive(
    archive_bytes: &[u8],
    root: &mut ArchiveScanNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    error_count: &mut usize,
) -> Result<(), ArchiveScanError> {
    let reader = sevenz_rust::SevenZReader::new(
        Cursor::new(archive_bytes),
        archive_bytes.len() as u64,
        sevenz_rust::Password::empty(),
    )
    .map_err(|error| ArchiveScanError::new(format!("无法读取嵌套 7Z 目录：{}", error)))?;
    for entry in &reader.archive().files {
        add_nested_archive_entry(
            root,
            NestedArchiveEntryContext {
                outer_archive_path,
                outer_archive_format,
                archive_member_path,
                nested_archive_format,
            },
            NestedArchiveEntryInfo {
                raw_name: entry.name(),
                is_directory: entry.is_directory(),
                size: Some(entry.size),
            },
            error_count,
        );
    }
    Ok(())
}

/// 把已扫描出的内层压缩包压缩包扫描树挂到外层压缩包压缩包扫描树中。
fn add_nested_archive_tree(
    root: &mut ArchiveScanNode,
    archive_member_path: &str,
    nested_tree: ArchiveScanNode,
    error_count: &mut usize,
) {
    let segments = match split_archive_entry_path(archive_member_path) {
        Ok(segments) => segments,
        Err(reason) => {
            *error_count += 1;
            root.add_archive_error_entry(archive_member_path, "非法路径", reason);
            return;
        }
    };
    root.add_subtree_path(&segments, nested_tree);
}

/// 把压缩包条目添加到压缩包扫描树。
///
/// 业务意图：
/// - 所有格式都必须经过同一个路径安全检查，避免不同压缩包实现出现展示边界差异。
/// - 条目目录会自动补齐隐式父目录，使压缩包内没有显式目录项时仍能形成压缩包扫描树。
///
/// 边界条件：
/// - 空路径、绝对路径、盘符路径和包含 `..` 的路径会转成错误节点。
/// - 目录条目不展示大小；文件条目展示未压缩大小。
fn add_archive_entry(
    root: &mut ArchiveScanNode,
    archive_path: &Path,
    archive_format: ArchiveFormat,
    raw_name: &str,
    is_directory: bool,
    size: Option<u64>,
    error_count: &mut usize,
) {
    let segments = match split_archive_entry_path(raw_name) {
        Ok(segments) => segments,
        Err(reason) => {
            *error_count += 1;
            root.add_archive_error_entry(raw_name, "非法路径", reason);
            return;
        }
    };

    let kind = if is_directory {
        ArchiveScanEntryKind::Directory
    } else {
        ArchiveScanEntryKind::File
    };
    let meta = if is_directory {
        None
    } else {
        size.map(format_byte_size)
    };

    let source = if is_directory {
        None
    } else {
        Some(ArchiveMemberSource::Direct {
            archive_path: archive_path.to_path_buf(),
            archive_format,
            member_path: join_archive_segments(&segments),
        })
    };

    root.add_leaf_path(&segments, kind, meta, source, None);
}

/// 把内层压缩包条目添加到内层压缩包扫描树。
///
/// 业务意图：
/// - 内层压缩包被当作目录显示，但其子文件仍需要保留“外层压缩包 + 内层压缩包成员 + 内层文件成员”的完整读取来源。
/// - 这里同时构建树节点和完整读取来源，外层上下文和条目元数据分别通过参数对象传入，避免位置参数误传。
struct NestedArchiveEntryContext<'a> {
    /// 外层压缩包路径。
    outer_archive_path: &'a Path,
    /// 外层压缩包格式。
    outer_archive_format: ArchiveFormat,
    /// 外层压缩包中的内层压缩包成员路径。
    archive_member_path: &'a str,
    /// 内层压缩包格式。
    nested_archive_format: ArchiveFormat,
}

/// 内层压缩包条目的轻量元数据。
struct NestedArchiveEntryInfo<'a> {
    /// 内层条目原始路径。
    raw_name: &'a str,
    /// 是否目录条目。
    is_directory: bool,
    /// 条目未压缩大小。
    size: Option<u64>,
}

/// 把内层压缩包条目添加到内层压缩包扫描树。
fn add_nested_archive_entry(
    root: &mut ArchiveScanNode,
    context: NestedArchiveEntryContext<'_>,
    entry: NestedArchiveEntryInfo<'_>,
    error_count: &mut usize,
) {
    let NestedArchiveEntryContext {
        outer_archive_path,
        outer_archive_format,
        archive_member_path,
        nested_archive_format,
    } = context;
    let NestedArchiveEntryInfo {
        raw_name,
        is_directory,
        size,
    } = entry;

    let segments = match split_archive_entry_path(raw_name) {
        Ok(segments) => segments,
        Err(reason) => {
            *error_count += 1;
            root.add_archive_error_entry(raw_name, "非法路径", reason);
            return;
        }
    };

    let kind = if is_directory {
        ArchiveScanEntryKind::Directory
    } else {
        ArchiveScanEntryKind::File
    };
    let meta = if is_directory {
        None
    } else {
        size.map(format_byte_size)
    };
    let source = if is_directory {
        None
    } else {
        Some(ArchiveMemberSource::Nested {
            outer_archive_path: outer_archive_path.to_path_buf(),
            outer_archive_format,
            archive_member_path: archive_member_path.to_string(),
            nested_archive_format,
            nested_member_path: join_archive_segments(&segments),
        })
    };

    root.add_leaf_path(&segments, kind, meta, source, None);
}

/// 将字节数格式化成适合目录树元信息展示的短文本。
///
/// 业务意图：
/// - 压缩包扫描阶段需要保留与日志目录树一致的文件大小展示文本，但不能依赖加载器私有函数。
fn format_byte_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let bytes_f64 = bytes as f64;
    if bytes_f64 >= GB {
        format!("{:.0} GB", bytes_f64 / GB)
    } else if bytes_f64 >= MB {
        format!("{:.0} MB", bytes_f64 / MB)
    } else if bytes_f64 >= KB {
        format!("{:.0} KB", bytes_f64 / KB)
    } else {
        format!("{} B", bytes)
    }
}
