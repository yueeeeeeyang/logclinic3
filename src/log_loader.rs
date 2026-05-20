//! 日志来源加载与目录树构建模块。
//!
//! 业务意图：
//! - 该模块只负责把用户选择的文件、目录或压缩包转换成左侧目录树需要的轻量结构。
//! - 当前阶段不读取日志正文、不做编码识别、不做搜索索引；但 7Z 会在加载阶段物化普通成员，避免后续点击反复顺序解压。
//! - UI 层只消费 `LoadedLogTree`，避免 GPUI 渲染代码直接依赖文件系统和压缩包格式细节。
//! - 加载层会为每个节点生成当前树内稳定 ID 和子节点标记，供 UI 实现展开、收起和虚拟列表渲染。
//!
//! 关键约束：
//! - 目录扫描必须完整递归，但不跟随符号链接，避免跨目录边界读取用户未明确选择的位置。
//! - ZIP/RAR/TAR.GZ/GZ 只读取目录项元数据；7Z 会写入 session 临时目录，加载结果必须携带清理路径，避免临时磁盘长期累积。
//! - 压缩包内部路径必须做安全归一化，绝对路径、盘符路径和 `..` 路径即使不落盘也不能作为正常树节点展示。

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use walkdir::WalkDir;

use crate::archive::{
    ArchiveFormat, ArchiveMemberSource, ArchiveScanEntryKind, ArchiveScanNode, ArchiveScanProgress,
    ArchiveScanResult, scan_archive_with_progress,
};
use crate::log_source::{LogFileSource, display_name_for_path};

#[path = "log_loader/types.rs"]
mod types;
pub use types::{LoadedLogTree, LogLoadError, LogLoadProgress, LogTreeEntryKind, LogTreeRow};

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
#[cfg(test)]
pub fn load_log_sources(paths: Vec<PathBuf>) -> Result<LoadedLogTree, LogLoadError> {
    load_log_sources_with_progress(paths, |_| {})
}

/// 加载用户选择的一个或多个日志来源，并把来源级进度回传给调用方。
///
/// 业务意图：
/// - GPUI 主线程不能等待目录递归或压缩包扫描完成；调用方可以把进度写入共享快照，由 UI 轮询后在屏幕中央显示。
/// - 进度以用户选择的来源为单位，既能覆盖多文件/多目录加载，也不会要求扫描层预先统计目录总文件数，从而避免额外遍历成本。
///
/// 边界条件：
/// - 空选择返回空树，同时汇报 100% 完成，方便 UI 清理加载态。
/// - 单个来源内部出现权限或压缩包错误时仍继续处理其它来源；进度中的错误数用于用户感知，真实错误节点仍写入目录树。
pub fn load_log_sources_with_progress<F>(
    paths: Vec<PathBuf>,
    mut report_progress: F,
) -> Result<LoadedLogTree, LogLoadError>
where
    F: FnMut(LogLoadProgress),
{
    let mut progress = LogLoadProgress::new(paths.len(), "正在加载日志来源");
    report_progress(progress.clone());

    if paths.is_empty() {
        progress.processed_sources = 0;
        progress.message = "未选择日志来源".to_string();
        report_progress(progress);
        return Ok(LoadedLogTree {
            summary: "未选择".to_string(),
            rows: Vec::new(),
            error_count: 0,
            temporary_paths: Vec::new(),
        });
    }

    let mut error_count = 0usize;
    let mut temporary_paths = Vec::new();
    let mut root = if paths.len() == 1 {
        let source_label = display_name_for_path(&paths[0]);
        progress.current_source = Some(source_label.clone());
        progress.message = format!(
            "正在加载：{}",
            progress.current_source.as_deref().unwrap_or("日志来源")
        );
        report_progress(progress.clone());
        let error_count_before_source = error_count;
        let root = load_single_source(
            &paths[0],
            &mut error_count,
            &mut temporary_paths,
            |archive_progress| {
                apply_archive_progress_to_load_progress(
                    &mut progress,
                    &source_label,
                    error_count_before_source,
                    archive_progress,
                );
                report_progress(progress.clone());
            },
        );
        progress.processed_sources = 1;
        progress.current_source = None;
        clear_current_source_progress(&mut progress);
        progress.discovered_nodes = root.node_count();
        progress.error_count = error_count;
        progress.message = "正在整理目录树".to_string();
        report_progress(progress.clone());
        root
    } else {
        let mut virtual_root = TreeNode::new(
            format!("已加载 {} 个来源", paths.len()),
            LogTreeEntryKind::Directory,
        );

        for path in &paths {
            let source_label = display_name_for_path(path);
            progress.current_source = Some(source_label.clone());
            progress.message = format!(
                "正在加载：{}",
                progress.current_source.as_deref().unwrap_or("日志来源")
            );
            report_progress(progress.clone());
            let error_count_before_source = error_count;
            let source_root = load_single_source(
                path,
                &mut error_count,
                &mut temporary_paths,
                |archive_progress| {
                    apply_archive_progress_to_load_progress(
                        &mut progress,
                        &source_label,
                        error_count_before_source,
                        archive_progress,
                    );
                    report_progress(progress.clone());
                },
            );
            virtual_root.children.push(source_root);
            progress.processed_sources = progress.processed_sources.saturating_add(1);
            progress.current_source = None;
            clear_current_source_progress(&mut progress);
            progress.discovered_nodes = virtual_root.node_count();
            progress.error_count = error_count;
            progress.message = "正在整理目录树".to_string();
            report_progress(progress.clone());
        }

        virtual_root
    };

    root.sort_recursively();

    let mut rows = Vec::new();
    let mut next_row_id = 0usize;
    root.flatten_into(0, &mut next_row_id, &mut rows);
    progress.processed_sources = paths.len();
    progress.current_source = None;
    clear_current_source_progress(&mut progress);
    progress.discovered_nodes = rows.len();
    progress.error_count = error_count;
    progress.message = "加载完成".to_string();
    report_progress(progress);

    let summary = if error_count == 0 {
        format!("{} 个节点", rows.len())
    } else {
        format!("{} 个节点，{} 个错误", rows.len(), error_count)
    };

    Ok(LoadedLogTree {
        summary,
        rows,
        error_count,
        temporary_paths,
    })
}

/// 将压缩包扫描进度合并到日志加载进度快照。
///
/// 业务意图：
/// - 压缩包扫描属于当前顶层来源内部的细分工作；加载器负责把它转换成 UI 已经认识的 `LogLoadProgress`。
/// - 这样普通文件、目录和压缩包都继续走同一个中央进度条，不需要 UI 层理解 ZIP/RAR/TAR/7Z 的差异。
///
/// 边界条件：
/// - 压缩包扫描错误数只代表当前来源内部错误；这里要叠加进入该来源前已有的错误数，避免多来源加载时错误数量回退。
/// - `source_label` 是已经裁剪过的用户可见来源名称，不把绝对路径写入中央浮层。
fn apply_archive_progress_to_load_progress(
    progress: &mut LogLoadProgress,
    source_label: &str,
    error_count_before_source: usize,
    archive_progress: ArchiveScanProgress,
) {
    progress.current_source = Some(source_label.to_string());
    progress.current_source_step = Some(format!(
        "{}：{}",
        archive_progress.format.label(),
        archive_progress.step
    ));
    progress.current_entry = archive_progress.current_entry;
    progress.current_source_work_done = archive_progress.processed_entries;
    progress.current_source_work_total = archive_progress.total_entries;
    progress.discovered_nodes = archive_progress.discovered_nodes;
    progress.error_count = error_count_before_source.saturating_add(archive_progress.error_count);
    progress.message = format!("正在扫描压缩包：{source_label}");
}

/// 清理当前来源内部的细分进度。
///
/// 业务意图：
/// - 一个来源完成后，UI 应回到“整理目录树”或下一个来源的状态，不能继续显示上一个压缩包的成员名。
fn clear_current_source_progress(progress: &mut LogLoadProgress) {
    progress.current_source_step = None;
    progress.current_entry = None;
    progress.current_source_work_done = 0;
    progress.current_source_work_total = None;
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
fn load_single_source(
    path: &Path,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
    mut report_archive_progress: impl FnMut(ArchiveScanProgress),
) -> TreeNode {
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
            source: None,
            children: Vec::new(),
        };
    }

    if metadata.is_dir() {
        let mut root = TreeNode::new(label, LogTreeEntryKind::Directory);
        scan_directory(path, &mut root, error_count);
        return root;
    }

    if metadata.is_file() {
        if let Some(format) = ArchiveFormat::from_file(path) {
            let mut root = TreeNode {
                label,
                kind: LogTreeEntryKind::Archive,
                meta: Some(format.label().to_string()),
                error_message: None,
                source: None,
                children: Vec::new(),
            };

            match scan_archive_with_progress(path, format, |archive_progress| {
                report_archive_progress(archive_progress);
            }) {
                Ok(scan_result) => {
                    append_archive_scan_result(&mut root, scan_result, error_count, temporary_paths)
                }
                Err(error) => {
                    *error_count += 1;
                    root.children.push(TreeNode::error(
                        "压缩包读取失败",
                        "读取失败",
                        error.to_string(),
                    ));
                }
            }

            return root;
        }

        return TreeNode {
            label,
            kind: LogTreeEntryKind::File,
            meta: Some(format_byte_size(metadata.len())),
            error_message: None,
            source: Some(LogFileSource::LocalFile {
                path: path.to_path_buf(),
            }),
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
                    root.add_leaf_path(&segments, LogTreeEntryKind::Directory, None, None, None);
                } else if file_type.is_file() {
                    let size = entry.metadata().ok().map(|metadata| metadata.len());
                    root.add_leaf_path(
                        &segments,
                        LogTreeEntryKind::File,
                        size.map(format_byte_size),
                        Some(LogFileSource::LocalFile {
                            path: entry.path().to_path_buf(),
                        }),
                        None,
                    );
                } else if file_type.is_symlink() {
                    root.add_leaf_path(
                        &segments,
                        LogTreeEntryKind::Symlink,
                        Some("符号链接".to_string()),
                        None,
                        None,
                    );
                } else {
                    *error_count += 1;
                    root.add_leaf_path(
                        &segments,
                        LogTreeEntryKind::Error,
                        Some("不支持".to_string()),
                        None,
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

/// 将压缩包功能域的扫描结果挂载到日志目录树根节点。
///
/// 业务意图：
/// - `archive` 模块只知道压缩包内部结构，不知道左侧目录树的排序、展开 ID 和日志来源统一模型。
/// - 日志加载器在这里完成适配，保持文件系统扫描和压缩包扫描结果最终对 UI 呈现一致。
///
/// 边界条件：
/// - 扫描结果中的临时路径必须合并到 `LoadedLogTree::temporary_paths`，否则 7Z 和嵌套 RAR 扫描产生的临时文件无法被清理。
fn append_archive_scan_result(
    root: &mut TreeNode,
    scan_result: ArchiveScanResult,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) {
    *error_count += scan_result.error_count;
    temporary_paths.extend(scan_result.temporary_paths);
    root.children.extend(
        scan_result
            .children
            .into_iter()
            .map(archive_node_to_tree_node),
    );
}

/// 将压缩包扫描节点转换成日志目录树内部节点。
///
/// 业务意图：
/// - 这里是 `archive` 功能域和日志加载功能域之间的唯一结构适配点。
/// - 转换只复制展示和来源数据，不重新访问磁盘或读取压缩包，避免适配阶段改变扫描语义。
fn archive_node_to_tree_node(node: ArchiveScanNode) -> TreeNode {
    TreeNode {
        label: node.label,
        kind: match node.kind {
            ArchiveScanEntryKind::Directory => LogTreeEntryKind::Directory,
            ArchiveScanEntryKind::File => LogTreeEntryKind::File,
            ArchiveScanEntryKind::Error => LogTreeEntryKind::Error,
        },
        meta: node.meta,
        error_message: node.error_message,
        source: node.source.map(archive_source_to_log_source),
        children: node
            .children
            .into_iter()
            .map(archive_node_to_tree_node)
            .collect(),
    }
}

/// 将压缩包成员来源转换成日志查看器统一来源模型。
///
/// 业务意图：
/// - `LogFileSource` 仍是日志正文读取、搜索、分页和 tab 去重共享的内部模型。
/// - `archive` 模块返回自己的来源描述，避免压缩包扫描代码反向依赖日志树或 UI 模型。
fn archive_source_to_log_source(source: ArchiveMemberSource) -> LogFileSource {
    match source {
        ArchiveMemberSource::Direct {
            archive_path,
            archive_format,
            member_path,
        } => LogFileSource::ArchiveMember {
            archive_path,
            archive_format,
            member_path,
        },
        ArchiveMemberSource::Materialized {
            archive_path,
            archive_format,
            member_path,
            temp_path,
        } => LogFileSource::MaterializedArchiveMember {
            archive_path,
            archive_format,
            member_path,
            temp_path,
        },
        ArchiveMemberSource::Nested {
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_archive_format,
            nested_member_path,
        } => LogFileSource::NestedArchiveMember {
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_archive_format,
            nested_member_path,
        },
    }
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
    /// 文件节点的可打开来源。
    ///
    /// 业务意图：
    /// - 构建树时就保存来源，扁平化后 UI 才能直接打开对应日志正文。
    /// - 中间目录和错误节点没有正文来源，保持为 `None`。
    source: Option<LogFileSource>,
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
            source: None,
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
            source: None,
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
        source: Option<LogFileSource>,
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
        leaf.source = source;
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

    /// 返回当前内部树包含的节点总数。
    ///
    /// 业务意图：
    /// - 加载进度条需要在目录树扁平化前展示“已经发现多少节点”，直接复用内部树结构可以避免额外构造临时 `LogTreeRow`。
    /// - 该统计只用于进度提示，不影响最终 `summary`，最终摘要仍以扁平化后的真实行数为准。
    fn node_count(&self) -> usize {
        1 + self.children.iter().map(Self::node_count).sum::<usize>()
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
            meta: self.display_meta(),
            error_message: self.error_message.clone(),
            source: self.source.clone(),
        });

        for child in &self.children {
            child.flatten_into(depth + 1, next_row_id, rows);
        }
    }

    /// 返回当前节点适合目录树展示的补充信息。
    ///
    /// 业务意图：
    /// - 用户要求文件夹名称后展示文件夹中的文件数量，因此目录节点需要在扁平化时补齐该派生信息。
    /// - 文件数量使用递归统计，能表达该目录下所有可打开日志文件的规模，避免父目录只含子目录时显示为 0。
    ///
    /// 边界条件：
    /// - 压缩包根节点继续显示格式信息，避免 “ZIP/RAR/7Z” 格式提示被文件数覆盖。
    /// - 错误、符号链接和普通文件沿用构建阶段写入的元信息，不额外派生数量。
    fn display_meta(&self) -> Option<String> {
        match self.kind {
            LogTreeEntryKind::Directory => Some(format!("{} 个文件", self.descendant_file_count())),
            LogTreeEntryKind::Archive
            | LogTreeEntryKind::File
            | LogTreeEntryKind::Symlink
            | LogTreeEntryKind::Error => self.meta.clone(),
        }
    }

    /// 递归统计当前节点下可打开文件节点数量。
    ///
    /// 业务意图：
    /// - 目录树中的文件数量用于帮助用户快速判断目录规模，而不是统计目录项总数。
    /// - 只把 `LogTreeEntryKind::File` 计入数量，避免错误节点、符号链接或纯目录影响日志文件规模判断。
    ///
    /// 边界条件：
    /// - 当前目录本身不可能是文件节点时才调用；即使未来复用到其它节点，普通文件也会按 1 个文件处理。
    fn descendant_file_count(&self) -> usize {
        match self.kind {
            LogTreeEntryKind::File => 1,
            LogTreeEntryKind::Directory | LogTreeEntryKind::Archive => {
                self.children.iter().map(Self::descendant_file_count).sum()
            }
            LogTreeEntryKind::Symlink | LogTreeEntryKind::Error => 0,
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
    use crate::archive::{
        is_single_gzip_member_path, normalized_rar_nested_archive_member_path,
        split_archive_entry_path,
    };
    use flate2::{Compression, write::GzEncoder};
    use std::error::Error;
    use std::fs::File;
    use std::io::{self, Cursor, Write};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tar::Builder as TarBuilder;
    use zip::write::SimpleFileOptions;

    /// 验证日志来源加载会汇报可用于中央进度条的来源级进度。
    ///
    /// 业务意图：
    /// - UI 进度条依赖加载层回传的 `LogLoadProgress`，如果未来重构漏掉初始、来源完成或最终进度，加载界面会退化成静态提示。
    /// - 测试使用普通文件，避免依赖平台压缩工具或目录权限差异。
    #[test]
    fn 加载日志来源会回传进度快照() -> Result<(), Box<dyn Error>> {
        let root = unique_temp_dir("log_loader_progress")?;
        fs::create_dir_all(&root)?;
        let log_path = root.join("sample.log");
        fs::write(&log_path, "hello\n")?;

        let mut snapshots = Vec::new();
        let loaded = load_log_sources_with_progress(vec![log_path], |progress| {
            snapshots.push(progress);
        })?;

        assert_eq!(loaded.error_count, 0);
        assert!(snapshots.first().is_some_and(|progress| {
            progress.total_sources == 1 && progress.processed_sources == 0
        }));
        assert!(
            snapshots
                .iter()
                .any(|progress| { progress.current_source.as_deref() == Some("sample.log") })
        );
        assert!(snapshots.last().is_some_and(|progress| {
            progress.processed_sources == 1 && progress.percent() == 100
        }));

        fs::remove_dir_all(root)?;
        Ok(())
    }

    /// 验证 ZIP 压缩包加载会回传压缩包内部的条目级进度。
    ///
    /// 业务意图：
    /// - 用户加载单个压缩包时，顶层来源只有 1 个；如果没有内部条目进度，中央进度条会一直停在 0% 直到加载结束。
    /// - ZIP 可以提前知道目录条目总数，因此进度快照应保留精确的“已扫描 / 总条目”和当前条目名称，UI 再选择只展示条目进度。
    #[test]
    fn 加载_zip_压缩包会回传条目级进度() -> Result<(), Box<dyn Error>> {
        let root = unique_temp_dir("log_loader_zip_progress")?;
        let archive_path = root.join("logs.zip");
        {
            let file = File::create(&archive_path)?;
            let mut writer = zip::ZipWriter::new(file);
            writer.start_file("a.log", SimpleFileOptions::default())?;
            writer.write_all(b"INFO a")?;
            writer.start_file("nested/b.log", SimpleFileOptions::default())?;
            writer.write_all(b"INFO b")?;
            writer.finish()?;
        }

        let mut snapshots = Vec::new();
        let loaded = load_log_sources_with_progress(vec![archive_path], |progress| {
            snapshots.push(progress);
        })?;

        assert_eq!(loaded.error_count, 0);
        assert!(snapshots.iter().any(|progress| {
            progress
                .current_source_step
                .as_deref()
                .is_some_and(|step| step.contains("ZIP"))
                && progress.current_source_work_total == Some(2)
        }));
        assert!(snapshots.iter().any(|progress| {
            progress.current_entry.as_deref() == Some("nested/b.log")
                && progress.current_source_work_done == 2
        }));
        assert!(snapshots.iter().any(|progress| {
            progress.current_source_work_total == Some(2)
                && progress.percent() > 0
                && progress.percent() < 100
        }));

        fs::remove_dir_all(root)?;
        Ok(())
    }

    /// 验证未知总条目数的压缩包进度也会持续推进。
    ///
    /// 业务意图：
    /// - TAR/RAR 这类顺序格式无法在不额外遍历的情况下知道总条目数；加载器仍应基于已扫描条目给出保守估算，避免用户误以为卡死。
    #[test]
    fn 未知总条目数进度会使用保守估算() {
        let mut progress = LogLoadProgress::new(1, "正在加载日志来源");
        progress.current_source = Some("logs.tar".to_string());
        progress.current_source_step = Some("TAR：扫描 TAR 条目".to_string());
        progress.current_entry = Some("very/deep/current.log".to_string());
        progress.current_source_work_done = 8;
        progress.current_source_work_total = None;

        assert!(progress.fraction() > 0.0);
        assert!(progress.fraction() < 1.0);
        assert_eq!(progress.entry_detail(), "条目：8");
        assert!(
            !progress.entry_detail().contains("当前"),
            "加载进度明细不应展示当前条目名，避免长路径在中央进度条里跳动"
        );
        assert!(
            !progress.entry_detail().contains("来源"),
            "进度条底部只显示百分比和条目进度，来源应显示在进度条上方"
        );
    }

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
            ArchiveFormat::from_path(Path::new("logs.gz")),
            Some(ArchiveFormat::Gzip)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("logs.GZ")),
            Some(ArchiveFormat::Gzip)
        );
        assert_eq!(
            ArchiveFormat::from_path(Path::new("logs.tar")),
            Some(ArchiveFormat::Tar)
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

    /// 验证 RAR 内层压缩包候选路径会先归一化。
    ///
    /// 业务意图：
    /// - Windows 创建的 RAR 可能把内部路径写成 `dir\inner.zip`。
    /// - 后续物化、读取和挂载树都使用 `/` 分隔路径，避免和 RAR 处理阶段的归一化名称比较失败。
    #[test]
    fn rar_内层压缩包候选路径会归一化() {
        assert_eq!(
            normalized_rar_nested_archive_member_path("dir\\inner.zip", false, 1024),
            Some("dir/inner.zip".to_string())
        );
        assert_eq!(
            normalized_rar_nested_archive_member_path("../inner.zip", false, 1024),
            None
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
            Some(LogFileSource::LocalFile {
                path: PathBuf::from("b.log"),
            }),
            None,
        );
        root.add_leaf_path(
            &["api".to_string(), "access.log".to_string()],
            LogTreeEntryKind::File,
            Some("2 KB".to_string()),
            Some(LogFileSource::LocalFile {
                path: PathBuf::from("api/access.log"),
            }),
            None,
        );
        root.sort_recursively();

        let mut rows = Vec::new();
        let mut next_row_id = 0usize;
        root.flatten_into(0, &mut next_row_id, &mut rows);

        assert_eq!(rows[0].meta.as_deref(), Some("2 个文件"));
        assert_eq!(rows[1].label, "api");
        assert_eq!(rows[1].kind, LogTreeEntryKind::Directory);
        assert!(rows[1].has_children);
        assert_eq!(rows[1].id, 1);
        assert_eq!(rows[1].meta.as_deref(), Some("1 个文件"));
        assert_eq!(rows[2].label, "access.log");
        assert!(!rows[2].has_children);
        assert_eq!(rows[3].label, "b.log");
    }

    /// 验证 TAR.GZ 扫描会消费非目标条目正文并继续读取后续条目。
    ///
    /// 业务意图：
    /// - TAR.GZ 是顺序流，目录树扫描虽然只展示路径，也必须排空每个文件正文才能移动到下一个条目。
    /// - 该测试构造两个文件，确保第一个文件内容不会被误读成第二个 tar 头并产生“条目读取失败”。
    #[test]
    fn tar_gz_扫描会列出多个文件条目() -> Result<(), Box<dyn Error>> {
        let temp_root = unique_temp_dir("logclinic3-tar-gz-tree-test")?;
        let archive_path = temp_root.join("logs.tar.gz");
        write_test_tar_gz(
            &archive_path,
            &[
                ("logs/first.log", b"INFO first".as_slice()),
                ("logs/second.log", b"INFO second".as_slice()),
            ],
        )?;

        let loaded = load_log_sources(vec![archive_path.clone()])?;
        let file_labels = loaded
            .rows
            .iter()
            .filter(|row| row.kind == LogTreeEntryKind::File)
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>();

        assert_eq!(loaded.error_count, 0);
        assert!(file_labels.contains(&"first.log"));
        assert!(file_labels.contains(&"second.log"));

        fs::remove_dir_all(temp_root)?;
        Ok(())
    }

    /// 验证 `.tar.gz` 扩展名但实际是单个 gzip 日志时不会显示 TAR 条目错误。
    ///
    /// 业务意图：
    /// - 用户现场文件名可能叫 `xxx.tar.gz`，但内容只是 gzip 压缩的单个日志，而不是 tar 归档。
    /// - 目录树应生成一个可点击文件节点，后续读取链路再按虚拟成员路径解压 gzip 内容。
    #[test]
    fn tar_gz_单文件gzip会作为文件节点展示() -> Result<(), Box<dyn Error>> {
        let temp_root = unique_temp_dir("logclinic3-single-gzip-tree-test")?;
        let archive_path = temp_root.join("single.tar.gz");
        write_test_gzip(&archive_path, b"INFO gzip only")?;

        let loaded = load_log_sources(vec![archive_path.clone()])?;
        let fallback_row = loaded
            .rows
            .iter()
            .find(|row| row.label == "single")
            .expect("单文件 gzip fallback 应显示为去掉压缩扩展名的文件节点");

        assert_eq!(loaded.error_count, 0);
        assert_eq!(fallback_row.kind, LogTreeEntryKind::File);
        assert!(matches!(
            fallback_row.source.as_ref(),
            Some(LogFileSource::ArchiveMember {
                archive_path: source_archive_path,
                archive_format: ArchiveFormat::TarGz,
                member_path,
            }) if source_archive_path == &archive_path
                && is_single_gzip_member_path(member_path)
        ));

        fs::remove_dir_all(temp_root)?;
        Ok(())
    }

    /// 验证 `.gz` 单文件压缩日志会作为压缩包根下的唯一日志文件展示。
    ///
    /// 业务意图：
    /// - 用户直接加载 `app.log.gz` 时，目录树应显示原始压缩文件根节点，并提供去掉 `.gz` 后缀的可点击日志节点。
    /// - 来源格式必须记录为 `Gzip`，避免后续读取和分页物化继续误走 TAR.GZ 分支。
    #[test]
    fn gz_单文件压缩日志会作为文件节点展示() -> Result<(), Box<dyn Error>> {
        let temp_root = unique_temp_dir("logclinic3-gzip-tree-test")?;
        let archive_path = temp_root.join("app.log.gz");
        write_test_gzip(&archive_path, b"INFO gzip log")?;

        let loaded = load_log_sources(vec![archive_path.clone()])?;
        let root_row = loaded
            .rows
            .iter()
            .find(|row| row.label == "app.log.gz")
            .expect("GZ 根节点应保留用户选择的原始文件名");
        let gzip_row = loaded
            .rows
            .iter()
            .find(|row| row.label == "app.log")
            .expect("GZ 内部文件节点应显示为去掉 .gz 后缀的名称");

        assert_eq!(loaded.error_count, 0);
        assert_eq!(root_row.meta.as_deref(), Some("GZ"));
        assert_eq!(gzip_row.kind, LogTreeEntryKind::File);
        assert!(matches!(
            gzip_row.source.as_ref(),
            Some(LogFileSource::ArchiveMember {
                archive_path: source_archive_path,
                archive_format: ArchiveFormat::Gzip,
                member_path,
            }) if source_archive_path == &archive_path
                && is_single_gzip_member_path(member_path)
        ));

        fs::remove_dir_all(temp_root)?;
        Ok(())
    }

    /// 验证损坏 `.gz` 不会被伪装成普通日志文件。
    ///
    /// 业务意图：
    /// - `.gz` 扩展名已经声明为压缩格式，损坏时应在压缩包根节点下展示读取错误，而不是把压缩二进制交给文本解码。
    /// - 这能避免用户看到乱码并误以为日志编码选择错误。
    #[test]
    fn 损坏_gz_会展示压缩读取错误() -> Result<(), Box<dyn Error>> {
        let temp_root = unique_temp_dir("logclinic3-bad-gzip-tree-test")?;
        let archive_path = temp_root.join("bad.log.gz");
        fs::write(&archive_path, b"not a gzip stream")?;

        let loaded = load_log_sources(vec![archive_path.clone()])?;

        assert_eq!(loaded.error_count, 1);
        assert!(
            loaded
                .rows
                .iter()
                .any(|row| row.label == "压缩包读取失败" && row.kind == LogTreeEntryKind::Error)
        );
        assert!(
            loaded
                .rows
                .iter()
                .filter_map(|row| row.source.as_ref())
                .all(|source| !matches!(
                    source,
                    LogFileSource::ArchiveMember {
                        archive_format: ArchiveFormat::Gzip,
                        ..
                    }
                )),
            "损坏 GZ 不应生成可打开的 Gzip 来源"
        );

        fs::remove_dir_all(temp_root)?;
        Ok(())
    }

    /// 验证扩展名为 `.tar.gz` 但文件头是纯 tar 时按 TAR 加载。
    ///
    /// 业务意图：
    /// - 用户提供的现场样本就是这种扩展名与真实格式不一致的文件。
    /// - 加载树必须根据内容纠偏，否则会错误进入 gzip 解码并显示 “TAR.GZ 条目读取失败”。
    #[test]
    fn tar_gz_扩展名纯tar会按tar目录加载() -> Result<(), Box<dyn Error>> {
        let temp_root = unique_temp_dir("logclinic3-plain-tar-tree-test")?;
        let archive_path = temp_root.join("wrong-name.tar.gz");
        write_test_tar(
            &archive_path,
            &[
                ("logs/first.log", b"INFO first".as_slice()),
                ("logs/second.log", b"INFO second".as_slice()),
            ],
        )?;

        assert_eq!(
            ArchiveFormat::from_file(&archive_path),
            Some(ArchiveFormat::Tar)
        );
        let loaded = load_log_sources(vec![archive_path.clone()])?;
        let root_row = loaded
            .rows
            .iter()
            .find(|row| row.label == "wrong-name.tar.gz")
            .expect("压缩包根节点应保留用户看到的原始文件名");
        let second_row = loaded
            .rows
            .iter()
            .find(|row| row.label == "second.log")
            .expect("纯 tar 内部文件应正常出现在目录树");

        assert_eq!(loaded.error_count, 0);
        assert_eq!(root_row.meta.as_deref(), Some("TAR"));
        assert!(matches!(
            second_row.source.as_ref(),
            Some(LogFileSource::ArchiveMember {
                archive_path: source_archive_path,
                archive_format: ArchiveFormat::Tar,
                member_path,
            }) if source_archive_path == &archive_path && member_path == "logs/second.log"
        ));

        fs::remove_dir_all(temp_root)?;
        Ok(())
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

    /// 验证外层 ZIP 中的多文件 ZIP 会作为目录展开。
    ///
    /// 业务意图：
    /// - 内层压缩包如果包含多个文件，不能继续走“单文件压缩包直接打开”规则，否则用户无法选择具体日志。
    /// - 展开后的子文件必须保存嵌套来源，后续打开正文时才能先定位外层成员，再读取内层成员。
    #[test]
    fn 外层_zip_中的多文件_zip_会作为目录展开() -> Result<(), Box<dyn Error>> {
        let temp_root = unique_temp_dir("logclinic3-nested-zip-tree-test")?;
        let outer_path = temp_root.join("outer.zip");

        let mut inner_bytes = Cursor::new(Vec::new());
        {
            let mut inner_writer = zip::ZipWriter::new(&mut inner_bytes);
            inner_writer.start_file("a.log", SimpleFileOptions::default())?;
            inner_writer.write_all(b"INFO a")?;
            inner_writer.start_file("dir/b.log", SimpleFileOptions::default())?;
            inner_writer.write_all(b"INFO b")?;
            inner_writer.finish()?;
        }

        {
            let outer_file = File::create(&outer_path)?;
            let mut outer_writer = zip::ZipWriter::new(outer_file);
            outer_writer.start_file("nested.zip", SimpleFileOptions::default())?;
            outer_writer.write_all(inner_bytes.get_ref())?;
            outer_writer.finish()?;
        }

        let loaded = load_log_sources(vec![outer_path.clone()])?;
        let nested_row = loaded
            .rows
            .iter()
            .find(|row| row.label == "nested.zip")
            .expect("内层多文件 ZIP 应显示为可展开目录");
        assert_eq!(nested_row.kind, LogTreeEntryKind::Directory);
        assert!(nested_row.has_children);

        let nested_sources = loaded
            .rows
            .iter()
            .filter_map(|row| row.source.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(nested_sources.len(), 2);
        assert!(nested_sources.iter().any(|source| matches!(
            source,
            LogFileSource::NestedArchiveMember {
                outer_archive_path,
                outer_archive_format: ArchiveFormat::Zip,
                archive_member_path,
                nested_archive_format: ArchiveFormat::Zip,
                nested_member_path,
            } if outer_archive_path == &outer_path
                && archive_member_path == "nested.zip"
                && nested_member_path == "a.log"
        )));
        assert!(nested_sources.iter().any(|source| matches!(
            source,
            LogFileSource::NestedArchiveMember {
                nested_member_path,
                ..
            } if nested_member_path == "dir/b.log"
        )));

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

    /// 写入测试用 TAR.GZ 文件。
    ///
    /// 业务意图：
    /// - 多个测试都需要构造真实 gzip 包裹的 tar 顺序流，集中封装可以避免遗漏 header checksum 或 gzip finish。
    fn write_test_tar_gz(path: &Path, files: &[(&str, &[u8])]) -> io::Result<()> {
        let file = File::create(path)?;
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = TarBuilder::new(encoder);

        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(name)?;
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, *bytes)?;
        }

        let encoder = builder.into_inner()?;
        encoder.finish()?;
        Ok(())
    }

    /// 写入测试用未压缩 TAR 文件。
    ///
    /// 业务意图：
    /// - 用同样的 tar header 构造真实纯 tar 包，覆盖扩展名误写时的内容探测分支。
    fn write_test_tar(path: &Path, files: &[(&str, &[u8])]) -> io::Result<()> {
        let file = File::create(path)?;
        let mut builder = TarBuilder::new(file);

        for (name, bytes) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(name)?;
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append(&header, *bytes)?;
        }

        builder.finish()?;
        Ok(())
    }

    /// 写入测试用单文件 gzip 日志。
    ///
    /// 业务意图：
    /// - 覆盖“扩展名是 `.tar.gz` 但 gzip 内容不是 tar 归档”的兼容路径。
    fn write_test_gzip(path: &Path, bytes: &[u8]) -> io::Result<()> {
        let file = File::create(path)?;
        let mut encoder = GzEncoder::new(file, Compression::default());
        encoder.write_all(bytes)?;
        encoder.finish()?;
        Ok(())
    }
}
