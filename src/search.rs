//! 日志搜索核心模块。
//!
//! 业务意图：
//! - 本模块只处理“在哪些文本行或日志来源中查找查询词”，不直接依赖 GPUI 渲染状态。
//! - UI 可以把当前已解码文件、同目录文件来源和后台读取错误统一转换成这里的结果模型。
//! - 当前第一版只支持普通文本搜索，不支持正则和替换，避免在大日志场景中引入未定义的性能与语义边界。
//!
//! 关键约束：
//! - 查询词可能包含中文或其它多字节字符，所有命中范围都必须落在 UTF-8 字节边界上。
//! - 目录搜索只收集已有加载树中的可打开文件来源，不重新扫描文件系统，避免扩大用户授权的读取范围。
//! - 单个文件读取或解码失败时由 UI 记录为文件级错误，不能中断其它文件的搜索。

use std::{collections::HashSet, path::Path};

use crate::log_loader::{LoadedLogTree, LogFileSource};

/// 搜索范围。
///
/// 业务意图：
/// - 用户可以在当前 tab 已打开的文件中搜索，也可以在该文件所在目录及子目录中搜索。
/// - 枚举让 UI 状态、后台任务和测试共享同一套范围语义。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchScope {
    /// 只搜索当前激活 tab 对应的已解码文件。
    CurrentFile,
    /// 搜索当前文件所在目录及全部子目录中的可打开文件。
    CurrentDirectory,
}

impl SearchScope {
    /// 返回 UI 显示文案。
    ///
    /// 边界条件：
    /// - 文案只服务当前中文界面，不作为持久化或协议字段。
    pub fn label(self) -> &'static str {
        match self {
            Self::CurrentFile => "当前文件",
            Self::CurrentDirectory => "当前目录",
        }
    }
}

/// 单次搜索使用的匹配选项。
///
/// 业务意图：
/// - 查询词保持用户原始输入，是否区分大小写由独立布尔值控制。
/// - 这里不保存搜索范围，是为了让“搜索哪些文件”和“如何匹配文本”两个规则可以分别测试。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchOptions {
    /// 用户输入的普通文本查询词。
    pub query: String,
    /// 是否区分大小写。
    ///
    /// 边界条件：
    /// - 不区分大小写时使用 Rust `to_lowercase` 做 Unicode 级别折叠，适合中文和 ASCII 混合日志的第一版需求。
    pub case_sensitive: bool,
}

impl SearchOptions {
    /// 查询词去掉首尾空白后是否为空。
    ///
    /// 业务意图：
    /// - 空查询没有业务意义，UI 应阻止启动后台任务并给出提示。
    pub fn is_empty_query(&self) -> bool {
        self.query.trim().is_empty()
    }
}

/// 搜索进度快照。
///
/// 业务意图：
/// - 后台目录搜索逐文件推进，UI 需要在搜索对话框中实时显示已完成文件数和命中数。
/// - 当前文件搜索也复用该结构，表现为总文件数为 1。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchProgress {
    /// 已经完成搜索的文件数量。
    pub searched_files: usize,
    /// 本次任务预计搜索的文件总数。
    pub total_files: usize,
    /// 当前已经累计的命中条数。
    pub matched_lines: usize,
}

/// 单条搜索命中。
///
/// 业务意图：
/// - 结果面板需要展示文件、行号、预览文本，并在点击时重新定位到对应 `LogFileSource`。
/// - 命中范围使用当前 `line_text` 内的 UTF-8 字节范围，直接可用于 `StyledText` 高亮。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchResultItem {
    /// 命中文件来源。
    pub source: LogFileSource,
    /// 来源稳定键，供 UI 去重或定位已有 tab。
    pub source_key: String,
    /// 文件短名称。
    pub file_name: String,
    /// 用于结果面板展示的目录或压缩包路径摘要。
    pub location: String,
    /// 0 基行号。
    pub line_index: usize,
    /// 命中行的完整文本。
    pub line_text: String,
    /// 命中片段在 `line_text` 中的 UTF-8 字节范围。
    pub match_range: std::ops::Range<usize>,
}

/// 单个文件搜索失败信息。
///
/// 业务意图：
/// - 目录搜索应尽量返回可用结果；个别文件失败时把错误汇总到结果面板，方便用户判断搜索覆盖是否完整。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchFileError {
    /// 失败文件名称。
    pub file_name: String,
    /// 面向用户的中文错误说明。
    pub message: String,
}

/// 在已解码的行集合中搜索查询词。
///
/// 业务意图：
/// - 当前文件搜索直接复用已解码文档行，避免重新读取文件或压缩包。
/// - 目录搜索中的单个文件完成解码后也复用该函数，保证两种范围的匹配行为一致。
///
/// 边界条件：
/// - 空查询返回空结果，调用方应在 UI 层阻止启动搜索并给出提示。
/// - 每行只记录第一次命中，第一版结果面板以“命中行”为单位展示，而不是同一行多个片段。
pub fn search_lines(
    source: &LogFileSource,
    lines: &[String],
    options: &SearchOptions,
) -> Vec<SearchResultItem> {
    if options.is_empty_query() {
        return Vec::new();
    }

    let file_name = source.display_name();
    let source_key = source.stable_key();
    let location = source_location_label(source);

    lines
        .iter()
        .enumerate()
        .filter_map(|(line_index, line_text)| {
            find_query_range(line_text, &options.query, options.case_sensitive).map(|match_range| {
                SearchResultItem {
                    source: source.clone(),
                    source_key: source_key.clone(),
                    file_name: file_name.clone(),
                    location: location.clone(),
                    line_index,
                    line_text: line_text.clone(),
                    match_range,
                }
            })
        })
        .collect()
}

/// 在单行文本中查找查询词并返回原始行内的字节范围。
///
/// 业务意图：
/// - 不区分大小写搜索需要在折叠后的文本中查找，但最终高亮必须回到原始文本的 UTF-8 字节范围。
/// - 通过字符序号映射避免把中文或其它多字节字符切在中间。
fn find_query_range(
    line_text: &str,
    query: &str,
    case_sensitive: bool,
) -> Option<std::ops::Range<usize>> {
    if query.is_empty() {
        return None;
    }

    if case_sensitive {
        return line_text
            .find(query)
            .map(|start| start..start + query.len());
    }

    let folded_line = line_text.to_lowercase();
    let folded_query = query.to_lowercase();
    let folded_start = folded_line.find(&folded_query)?;
    let folded_end = folded_start + folded_query.len();
    let start = byte_index_from_folded_offset(line_text, folded_start);
    let end = byte_index_from_folded_offset(line_text, folded_end);

    (start <= end && line_text.is_char_boundary(start) && line_text.is_char_boundary(end))
        .then_some(start..end)
}

/// 将 `to_lowercase` 后的字节偏移映射回原始字符串的字节偏移。
///
/// 业务意图：
/// - 某些 Unicode 字符大小写折叠后字节长度会变化，不能直接把折叠字符串的 `find` 结果用于原字符串。
/// - 这里逐字符累计折叠后长度，找到不超过目标偏移的原始字符边界，保证返回值始终是 UTF-8 边界。
fn byte_index_from_folded_offset(original: &str, folded_offset: usize) -> usize {
    let mut folded_cursor = 0usize;

    for (byte_index, character) in original.char_indices() {
        if folded_cursor >= folded_offset {
            return byte_index;
        }
        folded_cursor += character.to_lowercase().to_string().len();
    }

    original.len()
}

/// 从已加载目录树收集当前文件所在目录及子目录中的文件来源。
///
/// 业务意图：
/// - “当前目录搜索”不能重新扫描磁盘或压缩包，只能在用户已经加载并授权的目录树范围内搜索。
/// - 本地文件按文件系统父目录递归；压缩包成员按同一压缩包内的成员父路径递归。
///
/// 边界条件：
/// - 如果当前来源不在加载树中，也仍按来源路径规则收集可匹配来源，避免单文件直接打开后搜索失效。
/// - 同一个来源可能在树中重复出现，结果按 `stable_key` 去重，避免重复搜索。
pub fn collect_current_directory_sources(
    tree: &LoadedLogTree,
    current_source: &LogFileSource,
) -> Vec<LogFileSource> {
    let mut seen_keys = HashSet::new();
    let mut sources = Vec::new();

    for source in tree.rows.iter().filter_map(|row| row.source.as_ref()) {
        if source_belongs_to_current_directory(source, current_source) {
            let key = source.stable_key();
            if seen_keys.insert(key) {
                sources.push(source.clone());
            }
        }
    }

    sources
}

/// 判断候选来源是否属于当前文件所在目录的递归范围。
fn source_belongs_to_current_directory(
    candidate: &LogFileSource,
    current_source: &LogFileSource,
) -> bool {
    match (candidate, current_source) {
        (LogFileSource::LocalFile { path }, LogFileSource::LocalFile { path: current_path }) => {
            let Some(current_parent) = current_path.parent() else {
                return false;
            };
            path_starts_with(path, current_parent)
        }
        (
            LogFileSource::ArchiveMember {
                archive_path,
                archive_format,
                member_path,
            },
            LogFileSource::ArchiveMember {
                archive_path: current_archive_path,
                archive_format: current_archive_format,
                member_path: current_member_path,
            },
        ) => {
            archive_format == current_archive_format
                && path_key(archive_path) == path_key(current_archive_path)
                && archive_member_belongs_to_directory(member_path, current_member_path)
        }
        _ => false,
    }
}

/// 判断本地路径是否位于指定目录下。
///
/// 边界条件：
/// - 测试和失败回退路径可能不是绝对路径，因此不能强制 `canonicalize`。
fn path_starts_with(path: &Path, parent: &Path) -> bool {
    path.starts_with(parent)
}

/// 判断压缩包成员是否位于当前成员的父目录递归范围内。
fn archive_member_belongs_to_directory(member_path: &str, current_member_path: &str) -> bool {
    let current_parent = archive_member_parent(current_member_path);
    current_parent.as_deref().is_none_or(|parent| {
        member_path == parent || member_path.starts_with(&format!("{parent}/"))
    })
}

/// 返回压缩包成员的父目录路径。
fn archive_member_parent(member_path: &str) -> Option<String> {
    member_path
        .rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
}

/// 为结果面板和目录搜索目标生成来源位置摘要。
///
/// 业务意图：
/// - 搜索结果需要展示“命中来自哪里”，目录搜索对话框也需要把当前搜索目标目录展示给用户确认和修改。
/// - 这里返回的是 UI 层可读目录标签，不作为持久化路径或权限边界；真实可搜索范围仍由已加载目录树限制。
pub fn source_location_label(source: &LogFileSource) -> String {
    match source {
        LogFileSource::LocalFile { path } => path
            .parent()
            .map(|parent| parent.display().to_string())
            .unwrap_or_else(|| "本地文件".to_string()),
        LogFileSource::ArchiveMember {
            archive_path,
            member_path,
            ..
        } => {
            let archive_name = archive_path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| archive_path.display().to_string());
            archive_member_parent(member_path)
                .map(|parent| format!("{archive_name}/{parent}"))
                .unwrap_or(archive_name)
        }
    }
}

/// 将路径转换为跨平台比较用的有损字符串。
///
/// 业务意图：
/// - 压缩包路径比较只用于当前加载结果内匹配同一个压缩包，使用有损展示字符串足够稳定。
fn path_key(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// 构造本地文件来源，供单元测试保持简洁。
#[cfg(test)]
fn local(path: impl Into<std::path::PathBuf>) -> LogFileSource {
    LogFileSource::LocalFile { path: path.into() }
}

/// 构造压缩包成员来源，供单元测试保持简洁。
#[cfg(test)]
fn archive(member_path: &str) -> LogFileSource {
    use std::path::PathBuf;

    LogFileSource::ArchiveMember {
        archive_path: PathBuf::from("logs.zip"),
        archive_format: crate::log_loader::ArchiveFormat::Zip,
        member_path: member_path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    //! 搜索核心规则测试。
    //!
    //! 业务意图：
    //! - 搜索结果会驱动 UI 打开文件和滚动定位，必须保证匹配范围、目录递归和错误不中断的基础规则稳定。

    use super::*;
    use crate::log_loader::{LogTreeEntryKind, LogTreeRow};

    /// 构造测试用加载树。
    fn tree_with_sources(sources: Vec<LogFileSource>) -> LoadedLogTree {
        LoadedLogTree {
            summary: "测试树".to_string(),
            rows: sources
                .into_iter()
                .enumerate()
                .map(|(id, source)| LogTreeRow {
                    id,
                    depth: 0,
                    label: source.display_name(),
                    kind: LogTreeEntryKind::File,
                    has_children: false,
                    meta: None,
                    error_message: None,
                    source: Some(source),
                })
                .collect(),
            error_count: 0,
        }
    }

    /// 普通文本搜索默认支持不区分大小写匹配。
    #[test]
    fn 普通文本搜索支持不区分大小写() {
        let source = local("/tmp/app/access.log");
        let lines = vec!["INFO started".to_string(), "error failed".to_string()];
        let results = search_lines(
            &source,
            &lines,
            &SearchOptions {
                query: "ERROR".to_string(),
                case_sensitive: false,
            },
        );

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].line_index, 1);
        assert_eq!(results[0].match_range, 0..5);
    }

    /// 区分大小写开启后只接受完全一致的文本。
    #[test]
    fn 区分大小写搜索只匹配原始大小写() {
        let source = local("/tmp/app/access.log");
        let lines = vec!["error lower".to_string(), "ERROR upper".to_string()];
        let results = search_lines(
            &source,
            &lines,
            &SearchOptions {
                query: "ERROR".to_string(),
                case_sensitive: true,
            },
        );

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].line_index, 1);
    }

    /// 中文查询词命中范围必须保持 UTF-8 字节边界，供 `StyledText` 安全高亮。
    #[test]
    fn 中文查询词返回合法_utf8_范围() {
        let source = local("/tmp/app/access.log");
        let lines = vec!["服务启动失败".to_string()];
        let results = search_lines(
            &source,
            &lines,
            &SearchOptions {
                query: "启动".to_string(),
                case_sensitive: false,
            },
        );

        let range = results[0].match_range.clone();
        assert!(results[0].line_text.is_char_boundary(range.start));
        assert!(results[0].line_text.is_char_boundary(range.end));
        assert_eq!(&results[0].line_text[range], "启动");
    }

    /// 本地当前目录搜索应递归收集子目录文件，但不越过当前目录边界。
    #[test]
    fn 本地当前目录递归收集来源() {
        let current = local("/logs/app/a.log");
        let tree = tree_with_sources(vec![
            current.clone(),
            local("/logs/app/nested/b.log"),
            local("/logs/other/c.log"),
        ]);
        let sources = collect_current_directory_sources(&tree, &current);

        assert_eq!(sources.len(), 2);
        assert!(sources.iter().any(|source| source == &current));
        assert!(
            sources
                .iter()
                .any(|source| source == &local("/logs/app/nested/b.log"))
        );
    }

    /// 压缩包成员当前目录搜索应限制在同一压缩包和同一成员目录递归范围内。
    #[test]
    fn 压缩包成员当前目录递归收集来源() {
        let current = archive("2026/access/a.log");
        let tree = tree_with_sources(vec![
            current.clone(),
            archive("2026/access/nested/b.log"),
            archive("2026/error/c.log"),
        ]);
        let sources = collect_current_directory_sources(&tree, &current);

        assert_eq!(sources.len(), 2);
        assert!(sources.iter().any(|source| source == &current));
        assert!(
            sources
                .iter()
                .any(|source| source == &archive("2026/access/nested/b.log"))
        );
    }
}
