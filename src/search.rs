//! 日志搜索核心模块。
//!
//! 业务意图：
//! - 本模块只处理“在哪些文本行或日志来源中查找查询词”，不直接依赖 GPUI 渲染状态。
//! - UI 可以把当前已解码文件、同目录文件来源和后台读取错误统一转换成这里的结果模型。
//! - 当前支持普通文本和 Rust `regex` 正则两种匹配模式；快搜继续使用多关键字普通文本 OR 搜索。
//!
//! 关键约束：
//! - 查询词可能包含中文或其它多字节字符，所有命中范围都必须落在 UTF-8 字节边界上。
//! - 正则搜索按单行执行，不做跨行匹配；`^` 和 `$` 只对应当前行边界。
//! - 目录搜索只收集已有加载树中的可打开文件来源，不重新扫描文件系统，避免扩大用户授权的读取范围。
//! - 单个文件读取或解码失败时由 UI 记录为文件级错误，不能中断其它文件的搜索。

use std::{collections::HashSet, error::Error, fmt, ops::Range, path::Path};

use regex::Regex;

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

/// 搜索匹配模式。
///
/// 业务意图：
/// - 普通搜索窗口可以在普通文本和正则之间切换，快搜仍固定为普通文本 OR，避免用户的正则开关改变快搜语义。
/// - 模式作为显式枚举保存在任务和历史记录中，结果面板可以准确展示本次搜索的匹配规则。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMatchMode {
    /// 普通文本匹配，支持现有“区分大小写”开关。
    Literal,
    /// Rust `regex` 正则匹配；大小写完全由表达式自身控制，例如 `(?i)`。
    Regex,
}

impl SearchMatchMode {
    /// 返回面向用户的短标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Literal => "普通文本",
            Self::Regex => "正则",
        }
    }

    /// 返回切换后的模式。
    pub fn toggled(self) -> Self {
        match self {
            Self::Literal => Self::Regex,
            Self::Regex => Self::Literal,
        }
    }
}

/// 搜索表达式校验错误。
///
/// 业务意图：
/// - 正则表达式必须在启动后台任务前校验，避免无效表达式生成空结果记录或写入历史。
/// - 错误文案面向中文 UI，底层 `regex` 错误只作为补充信息，方便用户定位表达式问题。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchPatternError {
    /// 可直接展示给用户的中文错误说明。
    message: String,
}

impl SearchPatternError {
    /// 构造无效正则错误。
    fn invalid_regex(query: &str, error: regex::Error) -> Self {
        Self {
            message: format!("正则表达式无效：{query}（{error}）"),
        }
    }
}

impl fmt::Display for SearchPatternError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SearchPatternError {}

/// 单次搜索使用的匹配选项。
///
/// 业务意图：
/// - 普通搜索传入单个查询词，快搜传入多个配置关键字，底层统一按“任一关键字命中”处理。
/// - 查询词保持用户原始输入；普通文本模式由独立布尔值控制大小写，正则模式忽略该布尔值。
/// - 这里不保存搜索范围，是为了让“搜索哪些文件”和“如何匹配文本”两个规则可以分别测试。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchOptions {
    /// 本次搜索使用的查询词列表。
    ///
    /// 边界条件：
    /// - 普通搜索只有一个关键字；快搜会包含多个普通文本关键字。
    /// - 空白关键字在构造时会被丢弃，避免逗号连续出现时生成无意义命中。
    pub queries: Vec<String>,
    /// 是否区分大小写。
    ///
    /// 边界条件：
    /// - 普通文本不区分大小写时使用 Rust `to_lowercase` 做 Unicode 级别折叠，适合中文和 ASCII 混合日志的第一版需求。
    /// - 正则模式不读取该字段，大小写应通过 `(?i)` 等 Rust regex 语法表达。
    pub case_sensitive: bool,
    /// 匹配模式。
    ///
    /// 业务意图：
    /// - 任务创建、计数、分页搜索和结果展示必须共享同一个模式，避免 UI 记录与后台实际匹配方式不一致。
    pub match_mode: SearchMatchMode,
}

impl SearchOptions {
    /// 构造普通文本单关键字搜索选项。
    ///
    /// 业务意图：
    /// - 当前生产路径会显式传入匹配模式，测试仍需要一个短入口构造默认普通文本搜索，避免每个用例重复写模式参数。
    #[cfg(test)]
    pub fn single(query: impl Into<String>, case_sensitive: bool) -> Self {
        Self::single_with_mode(query, case_sensitive, SearchMatchMode::Literal)
    }

    /// 构造指定模式的单关键字搜索选项。
    ///
    /// 业务意图：
    /// - 普通搜索窗口用该入口把“正则”开关传入核心搜索；快搜不调用它，保证快搜始终为普通文本 OR。
    pub fn single_with_mode(
        query: impl Into<String>,
        case_sensitive: bool,
        match_mode: SearchMatchMode,
    ) -> Self {
        let query = query.into();
        let queries = (!query.trim().is_empty())
            .then_some(query)
            .into_iter()
            .collect();
        Self {
            queries,
            case_sensitive,
            match_mode,
        }
    }

    /// 构造任一关键字命中的普通文本多关键字搜索选项。
    ///
    /// 业务意图：
    /// - 快搜必须保持普通文本 OR 语义，不受搜索窗口“正则”开关影响。
    pub fn any(queries: impl IntoIterator<Item = String>, case_sensitive: bool) -> Self {
        Self {
            queries: queries
                .into_iter()
                .map(|query| query.trim().to_string())
                .filter(|query| !query.is_empty())
                .collect(),
            case_sensitive,
            match_mode: SearchMatchMode::Literal,
        }
    }

    /// 查询词去掉首尾空白后是否为空。
    ///
    /// 业务意图：
    /// - 空查询没有业务意义，UI 应阻止启动后台任务并给出提示；快搜配置全为空时同样不能启动。
    pub fn is_empty_query(&self) -> bool {
        self.queries.is_empty() || self.queries.iter().all(|query| query.trim().is_empty())
    }

    /// 校验搜索表达式是否可以执行。
    ///
    /// 业务意图：
    /// - UI 在启动搜索或计数前调用该方法，保证无效正则不会创建后台任务、结果记录或搜索历史。
    /// - 普通文本模式没有可恢复的语法错误，只要非空即可执行。
    pub fn validate(&self) -> Result<(), SearchPatternError> {
        SearchMatcher::new(self).map(|_| ())
    }
}

/// 已编译的搜索匹配器。
///
/// 业务意图：
/// - 内存搜索、分页搜索和计数都通过该类型执行匹配，避免不同路径对正则、大小写和多关键字优先级产生分歧。
/// - 正则表达式只在任务启动前或搜索函数入口编译一次，避免大文件逐行重复编译带来的性能问题。
pub(crate) struct SearchMatcher {
    /// 普通文本模式是否区分大小写；正则模式忽略。
    case_sensitive: bool,
    /// 已规范化的查询词匹配器列表。
    terms: Vec<SearchMatcherTerm>,
}

/// 单个查询词的可执行匹配器。
enum SearchMatcherTerm {
    /// 普通文本查询词。
    Literal(String),
    /// 已编译正则表达式。
    Regex(Regex),
}

impl SearchMatcher {
    /// 根据搜索选项构造匹配器。
    ///
    /// 边界条件：
    /// - 空查询返回空匹配器，由调用方按“无结果”处理。
    /// - 正则模式下任一表达式编译失败都返回中文错误，不允许部分表达式继续执行，避免结果含义不完整。
    pub(crate) fn new(options: &SearchOptions) -> Result<Self, SearchPatternError> {
        let mut terms = Vec::new();
        for query in options
            .queries
            .iter()
            .filter(|query| !query.trim().is_empty())
        {
            match options.match_mode {
                SearchMatchMode::Literal => {
                    terms.push(SearchMatcherTerm::Literal(query.to_string()));
                }
                SearchMatchMode::Regex => {
                    let regex = Regex::new(query)
                        .map_err(|error| SearchPatternError::invalid_regex(query, error))?;
                    terms.push(SearchMatcherTerm::Regex(regex));
                }
            }
        }

        Ok(Self {
            case_sensitive: options.case_sensitive,
            terms,
        })
    }

    /// 返回单行中的最佳命中范围。
    ///
    /// 业务意图：
    /// - 结果面板以命中行为单位展示，单行内只需要一个高亮范围。
    /// - 多个查询词同时命中时选择最靠前的命中；同一位置保留查询词顺序，保持快搜优先级可预测。
    pub(crate) fn best_range(&self, line_text: &str) -> Option<Range<usize>> {
        let mut best: Option<(usize, Range<usize>)> = None;
        for (query_index, term) in self.terms.iter().enumerate() {
            let Some(match_range) = term.find_range(line_text, self.case_sensitive) else {
                continue;
            };
            let should_replace = match best.as_ref() {
                Some((best_index, best_range)) => {
                    match_range.start < best_range.start
                        || (match_range.start == best_range.start && query_index < *best_index)
                }
                None => true,
            };
            if should_replace {
                best = Some((query_index, match_range));
            }
        }
        best.map(|(_, range)| range)
    }

    /// 统计单行中的非重叠命中次数。
    ///
    /// 业务意图：
    /// - “计数”按钮统计片段出现次数，不等同于结果面板的命中行数。
    /// - 正则使用 `find_iter` 的非重叠语义，普通文本沿用 `matches` 的非重叠语义。
    pub(crate) fn count_in_line(&self, line_text: &str) -> usize {
        self.terms
            .iter()
            .map(|term| term.count_in_line(line_text, self.case_sensitive))
            .sum()
    }
}

impl SearchMatcherTerm {
    /// 返回当前查询词在单行中的第一个命中范围。
    fn find_range(&self, line_text: &str, case_sensitive: bool) -> Option<Range<usize>> {
        match self {
            Self::Literal(query) => find_literal_query_range(line_text, query, case_sensitive),
            Self::Regex(regex) => regex
                .find(line_text)
                .map(|matched| matched.start()..matched.end()),
        }
    }

    /// 统计当前查询词在单行中的非重叠命中次数。
    fn count_in_line(&self, line_text: &str, case_sensitive: bool) -> usize {
        match self {
            Self::Literal(query) => {
                count_literal_query_occurrences_in_line(line_text, query, case_sensitive)
            }
            Self::Regex(regex) => regex.find_iter(line_text).count(),
        }
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
    pub match_range: Range<usize>,
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
/// - 每行只记录一个命中，结果面板以“命中行”为单位展示，而不是同一行多个片段。
/// - 多关键字同时命中同一行时，高亮最靠前的命中；位置相同则按关键字配置顺序优先。
pub fn search_lines(
    source: &LogFileSource,
    lines: &[String],
    options: &SearchOptions,
) -> Vec<SearchResultItem> {
    if options.is_empty_query() {
        return Vec::new();
    }
    let Ok(matcher) = SearchMatcher::new(options) else {
        return Vec::new();
    };

    let file_name = source.display_name();
    let source_key = source.stable_key();
    let location = source_location_label(source);

    lines
        .iter()
        .enumerate()
        .filter_map(|(line_index, line_text)| {
            matcher
                .best_range(line_text)
                .map(|match_range| SearchResultItem {
                    source: source.clone(),
                    source_key: source_key.clone(),
                    file_name: file_name.clone(),
                    location: location.clone(),
                    line_index,
                    line_text: line_text.clone(),
                    match_range,
                })
        })
        .collect()
}

/// 统计已解码行集合中查询词的总出现次数。
///
/// 业务意图：
/// - 搜索窗口需要在用户输入关键字时即时显示“当前文件命中次数”，这个计数以片段出现次数为单位，
///   不等同于结果面板当前第一版采用的“命中行数”。
/// - 计数只消费已经打开并解码的当前文件行集合，不读取磁盘、不扫描目录，避免扩大搜索窗口的同步开销。
///
/// 边界条件：
/// - 空查询返回 0，调用方可决定是否展示占位。
/// - 同一行内多次出现会累计；匹配采用非重叠语义，和普通文本查找工具保持一致。
/// - 不区分大小写时使用 `to_lowercase`，与 `search_lines` 的匹配规则保持一致。
pub fn count_query_occurrences(lines: &[String], options: &SearchOptions) -> usize {
    if options.is_empty_query() {
        return 0;
    }
    let Ok(matcher) = SearchMatcher::new(options) else {
        return 0;
    };

    lines.iter().map(|line| matcher.count_in_line(line)).sum()
}

/// 统计单行中的普通文本查询词出现次数。
fn count_literal_query_occurrences_in_line(
    line_text: &str,
    query: &str,
    case_sensitive: bool,
) -> usize {
    if query.is_empty() {
        return 0;
    }

    if case_sensitive {
        return line_text.matches(query).count();
    }

    let folded_line = line_text.to_lowercase();
    let folded_query = query.to_lowercase();
    folded_line.matches(&folded_query).count()
}

/// 在单行文本中查找普通文本查询词并返回原始行内的字节范围。
///
/// 业务意图：
/// - 不区分大小写搜索需要在折叠后的文本中查找，但最终高亮必须回到原始文本的 UTF-8 字节范围。
/// - 通过字符序号映射避免把中文或其它多字节字符切在中间。
fn find_literal_query_range(
    line_text: &str,
    query: &str,
    case_sensitive: bool,
) -> Option<Range<usize>> {
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
        (
            LogFileSource::MaterializedArchiveMember {
                archive_path,
                archive_format,
                member_path,
                ..
            },
            LogFileSource::MaterializedArchiveMember {
                archive_path: current_archive_path,
                archive_format: current_archive_format,
                member_path: current_member_path,
                ..
            },
        ) => {
            archive_format == current_archive_format
                && path_key(archive_path) == path_key(current_archive_path)
                && archive_member_belongs_to_directory(member_path, current_member_path)
        }
        (
            LogFileSource::NestedArchiveMember {
                outer_archive_path,
                outer_archive_format,
                archive_member_path,
                nested_archive_format,
                nested_member_path,
            },
            LogFileSource::NestedArchiveMember {
                outer_archive_path: current_outer_archive_path,
                outer_archive_format: current_outer_archive_format,
                archive_member_path: current_archive_member_path,
                nested_archive_format: current_nested_archive_format,
                nested_member_path: current_nested_member_path,
            },
        ) => {
            outer_archive_format == current_outer_archive_format
                && path_key(outer_archive_path) == path_key(current_outer_archive_path)
                && archive_member_path == current_archive_member_path
                && nested_archive_format == current_nested_archive_format
                && archive_member_belongs_to_directory(
                    nested_member_path,
                    current_nested_member_path,
                )
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
        LogFileSource::MaterializedArchiveMember {
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
        LogFileSource::NestedArchiveMember {
            outer_archive_path,
            archive_member_path,
            nested_member_path,
            ..
        } => {
            let archive_name = outer_archive_path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| outer_archive_path.display().to_string());
            let nested_parent = archive_member_parent(nested_member_path);
            nested_parent
                .map(|parent| format!("{archive_name}/{archive_member_path}/{parent}"))
                .unwrap_or_else(|| format!("{archive_name}/{archive_member_path}"))
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
            temporary_paths: Vec::new(),
        }
    }

    /// 普通文本搜索默认支持不区分大小写匹配。
    #[test]
    fn 普通文本搜索支持不区分大小写() {
        let source = local("/tmp/app/access.log");
        let lines = vec!["INFO started".to_string(), "error failed".to_string()];
        let results = search_lines(&source, &lines, &SearchOptions::single("ERROR", false));

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].line_index, 1);
        assert_eq!(results[0].match_range, 0..5);
    }

    /// 普通文本查询词会保留非空首尾空格，避免核心搜索悄悄改变调用方传入的精确文本。
    #[test]
    fn 普通文本搜索保留查询词首尾空格() {
        let source = local("/tmp/app/access.log");
        let lines = vec![
            "prefix error suffix".to_string(),
            "error suffix".to_string(),
        ];
        let results = search_lines(&source, &lines, &SearchOptions::single(" error ", true));

        assert_eq!(results.len(), 1);
        assert_eq!(
            &results[0].line_text[results[0].match_range.clone()],
            " error "
        );
    }

    /// 区分大小写开启后只接受完全一致的文本。
    #[test]
    fn 区分大小写搜索只匹配原始大小写() {
        let source = local("/tmp/app/access.log");
        let lines = vec!["error lower".to_string(), "ERROR upper".to_string()];
        let results = search_lines(&source, &lines, &SearchOptions::single("ERROR", true));

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].line_index, 1);
    }

    /// 中文查询词命中范围必须保持 UTF-8 字节边界，供 `StyledText` 安全高亮。
    #[test]
    fn 中文查询词返回合法_utf8_范围() {
        let source = local("/tmp/app/access.log");
        let lines = vec!["服务启动失败".to_string()];
        let results = search_lines(&source, &lines, &SearchOptions::single("启动", false));

        let range = results[0].match_range.clone();
        assert!(results[0].line_text.is_char_boundary(range.start));
        assert!(results[0].line_text.is_char_boundary(range.end));
        assert_eq!(&results[0].line_text[range], "启动");
    }

    /// 当前文件计数会累计同一行内的多次命中。
    ///
    /// 业务意图：
    /// - 搜索窗口“计数”按钮展示的是关键字出现次数，而不是搜索结果面板当前使用的命中行数。
    /// - 同一行多次出现需要全部计入，才能让用户正确判断当前文件的匹配规模。
    #[test]
    fn 当前文件计数累计同一行多次命中() {
        let lines = vec![
            "error error".to_string(),
            "ERROR once".to_string(),
            "ok".to_string(),
        ];

        assert_eq!(
            count_query_occurrences(&lines, &SearchOptions::single("error", false),),
            3
        );
        assert_eq!(
            count_query_occurrences(&lines, &SearchOptions::single("error", true),),
            2
        );
    }

    /// 正则搜索按单行返回第一个命中范围，并使用 UTF-8 原始字节下标供 UI 高亮。
    #[test]
    fn 正则搜索返回首个命中和原始字节范围() {
        let source = local("/tmp/app/access.log");
        let lines = vec![
            "INFO id=42 path=/ok".to_string(),
            "WARN id=100 path=/retry".to_string(),
        ];
        let options = SearchOptions::single_with_mode(r"id=\d+", false, SearchMatchMode::Regex);

        let results = search_lines(&source, &lines, &options);

        assert_eq!(results.len(), 2);
        assert_eq!(
            &results[0].line_text[results[0].match_range.clone()],
            "id=42"
        );
        assert_eq!(
            &results[1].line_text[results[1].match_range.clone()],
            "id=100"
        );
    }

    /// 正则计数使用 `find_iter` 的非重叠语义，和普通文本计数的片段出现次数保持同一层级。
    #[test]
    fn 正则计数统计非重叠命中次数() {
        let lines = vec!["id=42 id=100".to_string(), "no id".to_string()];
        let options = SearchOptions::single_with_mode(r"id=\d+", true, SearchMatchMode::Regex);

        assert_eq!(count_query_occurrences(&lines, &options), 2);
    }

    /// 正则模式下大小写由表达式控制，普通“区分大小写”开关不会改变结果。
    #[test]
    fn 正则大小写由表达式控制并忽略普通大小写开关() {
        let source = local("/tmp/app/access.log");
        let lines = vec!["error lower".to_string(), "ERROR upper".to_string()];
        let insensitive =
            SearchOptions::single_with_mode(r"(?i)error", true, SearchMatchMode::Regex);
        let sensitive = SearchOptions::single_with_mode(r"error", false, SearchMatchMode::Regex);

        assert_eq!(search_lines(&source, &lines, &insensitive).len(), 2);
        assert_eq!(search_lines(&source, &lines, &sensitive).len(), 1);
    }

    /// 无效正则必须在启动搜索前暴露中文错误，避免后台任务生成误导性的空结果。
    #[test]
    fn 无效正则返回中文校验错误() {
        let options = SearchOptions::single_with_mode("[", false, SearchMatchMode::Regex);
        let error = options.validate().expect_err("损坏正则应返回错误");

        assert!(error.to_string().contains("正则表达式无效"));
    }

    /// 正则命中中文等多字节字符时，范围仍必须是合法 UTF-8 边界。
    #[test]
    fn 正则中文命中范围保持_utf8_边界() {
        let source = local("/tmp/app/access.log");
        let lines = vec!["服务启动失败 code=500".to_string()];
        let options = SearchOptions::single_with_mode(r"启动失败", true, SearchMatchMode::Regex);

        let results = search_lines(&source, &lines, &options);
        let range = results[0].match_range.clone();

        assert!(results[0].line_text.is_char_boundary(range.start));
        assert!(results[0].line_text.is_char_boundary(range.end));
        assert_eq!(&results[0].line_text[range], "启动失败");
    }

    /// 快搜构造始终保持普通文本多关键字模式，不受普通搜索窗口“正则”开关影响。
    #[test]
    fn 快搜选项始终为普通文本模式() {
        let options = SearchOptions::any(vec!["ERROR".to_string(), "WARN".to_string()], false);

        assert_eq!(options.match_mode, SearchMatchMode::Literal);
    }

    /// 多关键字搜索按任一关键字命中，并且同一行只返回一条结果。
    #[test]
    fn 多关键字搜索任一命中且同一行只返回一条结果() {
        let source = local("/tmp/app/access.log");
        let lines = vec![
            "INFO ok".to_string(),
            "WARN retry then ERROR failed".to_string(),
            "Timeout waiting".to_string(),
        ];
        let results = search_lines(
            &source,
            &lines,
            &SearchOptions::any(vec!["ERROR".to_string(), "Timeout".to_string()], false),
        );

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].line_index, 1);
        assert_eq!(
            &results[0].line_text[results[0].match_range.clone()],
            "ERROR"
        );
        assert_eq!(results[1].line_index, 2);
    }

    /// 多关键字同时命中一行时高亮最靠前位置；位置相同则保留配置顺序。
    #[test]
    fn 多关键字搜索高亮最靠前命中并按配置顺序打破平局() {
        let source = local("/tmp/app/access.log");
        let lines = vec![
            "prefix ERROR Timeout".to_string(),
            "Exception happens".to_string(),
        ];
        let first_results = search_lines(
            &source,
            &lines,
            &SearchOptions::any(vec!["Timeout".to_string(), "ERROR".to_string()], true),
        );
        assert_eq!(
            &first_results[0].line_text[first_results[0].match_range.clone()],
            "ERROR"
        );

        let tie_results = search_lines(
            &source,
            &lines,
            &SearchOptions::any(vec!["Exception".to_string(), "Ex".to_string()], true),
        );
        assert_eq!(
            &tie_results[0].line_text[tie_results[0].match_range.clone()],
            "Exception"
        );
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
