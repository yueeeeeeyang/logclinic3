//! 日志来源加载与目录树构建模块。
//!
//! 业务意图：
//! - 该模块只负责把用户选择的文件、目录或压缩包转换成左侧目录树需要的轻量结构。
//! - 当前阶段不读取日志正文、不做编码识别、不做搜索索引，也不把压缩包内容解压到磁盘。
//! - UI 层只消费 `LoadedLogTree`，避免 GPUI 渲染代码直接依赖文件系统和压缩包格式细节。
//! - 加载层会为每个节点生成当前树内稳定 ID 和子节点标记，供 UI 实现展开、收起和虚拟列表渲染。
//!
//! 关键约束：
//! - 目录扫描必须完整递归，但不跟随符号链接，避免跨目录边界读取用户未明确选择的位置。
//! - 压缩包只读取目录项元数据，不能落盘解压，避免写入临时目录带来权限、清理和安全边界问题。
//! - 压缩包内部路径必须做安全归一化，绝对路径、盘符路径和 `..` 路径即使不落盘也不能作为正常树节点展示。

use std::{
    error::Error,
    fmt::{self, Display},
    fs::{self, File},
    io::BufReader,
    path::{Component, Path, PathBuf},
};

use flate2::read::GzDecoder;
use tar::Archive as TarArchive;
use walkdir::WalkDir;
use zip::ZipArchive;

/// 加载完成后提供给左侧目录树渲染的稳定数据结构。
///
/// 业务意图：
/// - `rows` 已经是按展示顺序扁平化后的树节点，UI 层无需再递归遍历文件系统或压缩包。
/// - `summary` 用于标题右侧展示加载结果摘要，避免 UI 层重复计算节点和错误数量。
///
/// 边界条件：
/// - 当前结构不保存真实文件句柄，也不缓存压缩包解码器；后续点击节点读取正文时必须重新按来源规则打开。
/// - 展开状态属于 UI 会话状态，不写入该加载结果，避免业务数据和交互状态耦合。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedLogTree {
    /// 左侧树的标题补充信息。
    ///
    /// 业务意图：
    /// - 多个来源加载时使用统一摘要，让用户知道当前树来自真实选择而不是示例数据。
    /// - 摘要只表达节点规模和错误数量，不承诺日志条数或文件编码状态。
    pub summary: String,

    /// 按展示顺序扁平化后的目录树行。
    ///
    /// 边界条件：
    /// - 行内只包含展示所需的名称、层级、类型和元信息，不包含完整正文。
    /// - 后续如需点击读取，应新增稳定来源 ID，而不是让 UI 反向解析展示文本。
    pub rows: Vec<LogTreeRow>,

    /// 加载过程中收集到的非致命错误数量。
    ///
    /// 业务意图：
    /// - 权限失败、坏压缩包条目或不支持的特殊路径不应中断其它可读取节点。
    /// - UI 可以通过该字段决定是否展示额外的错误提示或诊断入口。
    pub error_count: usize,
}

/// 左侧目录树的一行真实加载节点。
///
/// 业务意图：
/// - 用统一结构表达普通文件、目录、压缩包、符号链接和错误节点，降低 UI 渲染分支复杂度。
/// - `id` 和 `has_children` 让 UI 可以只维护展开集合，而不用重新推导树结构或解析缩进层级。
/// - `depth` 由加载层计算，UI 层只根据层级缩进，不需要知道真实路径父子关系。
///
/// 边界条件：
/// - `id` 只在一次加载结果内部稳定，不跨加载、不跨进程持久化，后续不能把它当作文件来源 ID 使用。
/// - 当前节点不可选、不保存持久来源定位；后续点击读取正文需要先定义来源定位规则。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogTreeRow {
    /// 当前加载结果内部的稳定节点 ID。
    ///
    /// 业务意图：
    /// - UI 需要用稳定键维护展开/收起状态，不能使用行号，因为虚拟列表和折叠会改变可见行位置。
    /// - ID 由加载阶段按前序遍历分配，保证同一次加载中父节点、子节点和可见缓存都能引用同一节点。
    ///
    /// 边界条件：
    /// - ID 只对当前 `LoadedLogTree` 有意义；重新加载日志后即使路径相同也会重新分配。
    pub id: usize,

    /// 节点在目录树中的层级深度。
    ///
    /// 边界条件：
    /// - 根节点深度为 0，子节点逐级递增。
    /// - 深度只服务于当前扁平展示，不代表未来持久化模型。
    pub depth: usize,

    /// 节点显示名称。
    ///
    /// 业务意图：
    /// - 目录扫描使用文件名，压缩包内部使用安全归一化后的路径片段。
    /// - 发生错误时也会提供可读名称，帮助用户定位问题来源。
    pub label: String,

    /// 节点类型。
    ///
    /// 业务意图：
    /// - UI 用该字段选择图标和颜色，不应从文件名扩展名推断显示类型。
    pub kind: LogTreeEntryKind,

    /// 当前节点是否拥有子节点。
    ///
    /// 业务意图：
    /// - UI 根据该字段决定是否显示展开箭头，以及点击时是否切换展开状态。
    /// - 使用加载层计算结果比 UI 根据后续行深度推断更可靠，也减少虚拟列表滚动时的重复计算。
    pub has_children: bool,

    /// 节点右侧补充信息。
    ///
    /// 边界条件：
    /// - 文件节点通常展示大小；目录节点可以为空；错误节点展示简短错误类别。
    /// - 这里不展示绝对路径，避免窄面板被长路径挤压。
    pub meta: Option<String>,

    /// 错误节点或降级节点的详细说明。
    ///
    /// 业务意图：
    /// - 当前 UI 只展示简短元信息，后续可以把该字段接入悬浮提示或状态面板。
    /// - 非错误节点通常为 `None`。
    pub error_message: Option<String>,
}

/// 目录树节点的业务类型。
///
/// 业务意图：
/// - 类型枚举让 UI 渲染保持稳定，不依赖扩展名、颜色或错误文案等易变展示细节。
/// - 压缩包作为独立类型展示，方便用户区分真实目录和归档文件内部结构。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogTreeEntryKind {
    /// 普通目录节点。
    Directory,
    /// 普通文件节点。
    File,
    /// 压缩包根节点。
    Archive,
    /// 符号链接节点。
    Symlink,
    /// 加载、权限或路径安全错误节点。
    Error,
}

/// 当前阶段支持识别的压缩包格式。
///
/// 业务意图：
/// - 用户明确要求支持 zip、rar、tar.gz 和 7z。
/// - 将格式识别集中在枚举上，避免文件扫描、UI 展示和测试各自硬编码扩展名。
///
/// 边界条件：
/// - 这里只基于文件名扩展名判断格式，不读取魔数；后续如需更强识别能力需要补充验收标准。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveFormat {
    /// ZIP 压缩包。
    Zip,
    /// RAR 压缩包。
    Rar,
    /// gzip 压缩的 tar 归档，包含 `.tar.gz` 和 `.tgz`。
    TarGz,
    /// 7-Zip 压缩包。
    SevenZ,
}

impl ArchiveFormat {
    /// 根据路径文件名识别当前支持的压缩包格式。
    ///
    /// 业务意图：
    /// - 统一处理大小写扩展名和 `.tar.gz` 这种复合扩展名。
    /// - 普通日志文件返回 `None`，由加载层按单文件节点处理。
    ///
    /// 边界条件：
    /// - 文件名无法转为字符串时采用有损转换，仅用于扩展名判断，不影响真实路径打开。
    /// - 当前不把 `.tar` 视为支持格式，因为用户本轮明确要求的是 `.tar.gz`。
    pub fn from_path(path: &Path) -> Option<Self> {
        let file_name = path.file_name()?.to_string_lossy().to_ascii_lowercase();

        if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
            return Some(Self::TarGz);
        }

        if file_name.ends_with(".zip") {
            return Some(Self::Zip);
        }

        if file_name.ends_with(".rar") {
            return Some(Self::Rar);
        }

        if file_name.ends_with(".7z") {
            return Some(Self::SevenZ);
        }

        None
    }

    /// 返回适合展示在目录树元信息中的格式名称。
    ///
    /// 边界条件：
    /// - 文案只用于当前中文界面，不作为序列化或匹配逻辑使用。
    pub fn label(self) -> &'static str {
        match self {
            Self::Zip => "ZIP",
            Self::Rar => "RAR",
            Self::TarGz => "TAR.GZ",
            Self::SevenZ => "7Z",
        }
    }
}

/// 日志加载阶段的致命错误。
///
/// 业务意图：
/// - 大多数单个节点错误会被转为目录树错误节点并继续加载。
/// - 只有整个加载流程无法形成结果时才返回该错误，例如应用层传入空路径以外的异常状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogLoadError {
    /// 面向开发者和 UI 的中文错误说明。
    message: String,
}

impl LogLoadError {
    /// 创建一个新的加载错误。
    ///
    /// 边界条件：
    /// - 该错误类型当前只保存文本，不包装底层错误对象，避免在跨线程 UI 更新时引入复杂生命周期。
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for LogLoadError {
    /// 将加载错误格式化为中文可读文本。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for LogLoadError {}

/// 加载用户选择的一个或多个日志来源。
///
/// 业务意图：
/// - 统一处理文件、目录和压缩包，生成左侧目录树可直接渲染的扁平行。
/// - 多个来源使用虚拟根节点包裹，避免不同来源的同名根目录混在一起。
///
/// 关键约束：
/// - 目录完整递归扫描，但不跟随符号链接。
/// - 压缩包只读取条目元数据，不实际解压。
/// - 单个来源失败会变成错误节点，其它来源继续加载。
///
/// 边界条件：
/// - 空选择通常来自用户取消对话框；这里返回空结果而不是错误，方便 UI 保持当前状态。
/// - 当前不限制节点数量，极大目录会完整扫描；后续如需上限必须由业务规则明确。
pub fn load_log_sources(paths: Vec<PathBuf>) -> Result<LoadedLogTree, LogLoadError> {
    if paths.is_empty() {
        return Ok(LoadedLogTree {
            summary: "未选择".to_string(),
            rows: Vec::new(),
            error_count: 0,
        });
    }

    let mut error_count = 0usize;
    let mut root = if paths.len() == 1 {
        load_single_source(&paths[0], &mut error_count)
    } else {
        let mut virtual_root = TreeNode::new(
            format!("已加载 {} 个来源", paths.len()),
            LogTreeEntryKind::Directory,
        );

        for path in &paths {
            virtual_root
                .children
                .push(load_single_source(path, &mut error_count));
        }

        virtual_root
    };

    root.sort_recursively();

    let mut rows = Vec::new();
    let mut next_row_id = 0usize;
    root.flatten_into(0, &mut next_row_id, &mut rows);

    let summary = if error_count == 0 {
        format!("{} 个节点", rows.len())
    } else {
        format!("{} 个节点，{} 个错误", rows.len(), error_count)
    };

    Ok(LoadedLogTree {
        summary,
        rows,
        error_count,
    })
}

/// 加载单个用户选择的来源路径。
///
/// 业务意图：
/// - 按文件系统元数据区分目录、普通文件和压缩包。
/// - 单来源失败时仍返回一个错误节点，让左侧树能明确告诉用户哪个来源不可用。
///
/// 边界条件：
/// - 符号链接来源会作为符号链接节点展示，不跟随目标。
/// - `symlink_metadata` 失败通常代表路径不存在或权限不足，此时转为错误节点。
fn load_single_source(path: &Path, error_count: &mut usize) -> TreeNode {
    let label = display_name_for_path(path);

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            *error_count += 1;
            return TreeNode::error(label, "读取失败", format!("无法读取来源元数据：{}", error));
        }
    };

    if metadata.file_type().is_symlink() {
        return TreeNode {
            label,
            kind: LogTreeEntryKind::Symlink,
            meta: Some("符号链接".to_string()),
            error_message: None,
            children: Vec::new(),
        };
    }

    if metadata.is_dir() {
        let mut root = TreeNode::new(label, LogTreeEntryKind::Directory);
        scan_directory(path, &mut root, error_count);
        return root;
    }

    if metadata.is_file() {
        if let Some(format) = ArchiveFormat::from_path(path) {
            let mut root = TreeNode {
                label,
                kind: LogTreeEntryKind::Archive,
                meta: Some(format.label().to_string()),
                error_message: None,
                children: Vec::new(),
            };

            if let Err(error) = scan_archive(path, format, &mut root, error_count) {
                *error_count += 1;
                root.children.push(TreeNode::error(
                    "压缩包读取失败",
                    "读取失败",
                    error.to_string(),
                ));
            }

            return root;
        }

        return TreeNode {
            label,
            kind: LogTreeEntryKind::File,
            meta: Some(format_byte_size(metadata.len())),
            error_message: None,
            children: Vec::new(),
        };
    }

    *error_count += 1;
    TreeNode::error(label, "不支持", "当前来源不是普通文件、目录或符号链接")
}

/// 扫描普通目录并把结果写入树节点。
///
/// 业务意图：
/// - 用户选择目录后，需要完整展示目录内部文件结构，作为后续选择日志文件的基础。
/// - 使用 `walkdir` 保持跨平台递归行为一致，并显式关闭符号链接跟随。
///
/// 关键约束：
/// - 不跟随符号链接，避免扫描越过用户选择的目录边界。
/// - 遇到权限错误或读取失败时添加错误节点并继续处理其它可访问路径。
///
/// 边界条件：
/// - 当前不跳过隐藏文件、不按扩展名过滤，也不限制深度或节点数量。
fn scan_directory(root_path: &Path, root: &mut TreeNode, error_count: &mut usize) {
    for entry in WalkDir::new(root_path)
        .follow_links(false)
        .into_iter()
        .skip(1)
    {
        match entry {
            Ok(entry) => {
                let relative_path = match entry.path().strip_prefix(root_path) {
                    Ok(path) => path,
                    Err(error) => {
                        *error_count += 1;
                        root.add_error_child(
                            "路径归属异常",
                            "路径错误",
                            format!("目录项不在所选根目录下：{}", error),
                        );
                        continue;
                    }
                };

                let segments = path_components_for_tree(relative_path);
                if segments.is_empty() {
                    continue;
                }

                let file_type = entry.file_type();
                if file_type.is_dir() {
                    root.add_leaf_path(&segments, LogTreeEntryKind::Directory, None, None);
                } else if file_type.is_file() {
                    let size = entry.metadata().ok().map(|metadata| metadata.len());
                    root.add_leaf_path(
                        &segments,
                        LogTreeEntryKind::File,
                        size.map(format_byte_size),
                        None,
                    );
                } else if file_type.is_symlink() {
                    root.add_leaf_path(
                        &segments,
                        LogTreeEntryKind::Symlink,
                        Some("符号链接".to_string()),
                        None,
                    );
                } else {
                    *error_count += 1;
                    root.add_leaf_path(
                        &segments,
                        LogTreeEntryKind::Error,
                        Some("不支持".to_string()),
                        Some("当前目录项不是普通文件、目录或符号链接".to_string()),
                    );
                }
            }
            Err(error) => {
                *error_count += 1;
                let label = error
                    .path()
                    .and_then(Path::file_name)
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "目录项读取失败".to_string());
                root.add_error_child(label, "读取失败", error.to_string());
            }
        }
    }
}

/// 按压缩包格式分发扫描逻辑。
///
/// 业务意图：
/// - 格式相关 API 差异集中在本函数附近，调用方只关心压缩包根节点和错误处理。
/// - 每一种格式都只读取条目列表，不把文件内容解压到磁盘。
fn scan_archive(
    path: &Path,
    format: ArchiveFormat,
    root: &mut TreeNode,
    error_count: &mut usize,
) -> Result<(), LogLoadError> {
    match format {
        ArchiveFormat::Zip => scan_zip_archive(path, root, error_count),
        ArchiveFormat::Rar => scan_rar_archive(path, root, error_count),
        ArchiveFormat::TarGz => scan_tar_gz_archive(path, root, error_count),
        ArchiveFormat::SevenZ => scan_7z_archive(path, root, error_count),
    }
}

/// 扫描 ZIP 压缩包目录项。
///
/// 业务意图：
/// - `zip` crate 支持按索引读取条目元数据，适合当前只构建目录树的需求。
/// - 读取条目名称、目录标记和未压缩大小即可，不读取正文内容。
///
/// 边界条件：
/// - ZIP 条目名可能包含不安全路径，必须交给 `add_archive_entry` 统一校验。
fn scan_zip_archive(
    path: &Path,
    root: &mut TreeNode,
    error_count: &mut usize,
) -> Result<(), LogLoadError> {
    let file = File::open(path).map_err(|error| {
        LogLoadError::new(format!("无法打开 ZIP 压缩包 {}：{}", path.display(), error))
    })?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|error| LogLoadError::new(format!("无法读取 ZIP 目录：{}", error)))?;

    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            LogLoadError::new(format!("无法读取 ZIP 条目 {}：{}", index, error))
        })?;
        add_archive_entry(
            root,
            entry.name(),
            entry.is_dir(),
            Some(entry.size()),
            error_count,
        );
    }

    Ok(())
}

/// 扫描 RAR 压缩包目录项。
///
/// 业务意图：
/// - `unrar` crate 封装 RARLAB unrar 库的列表能力，可以在不解压文件的前提下读取条目元数据。
/// - 当前只使用列表模式，不调用任何写入磁盘的解压接口。
///
/// 边界条件：
/// - 加密文件没有密码规则，当前以错误节点展示，后续需要用户确认密码输入和缓存策略后再支持。
/// - `unrar` 依赖底层 unrar 实现，跨平台构建如遇工具链问题需要在对应平台单独验收。
fn scan_rar_archive(
    path: &Path,
    root: &mut TreeNode,
    error_count: &mut usize,
) -> Result<(), LogLoadError> {
    let archive = unrar::Archive::new(path)
        .open_for_listing()
        .map_err(|error| LogLoadError::new(format!("无法打开 RAR 目录：{}", error)))?;

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

        add_archive_entry(
            root,
            &raw_name,
            entry.is_directory(),
            Some(entry.unpacked_size),
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
    root: &mut TreeNode,
    error_count: &mut usize,
) -> Result<(), LogLoadError> {
    let file = File::open(path).map_err(|error| {
        LogLoadError::new(format!(
            "无法打开 TAR.GZ 压缩包 {}：{}",
            path.display(),
            error
        ))
    })?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let mut archive = TarArchive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| LogLoadError::new(format!("无法读取 TAR.GZ 目录：{}", error)))?;

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                *error_count += 1;
                root.add_error_child("TAR.GZ 条目读取失败", "读取失败", error.to_string());
                continue;
            }
        };

        let path = match entry.path() {
            Ok(path) => path,
            Err(error) => {
                *error_count += 1;
                root.add_error_child("TAR.GZ 路径读取失败", "路径错误", error.to_string());
                continue;
            }
        };
        let raw_name = path.to_string_lossy();
        let is_directory = entry.header().entry_type().is_dir();
        let size = entry.size();

        add_archive_entry(root, &raw_name, is_directory, Some(size), error_count);
    }

    Ok(())
}

/// 扫描 7z 压缩包目录项。
///
/// 业务意图：
/// - `sevenz-rust` 能读取 7z 文件头部中的文件列表，符合当前只构建目录树、不解压的需求。
/// - 该函数只访问归档元数据，不写入临时目录。
///
/// 边界条件：
/// - 加密 7z 或损坏头部会返回读取错误，当前展示为压缩包根节点下的错误节点。
fn scan_7z_archive(
    path: &Path,
    root: &mut TreeNode,
    error_count: &mut usize,
) -> Result<(), LogLoadError> {
    let archive = sevenz_rust::Archive::open(path)
        .map_err(|error| LogLoadError::new(format!("无法读取 7Z 目录：{}", error)))?;

    for entry in archive.files {
        add_archive_entry(
            root,
            &entry.name,
            entry.is_directory(),
            Some(entry.size()),
            error_count,
        );
    }

    Ok(())
}

/// 把压缩包条目添加到目录树。
///
/// 业务意图：
/// - 所有格式都必须经过同一个路径安全检查，避免不同压缩包实现出现展示边界差异。
/// - 条目目录会自动补齐隐式父目录，使压缩包内没有显式目录项时仍能形成目录树。
///
/// 边界条件：
/// - 空路径、绝对路径、盘符路径和包含 `..` 的路径会转成错误节点。
/// - 目录条目不展示大小；文件条目展示未压缩大小。
fn add_archive_entry(
    root: &mut TreeNode,
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
        LogTreeEntryKind::Directory
    } else {
        LogTreeEntryKind::File
    };
    let meta = if is_directory {
        None
    } else {
        size.map(format_byte_size)
    };

    root.add_leaf_path(&segments, kind, meta, None);
}

/// 将普通文件系统相对路径拆成目录树片段。
///
/// 业务意图：
/// - 目录扫描已经由操作系统解析真实路径，这里只做展示片段转换。
/// - 使用 `Component` 可以自然忽略当前目录片段，并避免把根前缀错误显示为节点。
///
/// 边界条件：
/// - 普通文件系统路径来自 `strip_prefix` 后的相对路径，理论上不应包含根目录或盘符前缀。
fn path_components_for_tree(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// 将压缩包内部路径拆成安全的目录树片段。
///
/// 业务意图：
/// - 压缩包路径是外部输入，即使当前不解压，也不能把危险路径伪装成正常目录树。
/// - 统一将反斜杠归一化为斜杠，兼容 Windows 上创建的压缩包。
///
/// 安全边界：
/// - 拒绝绝对路径，避免 `/var/log/a.log` 这类条目看起来像真实本机路径。
/// - 拒绝盘符路径，避免 `C:\logs\a.log` 在 Windows 语境下产生歧义。
/// - 拒绝 `..`，避免后续接入读取或解压能力时遗留路径穿越风险。
/// - 拒绝空片段和 NUL 字符，避免展示异常或底层 API 解析差异。
fn split_archive_entry_path(raw_name: &str) -> Result<Vec<String>, String> {
    let normalized = raw_name.replace('\\', "/");
    let trimmed = normalized.trim_matches('/');

    if normalized.starts_with('/') {
        return Err("压缩包条目是绝对路径".to_string());
    }

    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        return Err("压缩包条目包含 Windows 盘符路径".to_string());
    }

    if trimmed.is_empty() {
        return Err("压缩包条目路径为空".to_string());
    }

    let mut segments = Vec::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }

        if segment == ".." {
            return Err("压缩包条目包含上级目录片段".to_string());
        }

        if segment.contains('\0') {
            return Err("压缩包条目包含非法 NUL 字符".to_string());
        }

        segments.push(segment.to_string());
    }

    if segments.is_empty() {
        return Err("压缩包条目路径为空".to_string());
    }

    Ok(segments)
}

/// 为本地路径生成目录树根节点显示名。
///
/// 业务意图：
/// - 优先展示文件名或目录名，让左侧树在默认 300px 宽度下更易扫描。
/// - 如果路径没有文件名，则回退到完整路径字符串，保证错误节点仍可定位来源。
fn display_name_for_path(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

/// 将字节数格式化成适合目录树元信息展示的短文本。
///
/// 业务意图：
/// - 目录树右侧空间有限，需要使用紧凑单位表达文件大小。
/// - 该格式只用于展示，不用于排序或精确计算。
///
/// 边界条件：
/// - 小于 1KB 使用字节；KB 以上保留整数，避免过长小数影响窄面板布局。
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

/// 构建目录树时使用的内部节点。
///
/// 业务意图：
/// - 加载阶段需要递归插入和排序，直接使用扁平行会导致隐式目录补齐逻辑复杂。
/// - 内部节点允许先构建树，再统一排序和扁平化给 UI。
///
/// 边界条件：
/// - 该结构不暴露给 UI，后续可以替换为更高效的 arena 或索引结构。
#[derive(Clone, Debug, PartialEq, Eq)]
struct TreeNode {
    /// 节点显示名称。
    label: String,
    /// 节点类型。
    kind: LogTreeEntryKind,
    /// 右侧元信息。
    meta: Option<String>,
    /// 错误详细信息。
    error_message: Option<String>,
    /// 子节点列表。
    children: Vec<TreeNode>,
}

impl TreeNode {
    /// 创建一个普通内部节点。
    ///
    /// 边界条件：
    /// - 新节点默认没有元信息、错误信息和子节点，调用方可按来源类型继续补充。
    fn new(label: impl Into<String>, kind: LogTreeEntryKind) -> Self {
        Self {
            label: label.into(),
            kind,
            meta: None,
            error_message: None,
            children: Vec::new(),
        }
    }

    /// 创建一个错误节点。
    ///
    /// 业务意图：
    /// - 用树节点承载错误，而不是只返回全局错误，保证用户能看到具体失败位置。
    fn error(
        label: impl Into<String>,
        meta: impl Into<String>,
        error_message: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            kind: LogTreeEntryKind::Error,
            meta: Some(meta.into()),
            error_message: Some(error_message.into()),
            children: Vec::new(),
        }
    }

    /// 添加一个普通错误子节点。
    ///
    /// 业务意图：
    /// - 目录扫描或压缩包扫描遇到局部失败时，用该方法把错误放在当前来源根节点下。
    fn add_error_child(
        &mut self,
        label: impl Into<String>,
        meta: impl Into<String>,
        error_message: impl Into<String>,
    ) {
        self.children
            .push(Self::error(label.into(), meta.into(), error_message.into()));
    }

    /// 添加一个与压缩包内部路径相关的错误节点。
    ///
    /// 业务意图：
    /// - 非法路径或加密条目仍应显示原始条目名，帮助用户判断压缩包内容问题。
    /// - 错误节点放在压缩包根下，不参与正常路径层级构建。
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

    /// 根据路径片段添加叶子节点，并自动补齐中间目录。
    ///
    /// 业务意图：
    /// - 目录扫描和压缩包扫描都可能先看到文件路径，父目录节点需要自动出现。
    /// - 同名同类型节点会复用，避免压缩包显式目录项和文件路径隐式目录产生重复目录。
    ///
    /// 边界条件：
    /// - 如果同一目录下同时存在同名文件和目录，当前会保留两个不同类型节点。
    /// - 重复文件路径会更新已有节点的元信息，避免目录树出现完全相同的重复行。
    fn add_leaf_path(
        &mut self,
        segments: &[String],
        kind: LogTreeEntryKind,
        meta: Option<String>,
        error_message: Option<String>,
    ) {
        if segments.is_empty() {
            return;
        }

        let mut current = self;
        for segment in &segments[..segments.len() - 1] {
            current = current.get_or_insert_child(segment, LogTreeEntryKind::Directory);
        }

        let leaf_label = &segments[segments.len() - 1];
        let leaf = current.get_or_insert_child(leaf_label, kind);
        leaf.meta = meta;
        leaf.error_message = error_message;
    }

    /// 查找或插入一个同名同类型子节点。
    ///
    /// 业务意图：
    /// - 复用目录节点可以自然合并多条路径的公共前缀。
    /// - 文件和目录同名时不合并，避免错误隐藏真实结构冲突。
    fn get_or_insert_child(&mut self, label: &str, kind: LogTreeEntryKind) -> &mut TreeNode {
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

    /// 对当前节点及其所有子节点排序。
    ///
    /// 业务意图：
    /// - 目录优先、文件其次、错误最后，符合日志目录扫描习惯。
    /// - 名称使用大小写不敏感排序，减少跨平台文件系统大小写差异带来的视觉跳动。
    fn sort_recursively(&mut self) {
        for child in &mut self.children {
            child.sort_recursively();
        }

        self.children.sort_by(|left, right| {
            let left_key = (sort_rank(left.kind), left.label.to_ascii_lowercase());
            let right_key = (sort_rank(right.kind), right.label.to_ascii_lowercase());
            left_key.cmp(&right_key)
        });
    }

    /// 将递归树节点扁平化为 UI 可直接渲染的行。
    ///
    /// 边界条件：
    /// - 当前函数仍输出完整树，展开/收起只在 UI 的可见行缓存中处理。
    /// - `next_row_id` 只服务于当前加载结果，不能跨加载复用。
    fn flatten_into(&self, depth: usize, next_row_id: &mut usize, rows: &mut Vec<LogTreeRow>) {
        let id = *next_row_id;
        *next_row_id += 1;

        rows.push(LogTreeRow {
            id,
            depth,
            label: self.label.clone(),
            kind: self.kind,
            has_children: !self.children.is_empty(),
            meta: self.meta.clone(),
            error_message: self.error_message.clone(),
        });

        for child in &self.children {
            child.flatten_into(depth + 1, next_row_id, rows);
        }
    }
}

/// 返回目录树排序时使用的类型优先级。
///
/// 业务意图：
/// - 目录和压缩包都可包含子节点，因此排在普通文件前，方便用户先浏览结构。
/// - 错误节点放在最后，避免局部失败打断主要文件结构扫描。
fn sort_rank(kind: LogTreeEntryKind) -> u8 {
    match kind {
        LogTreeEntryKind::Directory => 0,
        LogTreeEntryKind::Archive => 1,
        LogTreeEntryKind::File => 2,
        LogTreeEntryKind::Symlink => 3,
        LogTreeEntryKind::Error => 4,
    }
}

#[cfg(test)]
mod tests {
    //! 加载模块的单元测试。
    //!
    //! 业务意图：
    //! - 这些测试覆盖不依赖图形环境的纯业务规则，尤其是压缩包扩展名识别、路径安全和树排序。
    //! - UI 交互由后续桌面验收或自动化截图覆盖，本模块只验证数据结构构建规则。

    use super::*;
    use std::io;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 验证复合压缩包扩展名和大小写扩展名都能识别。
    #[test]
    fn 识别支持的压缩包扩展名() {
        assert_eq!(
            ArchiveFormat::from_path(Path::new("logs.tar.gz")),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("logs.TGZ")),
            Some(ArchiveFormat::TarGz)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("logs.zip")),
            Some(ArchiveFormat::Zip)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("logs.RAR")),
            Some(ArchiveFormat::Rar)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("logs.7z")),
            Some(ArchiveFormat::SevenZ)
        );
        assert_eq!(ArchiveFormat::from_path(Path::new("plain.log")), None);
    }

    /// 验证压缩包路径安全检查会拒绝危险路径。
    #[test]
    fn 压缩包路径安全检查拒绝危险路径() {
        assert!(split_archive_entry_path("/etc/passwd").is_err());
        assert!(split_archive_entry_path("C:\\logs\\a.log").is_err());
        assert!(split_archive_entry_path("../a.log").is_err());
        assert!(split_archive_entry_path("a/../../b.log").is_err());
        assert!(split_archive_entry_path("").is_err());
        assert!(split_archive_entry_path("a/\0/b.log").is_err());
    }

    /// 验证压缩包路径安全检查会归一化 Windows 分隔符。
    #[test]
    fn 压缩包路径安全检查支持反斜杠分隔符() {
        assert_eq!(
            split_archive_entry_path("service\\access.log").unwrap(),
            vec!["service".to_string(), "access.log".to_string()]
        );
    }

    /// 验证树构建会补齐隐式目录并保持目录优先排序。
    #[test]
    fn 树构建会补齐隐式目录并保持目录优先() {
        let mut root = TreeNode::new("root", LogTreeEntryKind::Directory);
        root.add_leaf_path(
            &["b.log".to_string()],
            LogTreeEntryKind::File,
            Some("1 KB".to_string()),
            None,
        );
        root.add_leaf_path(
            &["api".to_string(), "access.log".to_string()],
            LogTreeEntryKind::File,
            Some("2 KB".to_string()),
            None,
        );
        root.sort_recursively();

        let mut rows = Vec::new();
        let mut next_row_id = 0usize;
        root.flatten_into(0, &mut next_row_id, &mut rows);

        assert_eq!(rows[1].label, "api");
        assert_eq!(rows[1].kind, LogTreeEntryKind::Directory);
        assert!(rows[1].has_children);
        assert_eq!(rows[1].id, 1);
        assert_eq!(rows[2].label, "access.log");
        assert!(!rows[2].has_children);
        assert_eq!(rows[3].label, "b.log");
    }

    /// 验证目录扫描不会跟随符号链接。
    ///
    /// 边界条件：
    /// - Windows 创建符号链接通常需要额外权限，因此该测试只在 Unix 平台运行。
    #[cfg(unix)]
    #[test]
    fn 目录扫描不跟随符号链接() -> io::Result<()> {
        use std::os::unix::fs::symlink;

        let temp_root = unique_temp_dir("logclinic3-symlink-test")?;
        let real_dir = temp_root.join("real");
        let selected_dir = temp_root.join("selected");
        fs::create_dir_all(&real_dir)?;
        fs::create_dir_all(&selected_dir)?;
        fs::write(real_dir.join("outside.log"), b"outside")?;
        symlink(&real_dir, selected_dir.join("linked"))?;

        let loaded = load_log_sources(vec![selected_dir.clone()]).unwrap();
        let linked_row = loaded
            .rows
            .iter()
            .find(|row| row.label == "linked")
            .expect("目录树中应该显示符号链接节点");

        assert_eq!(linked_row.kind, LogTreeEntryKind::Symlink);
        assert!(
            loaded.rows.iter().all(|row| row.label != "outside.log"),
            "符号链接目标内部文件不应被扫描进所选目录树"
        );

        fs::remove_dir_all(temp_root)?;
        Ok(())
    }

    /// 创建当前测试专用的临时目录。
    ///
    /// 业务意图：
    /// - 避免引入额外测试依赖，只使用标准库在系统临时目录下创建唯一目录。
    /// - 目录名带纳秒时间戳，降低并发测试冲突概率。
    fn unique_temp_dir(prefix: &str) -> io::Result<PathBuf> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{}-{}", prefix, nanos));
        fs::create_dir_all(&path)?;
        Ok(path)
    }
}
