//! 泛微日志插件 sidecar。
//!
//! 业务意图：
//! - 该二进制作为独立插件工程编译和打包，由宿主通过 stdin/stdout 传递 JSON 请求和响应。
//! - “性能列表解析”先使用宿主传入的文件元数据解析文件名；用户点击“显示SQL”后才读取对应单个日志正文。
//! - “泛微日志分析”由日志工具栏触发，当前阶段只扫描宿主授权日志树快照，按规则列出匹配路径和数量。
//!
//! 边界条件：
//! - 只接收严格六段文件名：`耗时&用户&地址&时间戳&字段5&字段6.log`。
//! - 耗时必须为毫秒数字，时间戳必须为 13 位毫秒数字；用户名允许为空，其余格式错误的文件直接跳过。
//! - SQL 明细只读取宿主传入的 `read_path`，不会扫描任意目录；正文按 UTF-8 有损转换逐行解析，避免异常编码中断插件。
//! - 泛微日志分析规则只使用相对路径通配匹配；日期 token 只校验数字位数，不校验真实日期是否合法。

use std::{
    cmp::Ordering,
    collections::BTreeMap,
    fs::File,
    io::{self, BufRead, BufReader},
};

use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};

/// 插件请求。
#[derive(Debug, Deserialize)]
struct PluginCommandRequest {
    /// 协议版本。
    api_version: u32,
    /// 命令 ID。
    command_id: String,
    /// 命令上下文。
    context: PluginCommandContext,
}

/// 插件上下文。
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PluginCommandContext {
    /// 日志树右键菜单上下文。
    LogTreeMenu {
        /// 候选日志元数据。
        files: Vec<PluginLogFile>,
    },
    /// 日志工具栏按钮上下文。
    LogToolbarAction {
        /// 工具栏贡献点 ID。
        toolbar_id: String,
        /// 当前左侧日志树快照。
        files: Vec<PluginLogFile>,
        /// 宿主合并默认值和用户保存值后的规则配置。
        #[serde(default)]
        settings: BTreeMap<String, String>,
    },
    /// 表格行按钮针对单个日志文件发起的二次命令。
    LogFileAction {
        /// 行动作 ID。
        action_id: String,
        /// 当前请求对应的日志文件。
        file: PluginLogFile,
        /// 额外业务字段。
        #[serde(default)]
        data: BTreeMap<String, String>,
    },
    /// 其它上下文当前不处理。
    #[serde(other, skip_serializing)]
    Unsupported,
}

/// 宿主传入的日志元数据。
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct PluginLogFile {
    /// 宿主侧日志来源稳定键，用于行内延迟命令回传给宿主定位原始来源。
    #[serde(default)]
    source_key: String,
    /// 展示文件名。
    display_name: String,
    /// 展示路径或来源描述。
    #[serde(default)]
    path_label: String,
    /// 来源类型。
    #[serde(default)]
    source_kind: String,
    /// 左侧树节点类型。
    #[serde(default)]
    node_kind: String,
    /// 宿主授权插件读取的本地路径。
    #[serde(default)]
    read_path: Option<String>,
}

/// 插件响应。
#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum PluginCommandResponse {
    /// 打开声明式窗口。
    OpenWindow {
        /// 窗口标题。
        title: String,
        /// 页面内容。
        page: PluginPage,
    },
    /// 返回错误。
    Error {
        /// 错误内容。
        message: String,
    },
}

/// 插件流式事件。
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum PluginCommandEvent {
    /// 处理进度。
    Progress {
        /// 进度快照。
        progress: PluginCommandProgress,
    },
}

/// 插件处理进度。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct PluginCommandProgress {
    /// 当前阶段主文案。
    message: String,
    /// 当前阶段补充文案。
    detail: Option<String>,
    /// 已完成数量。
    done: u64,
    /// 总数量。
    total: Option<u64>,
    /// 数量单位。
    unit: Option<String>,
}

/// 声明式页面。
#[derive(Debug, Serialize)]
struct PluginPage {
    /// 页面标题。
    title: String,
    /// 页面说明。
    description: Option<String>,
    /// 统计摘要。
    stats: Vec<PluginPageStat>,
    /// 结果表格。
    table: Option<PluginPageTable>,
}

/// 页面统计项。
#[derive(Debug, Serialize)]
struct PluginPageStat {
    /// 统计名。
    label: String,
    /// 统计值。
    value: String,
}

/// 页面表格。
#[derive(Debug, Serialize)]
struct PluginPageTable {
    /// 表头。
    headers: Vec<String>,
    /// 行。
    rows: Vec<Vec<String>>,
    /// 每行对应的可点击动作；旧宿主忽略未知字段，新宿主会渲染为按钮。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    row_actions: Vec<Vec<PluginTableRowAction>>,
}

/// 页面表格行内动作。
#[derive(Debug, Serialize)]
struct PluginTableRowAction {
    /// 按钮文案。
    label: String,
    /// 新窗口标题。
    title: Option<String>,
    /// 点击后直接打开的页面。
    #[serde(skip_serializing_if = "Option::is_none")]
    page: Option<Box<PluginPage>>,
    /// 点击后延迟执行的插件命令。
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<PluginTableRowCommand>,
}

/// 页面表格行内延迟命令。
#[derive(Debug, Serialize)]
struct PluginTableRowCommand {
    /// 命令 ID。
    command_id: String,
    /// 命令上下文。
    context: PluginCommandContext,
}

/// 宿主写入 stdin 的日志内容流事件。
#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum PluginStdinEvent {
    /// 一行日志正文。
    LogContentLine {
        /// 已去掉换行符的日志行文本。
        line: String,
    },
    /// 其它事件当前忽略，保证未来宿主扩展时旧插件不崩溃。
    #[serde(other)]
    Unsupported,
}

/// 泛微性能日志文件名解析结果。
#[derive(Clone, Debug, PartialEq, Eq)]
struct WeaverPerformanceLog {
    /// 请求耗时，单位毫秒。
    elapsed_ms: u64,
    /// 请求用户名，允许为空。
    user: String,
    /// 请求地址。
    route: String,
    /// 请求时间戳，毫秒，用于排序和兜底展示。
    timestamp_ms: i64,
    /// 请求时间的本机可读展示文本。
    request_time: String,
    /// 原始文件名。
    file_name: String,
    /// 宿主传入的原始日志文件快照。
    source: PluginLogFile,
}

/// 泛微 SQL 明细行。
#[derive(Clone, Debug, PartialEq, Eq)]
struct WeaverSqlLog {
    /// SQL 总耗时，单位毫秒。
    total_ms: u64,
    /// 解析结果集耗时，单位毫秒。
    result_parse_ms: u64,
    /// 获取连接耗时，单位毫秒。
    connection_ms: u64,
    /// 事务提交耗时，单位毫秒。
    commit_ms: u64,
    /// 释放连接耗时，单位毫秒。
    release_ms: u64,
    /// SQL 文本。
    sql: String,
}

/// 按请求地址聚合后的性能摘要。
struct WeaverRouteSummary {
    /// 请求地址。
    route: String,
    /// 当前请求地址的全部请求明细。
    logs: Vec<WeaverPerformanceLog>,
}

/// 泛微日志类型默认匹配规则。
///
/// 业务意图：
/// - 这些规则对应泛微现场常见日志和配置文件，设置页可以覆盖每一项，但插件必须保留默认规则兜底。
/// - `default_patterns` 使用分号分隔多个 glob，与宿主设置页保存格式保持一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WeaverLogRule {
    /// 设置键和统计稳定键。
    key: &'static str,
    /// 用户可见日志类型。
    label: &'static str,
    /// 默认 glob 规则。
    default_patterns: &'static str,
}

/// 14 类泛微日志默认规则。
const WEAVER_LOG_RULES: &[WeaverLogRule] = &[
    WeaverLogRule {
        key: "memory",
        label: "内存日志",
        default_patterns: "memory_yyyy-MM-dd.log",
    },
    WeaverLogRule {
        key: "pool",
        label: "连接池日志",
        default_patterns: "pool_yyyyMMdd_ecology.log",
    },
    WeaverLogRule {
        key: "init_cache",
        label: "sql缓存配置文件",
        default_patterns: "initCache.properties",
    },
    WeaverLogRule {
        key: "resin3",
        label: "resin3配置文件",
        default_patterns: "resin.conf",
    },
    WeaverLogRule {
        key: "resin4",
        label: "resin4配置文件",
        default_patterns: "resin.properties",
    },
    WeaverLogRule {
        key: "web_xml",
        label: "web.xml配置文件",
        default_patterns: "web.xml",
    },
    WeaverLogRule {
        key: "ecology",
        label: "ecology日志",
        default_patterns: "ecology;ecology_yyyyMMdd.log",
    },
    WeaverLogRule {
        key: "stdout",
        label: "中间件标准输出日志",
        default_patterns: "stdout.log;stdout.*.log",
    },
    WeaverLogRule {
        key: "stderr",
        label: "中间件标准错误日志",
        default_patterns: "stderr.log;stderr.*.log",
    },
    WeaverLogRule {
        key: "jvm_app",
        label: "中间件启动日志",
        default_patterns: "jvm-app-0.log",
    },
    WeaverLogRule {
        key: "monitor_thread_40s",
        label: "40秒线程日志",
        default_patterns: "monitorThread/yyyyMMdd/thread_HHmmss.log;monitorThread/yyyyMMdd/thread_HHmmss.zip",
    },
    WeaverLogRule {
        key: "thread_3m",
        label: "3分钟线程日志",
        default_patterns: "yyyy-MM-dd/thread_HHmmss.log;yyyy-MM-dd/thread_HHmmss.zip",
    },
    WeaverLogRule {
        key: "messages",
        label: "服务器日志",
        default_patterns: "messages",
    },
    WeaverLogRule {
        key: "runtime",
        label: "runtime日志",
        default_patterns: "runtime/yyyy_MM_dd/**",
    },
];

impl WeaverRouteSummary {
    /// 请求出现次数。
    fn count(&self) -> usize {
        self.logs.len()
    }

    /// 总耗时，单位毫秒。
    fn total_elapsed_ms(&self) -> u128 {
        self.logs.iter().map(|log| u128::from(log.elapsed_ms)).sum()
    }

    /// 平均耗时，单位毫秒。
    fn average_elapsed_ms(&self) -> f64 {
        let count = self.count();
        if count == 0 {
            0.0
        } else {
            self.total_elapsed_ms() as f64 / count as f64
        }
    }
}

fn main() {
    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let response =
        match read_request(&mut reader).and_then(|request| handle_request(request, &mut reader)) {
            Ok(response) => response,
            Err(message) => PluginCommandResponse::Error { message },
        };
    match serde_json::to_string(&response) {
        Ok(raw) => println!("{raw}"),
        Err(error) => println!(
            "{{\"action\":\"error\",\"message\":\"序列化插件响应失败：{}\"}}",
            error
        ),
    }
}

/// 从 stdin 读取宿主请求。
fn read_request<R: BufRead>(reader: &mut R) -> Result<PluginCommandRequest, String> {
    let mut raw = String::new();
    reader
        .read_line(&mut raw)
        .map_err(|error| format!("读取宿主请求失败：{error}"))?;
    if raw.trim().is_empty() {
        return Err("宿主请求为空".to_string());
    }
    serde_json::from_str(&raw).map_err(|error| format!("解析宿主请求失败：{error}"))
}

/// 分发插件命令。
fn handle_request<R: BufRead>(
    request: PluginCommandRequest,
    content_reader: &mut R,
) -> Result<PluginCommandResponse, String> {
    if request.api_version != 1 {
        return Err(format!("不支持的插件协议版本：{}", request.api_version));
    }
    match (request.command_id.as_str(), request.context) {
        ("performance_list_parse", PluginCommandContext::LogTreeMenu { files }) => {
            Ok(build_performance_list_response(files, emit_progress))
        }
        (
            "weaver_log_scan",
            PluginCommandContext::LogToolbarAction {
                toolbar_id,
                files,
                settings,
            },
        ) if toolbar_id == "weaver.log_scan" => Ok(build_weaver_log_scan_response(
            files,
            settings,
            emit_progress,
        )),
        (
            "show_request_sql",
            PluginCommandContext::LogFileAction {
                action_id,
                file,
                data,
            },
        ) if action_id == "show_sql" => Ok(build_sql_list_response(
            file,
            data,
            content_reader,
            emit_progress,
        )),
        ("show_request_sql", _) => Err("显示 SQL 只能从请求详情页的按钮触发".to_string()),
        ("performance_list_parse", PluginCommandContext::Unsupported) => {
            Err("性能列表解析只能从日志目录树右键菜单触发".to_string())
        }
        ("weaver_log_scan", _) => Err("泛微日志分析只能从日志分析页工具栏触发".to_string()),
        (command_id, _) => Err(format!("未知插件命令：{command_id}")),
    }
}

/// 构造泛微日志分析扫描窗口响应。
///
/// 业务意图：
/// - 当前阶段只完成“找到了哪些泛微相关日志”的可视化清单，不读取日志正文做业务诊断。
/// - 匹配顺序按规则定义顺序和相对路径排序，保证同一批日志每次打开窗口时结果稳定。
fn build_weaver_log_scan_response<F>(
    files: Vec<PluginLogFile>,
    settings: BTreeMap<String, String>,
    mut report_progress: F,
) -> PluginCommandResponse
where
    F: FnMut(PluginCommandProgress),
{
    let scanned_count = files.len();
    report_progress(PluginCommandProgress {
        message: "正在扫描泛微日志路径".to_string(),
        detail: Some(format!("共收到 {scanned_count} 个日志树条目")),
        done: 0,
        total: Some(scanned_count as u64),
        unit: Some("条目".to_string()),
    });

    let mut rows: Vec<(usize, String, String)> = Vec::new();
    let mut counts = vec![0usize; WEAVER_LOG_RULES.len()];
    for (file_index, file) in files.into_iter().enumerate() {
        let relative_path = plugin_file_relative_path(&file);
        for (rule_index, rule) in WEAVER_LOG_RULES.iter().enumerate() {
            let patterns = settings
                .get(rule.key)
                .map(String::as_str)
                .unwrap_or(rule.default_patterns);
            if weaver_rule_matches(patterns, &relative_path) {
                counts[rule_index] += 1;
                rows.push((rule_index, rule.label.to_string(), relative_path.clone()));
            }
        }
        let done = file_index + 1;
        if should_report_progress(done, scanned_count) {
            report_progress(PluginCommandProgress {
                message: "正在扫描泛微日志路径".to_string(),
                detail: Some(relative_path),
                done: done as u64,
                total: Some(scanned_count as u64),
                unit: Some("条目".to_string()),
            });
        }
    }

    rows.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.2.cmp(&right.2))
            .then_with(|| left.1.cmp(&right.1))
    });
    let table_rows = rows
        .into_iter()
        .map(|(_, label, path)| vec![label, path])
        .collect::<Vec<_>>();
    let matched_count = table_rows.len();
    let mut stats = vec![
        PluginPageStat {
            label: "扫描条目".to_string(),
            value: scanned_count.to_string(),
        },
        PluginPageStat {
            label: "匹配日志".to_string(),
            value: matched_count.to_string(),
        },
    ];
    for (rule, count) in WEAVER_LOG_RULES.iter().zip(counts.iter()) {
        stats.push(PluginPageStat {
            label: rule.label.to_string(),
            value: count.to_string(),
        });
    }

    PluginCommandResponse::OpenWindow {
        title: "泛微日志分析".to_string(),
        page: PluginPage {
            title: "泛微日志分析".to_string(),
            description: if matched_count == 0 {
                Some("当前日志树快照中没有匹配到泛微日志类型。".to_string())
            } else {
                Some("当前阶段仅展示按规则扫描到的日志类型和相对路径。".to_string())
            },
            stats,
            table: Some(PluginPageTable {
                headers: vec!["日志类型".to_string(), "相对路径".to_string()],
                rows: table_rows,
                row_actions: Vec::new(),
            }),
        },
    }
}

/// 返回插件日志快照中用于规则匹配的相对路径。
///
/// 边界条件：
/// - 工具栏上下文由宿主把 `path_label` 填成相对路径；旧宿主或测试缺省时回退到文件名。
/// - Windows 路径分隔符统一成 `/`，保证同一规则跨平台可用。
fn plugin_file_relative_path(file: &PluginLogFile) -> String {
    let raw = if file.path_label.trim().is_empty() {
        file.display_name.as_str()
    } else {
        file.path_label.as_str()
    };
    normalize_weaver_path(raw)
}

/// 判断一组分号分隔规则是否命中指定相对路径。
fn weaver_rule_matches(patterns: &str, relative_path: &str) -> bool {
    patterns
        .split(';')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| weaver_pattern_matches(pattern, relative_path))
}

/// 判断单条 glob 规则是否命中相对路径。
///
/// 业务意图：
/// - 无 `/` 的规则按文件名匹配，便于 `web.xml`、`messages` 这类文件在任意目录出现时都能命中。
/// - 含 `/` 的规则按归一化相对路径匹配，同时允许规则出现在任意子目录下，避免用户必须知道加载根目录名称。
fn weaver_pattern_matches(pattern: &str, relative_path: &str) -> bool {
    let pattern = normalize_weaver_path(pattern);
    let relative_path = normalize_weaver_path(relative_path);
    if !pattern.contains('/') {
        let basename = relative_path
            .rsplit('/')
            .next()
            .unwrap_or(relative_path.as_str());
        return glob_pattern_matches(&pattern, basename);
    }

    for candidate_path in path_variants_for_matching(&relative_path) {
        if path_suffixes_for_matching(&candidate_path)
            .any(|candidate| glob_pattern_matches(&pattern, candidate))
        {
            return true;
        }
    }
    false
}

/// 生成全路径和每一级子路径后缀，支持规则在任意子目录下命中。
fn path_suffixes_for_matching(path: &str) -> impl Iterator<Item = &str> {
    std::iter::once(path).chain(path.match_indices('/').map(|(index, _)| &path[index + 1..]))
}

/// 生成规则匹配时使用的路径变体。
///
/// 业务意图：
/// - 宿主会用 `archive.zip!/inner.log` 展示压缩包链路，但业务规则描述的是解压后的日志目录结构。
/// - 路径型规则需要同时尝试原始链路和去掉压缩包容器段后的链路，让
///   `monitorThread/yyyyMMdd/thread_HHmmss.log` 可以命中
///   `thread_000038.zip!/thread_000038.log`。
fn path_variants_for_matching(path: &str) -> Vec<String> {
    let mut variants = vec![path.to_string()];
    let transparent = path
        .split('/')
        .filter(|segment| !is_archive_chain_segment(segment))
        .collect::<Vec<_>>()
        .join("/");
    if transparent != path && !transparent.is_empty() {
        variants.push(transparent);
    }
    let existing = variants.clone();
    for variant in existing {
        if let Some(compressed_log_variant) = compressed_log_path_variant(&variant)
            && !variants.contains(&compressed_log_variant)
        {
            variants.push(compressed_log_variant);
        }
    }
    variants
}

/// 判断路径片段是否是宿主展示压缩包链路时生成的容器段。
fn is_archive_chain_segment(segment: &str) -> bool {
    let lower = segment.to_ascii_lowercase();
    lower.ends_with(".zip!")
        || lower.ends_with(".rar!")
        || lower.ends_with(".7z!")
        || lower.ends_with(".tar!")
        || lower.ends_with(".gz!")
        || lower.ends_with(".tgz!")
        || lower.ends_with(".tar.gz!")
}

/// 为压缩日志文件生成“解压后日志名”的匹配变体。
///
/// 业务意图：
/// - 现场 `monitorThread/yyyyMMdd/thread_HHmmss.zip` 本身就是线程日志的压缩形态；
///   用户如果在设置中仍保存旧的 `.log` 规则，也应把它当作同名 `.log` 命中。
/// - 这里只改变规则匹配用的虚拟路径，不改变窗口展示的真实相对路径，避免用户看不到原始 zip 文件。
///
/// 边界条件：
/// - 仅处理常见单文件日志压缩后缀；普通目录压缩包内部的其它日志仍由宿主快照展开后匹配。
/// - `.tar.gz` 和 `.tgz` 这类目录包通常不是单个线程日志文件，不在这里伪装成 `.log`。
fn compressed_log_path_variant(path: &str) -> Option<String> {
    let lower_path = path.to_ascii_lowercase();
    for suffix in [".zip", ".gz", ".gzip"] {
        if lower_path.ends_with(suffix) {
            let prefix = &path[..path.len().saturating_sub(suffix.len())];
            return Some(format!("{prefix}.log"));
        }
    }
    None
}

/// 归一化插件路径文本。
fn normalize_weaver_path(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect::<Vec<_>>()
        .join("/")
}

/// 执行 glob 和日期 token 匹配。
///
/// 边界条件：
/// - `*` 只匹配单级路径片段，`**` 可以跨 `/`，`?` 匹配单个非 `/` 字符。
/// - 日期 token 只校验数字位数；例如 `yyyyMMddHHmmss` 需要 14 位数字，但不校验真实日历日期。
fn glob_pattern_matches(pattern: &str, target: &str) -> bool {
    let pattern_chars = pattern.chars().collect::<Vec<_>>();
    let target_chars = target.chars().collect::<Vec<_>>();
    let mut memo = BTreeMap::<(usize, usize), bool>::new();
    glob_pattern_matches_inner(&pattern_chars, &target_chars, 0, 0, &mut memo)
}

/// 递归匹配 glob 模式。
fn glob_pattern_matches_inner(
    pattern: &[char],
    target: &[char],
    pattern_index: usize,
    target_index: usize,
    memo: &mut BTreeMap<(usize, usize), bool>,
) -> bool {
    if let Some(value) = memo.get(&(pattern_index, target_index)) {
        return *value;
    }
    let result = if pattern_index == pattern.len() {
        target_index == target.len()
    } else if pattern[pattern_index] == '*' {
        if pattern.get(pattern_index + 1) == Some(&'*') {
            glob_pattern_matches_inner(pattern, target, pattern_index + 2, target_index, memo)
                || (target_index < target.len()
                    && glob_pattern_matches_inner(
                        pattern,
                        target,
                        pattern_index,
                        target_index + 1,
                        memo,
                    ))
        } else {
            glob_pattern_matches_inner(pattern, target, pattern_index + 1, target_index, memo)
                || (target_index < target.len()
                    && target[target_index] != '/'
                    && glob_pattern_matches_inner(
                        pattern,
                        target,
                        pattern_index,
                        target_index + 1,
                        memo,
                    ))
        }
    } else if pattern[pattern_index] == '?' {
        target_index < target.len()
            && target[target_index] != '/'
            && glob_pattern_matches_inner(
                pattern,
                target,
                pattern_index + 1,
                target_index + 1,
                memo,
            )
    } else if let Some(width) = date_token_width(pattern, pattern_index) {
        target_index + width <= target.len()
            && target[target_index..target_index + width]
                .iter()
                .all(|ch| ch.is_ascii_digit())
            && glob_pattern_matches_inner(
                pattern,
                target,
                pattern_index + width,
                target_index + width,
                memo,
            )
    } else {
        target_index < target.len()
            && pattern[pattern_index] == target[target_index]
            && glob_pattern_matches_inner(
                pattern,
                target,
                pattern_index + 1,
                target_index + 1,
                memo,
            )
    };
    memo.insert((pattern_index, target_index), result);
    result
}

/// 识别日期时间 token 并返回需要匹配的数字位数。
fn date_token_width(pattern: &[char], index: usize) -> Option<usize> {
    if !date_token_boundary_before(pattern, index) {
        return None;
    }
    let remaining = &pattern[index..];
    if remaining.starts_with(&['y', 'y', 'y', 'y']) || remaining.starts_with(&['Y', 'Y', 'Y', 'Y'])
    {
        return Some(4);
    }
    for token in [
        ['M', 'M'],
        ['d', 'd'],
        ['D', 'D'],
        ['H', 'H'],
        ['m', 'm'],
        ['s', 's'],
    ] {
        if remaining.starts_with(&token) {
            return Some(2);
        }
    }
    None
}

/// 判断当前位置是否可能是日期 token 的起点。
///
/// 业务意图：
/// - 避免把普通单词里的 `ss` 误判为秒，例如服务器日志文件名 `messages` 必须按字面匹配。
/// - 连续日期 token 内部仍允许直接相邻，例如 `yyyyMMddHHmmss`。
fn date_token_boundary_before(pattern: &[char], index: usize) -> bool {
    if index == 0 {
        return true;
    }
    let previous = pattern[index - 1];
    matches!(previous, '/' | '_' | '-' | '.')
        || matches!(previous, 'y' | 'Y' | 'M' | 'd' | 'D' | 'H' | 'm' | 's')
}

/// 构造性能列表解析窗口响应。
fn build_performance_list_response<F>(
    files: Vec<PluginLogFile>,
    mut report_progress: F,
) -> PluginCommandResponse
where
    F: FnMut(PluginCommandProgress),
{
    let scanned_count = files.len();
    report_progress(PluginCommandProgress {
        message: "正在解析泛微性能日志文件名".to_string(),
        detail: Some(format!("共收到 {scanned_count} 个候选日志")),
        done: 0,
        total: Some(scanned_count as u64),
        unit: Some("文件".to_string()),
    });
    let mut skipped_count = 0usize;
    let mut rows = Vec::new();
    for (index, file) in files.into_iter().enumerate() {
        if let Some(row) = parse_weaver_performance_log(&file) {
            rows.push(row);
        } else {
            skipped_count += 1;
        }
        let done = index + 1;
        if should_report_progress(done, scanned_count) {
            report_progress(PluginCommandProgress {
                message: "正在解析泛微性能日志文件名".to_string(),
                detail: Some(file.display_name),
                done: done as u64,
                total: Some(scanned_count as u64),
                unit: Some("文件".to_string()),
            });
        }
    }
    report_progress(PluginCommandProgress {
        message: "正在整理性能列表结果".to_string(),
        detail: Some("按请求地址聚合并统计次数".to_string()),
        done: scanned_count as u64,
        total: Some(scanned_count as u64),
        unit: Some("文件".to_string()),
    });
    let matched_count = rows.len();
    let summaries = build_route_summaries(rows);
    let (table_rows, row_actions) = build_summary_table_rows(&summaries);
    let route_count = summaries.len();
    PluginCommandResponse::OpenWindow {
        title: "性能列表解析".to_string(),
        page: PluginPage {
            title: "性能列表解析".to_string(),
            description: if matched_count == 0 {
                Some("选中范围内没有符合泛微性能日志文件名格式的日志。".to_string())
            } else {
                Some("按请求地址合并统计次数和平均耗时，默认按请求次数降序排列。".to_string())
            },
            stats: vec![
                PluginPageStat {
                    label: "扫描文件".to_string(),
                    value: scanned_count.to_string(),
                },
                PluginPageStat {
                    label: "匹配文件".to_string(),
                    value: matched_count.to_string(),
                },
                PluginPageStat {
                    label: "跳过文件".to_string(),
                    value: skipped_count.to_string(),
                },
                PluginPageStat {
                    label: "请求地址".to_string(),
                    value: route_count.to_string(),
                },
            ],
            table: Some(PluginPageTable {
                headers: vec![
                    "请求地址".to_string(),
                    "请求次数".to_string(),
                    "平均耗时(ms)".to_string(),
                    "操作".to_string(),
                ],
                rows: table_rows,
                row_actions,
            }),
        },
    }
}

/// 构建按请求地址聚合后的摘要列表。
///
/// 业务意图：
/// - 泛微性能日志的请求地址来自文件名第三段；汇总页关注“哪个接口出现得最多、平均耗时如何”。
/// - 明细页仍保留每次请求的耗时、用户和时间，供用户从汇总行继续钻取。
fn build_route_summaries(logs: Vec<WeaverPerformanceLog>) -> Vec<WeaverRouteSummary> {
    let mut grouped: BTreeMap<String, Vec<WeaverPerformanceLog>> = BTreeMap::new();
    for log in logs {
        grouped.entry(log.route.clone()).or_default().push(log);
    }

    let mut summaries = grouped
        .into_iter()
        .map(|(route, mut logs)| {
            logs.sort_by(compare_detail_logs);
            WeaverRouteSummary { route, logs }
        })
        .collect::<Vec<_>>();
    summaries.sort_by(compare_route_summaries);
    summaries
}

/// 汇总行排序：请求次数降序，其次平均耗时降序，最后请求地址升序。
fn compare_route_summaries(left: &WeaverRouteSummary, right: &WeaverRouteSummary) -> Ordering {
    right
        .count()
        .cmp(&left.count())
        .then_with(|| {
            right
                .average_elapsed_ms()
                .partial_cmp(&left.average_elapsed_ms())
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| left.route.cmp(&right.route))
}

/// 明细行排序：耗时降序，其次时间戳升序，最后文件名升序。
fn compare_detail_logs(left: &WeaverPerformanceLog, right: &WeaverPerformanceLog) -> Ordering {
    right
        .elapsed_ms
        .cmp(&left.elapsed_ms)
        .then_with(|| left.timestamp_ms.cmp(&right.timestamp_ms))
        .then_with(|| left.file_name.cmp(&right.file_name))
}

/// 构建汇总表格行和对应详情动作。
fn build_summary_table_rows(
    summaries: &[WeaverRouteSummary],
) -> (Vec<Vec<String>>, Vec<Vec<PluginTableRowAction>>) {
    let mut table_rows = Vec::with_capacity(summaries.len());
    let mut row_actions = Vec::with_capacity(summaries.len());
    for summary in summaries {
        table_rows.push(vec![
            summary.route.clone(),
            summary.count().to_string(),
            format_average_elapsed_ms(summary.average_elapsed_ms()),
            String::new(),
        ]);
        row_actions.push(vec![PluginTableRowAction {
            label: "详情".to_string(),
            title: Some(format!("请求详情 - {}", summary.route)),
            page: Some(Box::new(build_detail_page(summary))),
            command: None,
        }]);
    }
    (table_rows, row_actions)
}

/// 构建某个请求地址的详情页。
///
/// 业务意图：
/// - 详情页展示该请求地址下的每一次请求，按耗时从高到低排列，便于定位慢请求对应用户和时间。
/// - SQL 明细通过行内延迟命令读取单个日志正文，不在首个响应中预先解析所有文件，避免大目录结果膨胀。
fn build_detail_page(summary: &WeaverRouteSummary) -> PluginPage {
    let mut detail_rows = Vec::with_capacity(summary.logs.len());
    let mut row_actions = Vec::with_capacity(summary.logs.len());
    for log in &summary.logs {
        detail_rows.push(vec![
            log.route.clone(),
            log.elapsed_ms.to_string(),
            log.user.clone(),
            log.request_time.clone(),
            String::new(),
        ]);
        if log.source.read_path.is_some() || !log.source.source_key.is_empty() {
            let mut data = BTreeMap::new();
            data.insert("route".to_string(), log.route.clone());
            data.insert("request_time".to_string(), log.request_time.clone());
            row_actions.push(vec![PluginTableRowAction {
                label: "显示SQL".to_string(),
                title: Some(format!("SQL - {}", log.route)),
                page: None,
                command: Some(PluginTableRowCommand {
                    command_id: "show_request_sql".to_string(),
                    context: PluginCommandContext::LogFileAction {
                        action_id: "show_sql".to_string(),
                        file: log.source.clone(),
                        data,
                    },
                }),
            }]);
        } else {
            row_actions.push(Vec::new());
        }
    }
    PluginPage {
        title: "请求详情".to_string(),
        description: Some(format!("请求地址：{}", summary.route)),
        stats: vec![
            PluginPageStat {
                label: "请求次数".to_string(),
                value: summary.count().to_string(),
            },
            PluginPageStat {
                label: "平均耗时(ms)".to_string(),
                value: format_average_elapsed_ms(summary.average_elapsed_ms()),
            },
        ],
        table: Some(PluginPageTable {
            headers: vec![
                "请求路径".to_string(),
                "耗时(ms)".to_string(),
                "用户".to_string(),
                "请求时间".to_string(),
                "操作".to_string(),
            ],
            rows: detail_rows,
            row_actions,
        }),
    }
}

/// 格式化平均耗时。
///
/// 边界条件：
/// - 平均值保留一位小数，避免整数除法吞掉小规模差异；非有限值回退为 `0.0`。
fn format_average_elapsed_ms(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.1}")
    } else {
        "0.0".to_string()
    }
}

/// 判断是否需要上报进度。
///
/// 业务意图：
/// - 大量文件时不能每个文件都刷新 UI，避免 stdout 和主程序轮询压力过高。
/// - 首个、最后一个和固定步长进度必须上报，保证用户能看到任务确实在推进。
fn should_report_progress(done: usize, total: usize) -> bool {
    done == 1 || done == total || done % 256 == 0
}

/// 输出插件进度事件。
///
/// 边界条件：
/// - 进度事件写入 stdout，与最终响应同为 JSON Lines；普通诊断日志不得写 stdout。
/// - 序列化失败时写入 stderr，但不终止解析，避免进度展示问题影响最终结果。
fn emit_progress(progress: PluginCommandProgress) {
    let event = PluginCommandEvent::Progress { progress };
    match serde_json::to_string(&event) {
        Ok(raw) => println!("{raw}"),
        Err(error) => eprintln!("序列化插件进度失败：{error}"),
    }
}

/// 从文件名解析泛微性能日志元数据。
fn parse_weaver_performance_log(file: &PluginLogFile) -> Option<WeaverPerformanceLog> {
    let file_name = file.display_name.strip_suffix(".log")?;
    let parts = file_name.split('&').collect::<Vec<_>>();
    if parts.len() != 6 {
        return None;
    }
    let elapsed_ms = parts[0].parse::<u64>().ok()?;
    let route_raw = parts[2];
    if route_raw.is_empty() {
        return None;
    }
    let timestamp_ms = parts[3];
    if timestamp_ms.len() != 13 || !timestamp_ms.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let timestamp_ms = timestamp_ms.parse::<i64>().ok()?;
    Some(WeaverPerformanceLog {
        elapsed_ms,
        user: parts[1].to_string(),
        route: route_raw.replace('_', "/"),
        timestamp_ms,
        request_time: format_request_time_ms(timestamp_ms),
        file_name: file.display_name.clone(),
        source: file.clone(),
    })
}

/// 将 13 位毫秒时间戳格式化为本机可读时间。
///
/// 业务意图：
/// - 泛微性能日志文件名只包含毫秒时间戳；结果窗口需要直接展示可读时间，减少人工换算成本。
/// - 使用本机时区符合桌面日志分析场景；如果时间戳超出 chrono 可表示范围，则回退显示原始毫秒值，避免插件报错中断。
fn format_request_time_ms(timestamp_ms: i64) -> String {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
        .unwrap_or_else(|| timestamp_ms.to_string())
}

/// 构造单个请求对应的 SQL 明细窗口响应。
///
/// 业务意图：
/// - SQL 明细只在用户点击详情行后读取对应单个日志，避免首次性能列表解析时读取上万份正文。
/// - 读取路径必须来自宿主传入的 `read_path`，插件不根据文件名或目录自行推导其它路径。
fn build_sql_list_response<F>(
    file: PluginLogFile,
    data: BTreeMap<String, String>,
    content_reader: &mut dyn BufRead,
    mut report_progress: F,
) -> PluginCommandResponse
where
    F: FnMut(PluginCommandProgress),
{
    let route = data.get("route").cloned().unwrap_or_default();

    report_progress(PluginCommandProgress {
        message: "正在读取 SQL 明细".to_string(),
        detail: Some(file.display_name.clone()),
        done: 0,
        total: None,
        unit: Some("行".to_string()),
    });

    let read_result = if let Some(read_path) = file.read_path.clone() {
        read_sql_logs_from_path(&read_path, |done| {
            if done == 1 || done % 2048 == 0 {
                report_progress(PluginCommandProgress {
                    message: "正在解析 SQL 明细".to_string(),
                    detail: Some(format!("已扫描 {done} 行")),
                    done,
                    total: None,
                    unit: Some("行".to_string()),
                });
            }
        })
    } else {
        read_sql_logs_from_stdin_events(content_reader)
    };
    let (mut sql_logs, scanned_lines, skipped_lines) = match read_result {
        Ok(result) => result,
        Err(message) => return PluginCommandResponse::Error { message },
    };

    sql_logs.sort_by(compare_sql_logs);
    report_progress(PluginCommandProgress {
        message: "正在整理 SQL 明细".to_string(),
        detail: Some(format!("共解析 {} 条 SQL", sql_logs.len())),
        done: scanned_lines,
        total: Some(scanned_lines),
        unit: Some("行".to_string()),
    });

    let rows = sql_logs
        .iter()
        .map(|sql| {
            vec![
                sql.total_ms.to_string(),
                sql.result_parse_ms.to_string(),
                sql.connection_ms.to_string(),
                sql.commit_ms.to_string(),
                sql.release_ms.to_string(),
                sql.sql.clone(),
            ]
        })
        .collect::<Vec<_>>();

    PluginCommandResponse::OpenWindow {
        title: "SQL 明细".to_string(),
        page: PluginPage {
            title: "SQL 明细".to_string(),
            description: if rows.is_empty() {
                Some("当前请求日志中没有符合 SQL 明细格式的行。".to_string())
            } else {
                Some("按 SQL 总耗时从高到低排序。".to_string())
            },
            stats: vec![
                PluginPageStat {
                    label: "请求路径".to_string(),
                    value: if route.is_empty() {
                        "-".to_string()
                    } else {
                        route
                    },
                },
                PluginPageStat {
                    label: "扫描行".to_string(),
                    value: scanned_lines.to_string(),
                },
                PluginPageStat {
                    label: "SQL 条数".to_string(),
                    value: rows.len().to_string(),
                },
                PluginPageStat {
                    label: "跳过行".to_string(),
                    value: skipped_lines.to_string(),
                },
            ],
            table: Some(PluginPageTable {
                headers: vec![
                    "SQL总耗时(ms)".to_string(),
                    "解析结果集(ms)".to_string(),
                    "获取连接(ms)".to_string(),
                    "事务提交(ms)".to_string(),
                    "释放连接(ms)".to_string(),
                    "SQL文本".to_string(),
                ],
                rows,
                row_actions: Vec::new(),
            }),
        },
    }
}

/// 逐行读取并解析 SQL 明细。
///
/// 边界条件：
/// - 使用 `read_until` 加 UTF-8 有损转换，避免文件中夹杂非 UTF-8 字节时整个插件失败。
/// - 该函数只返回符合“5 个耗时列 + SQL 文本”格式的行；其它行计入跳过数量，不中断用户分析。
fn read_sql_logs_from_path<F>(
    read_path: &str,
    mut report_lines: F,
) -> Result<(Vec<WeaverSqlLog>, u64, u64), String>
where
    F: FnMut(u64),
{
    let file = File::open(read_path).map_err(|error| format!("打开日志正文失败：{error}"))?;
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    let mut sql_logs = Vec::new();
    let mut scanned_lines = 0u64;
    let mut skipped_lines = 0u64;

    loop {
        buffer.clear();
        let bytes = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|error| format!("读取日志正文失败：{error}"))?;
        if bytes == 0 {
            break;
        }
        scanned_lines += 1;
        let line = String::from_utf8_lossy(&buffer);
        let line = line.trim_end_matches(['\r', '\n']);
        if let Some(sql_log) = parse_sql_log_line(line) {
            sql_logs.push(sql_log);
        } else {
            skipped_lines += 1;
        }
        report_lines(scanned_lines);
    }

    Ok((sql_logs, scanned_lines, skipped_lines))
}

/// 从宿主 stdin 内容流中解析 SQL 明细。
///
/// 业务意图：
/// - 压缩包成员不需要先物化成本地临时文件；宿主把当前日志按行包装成 JSON Lines 事件，插件边读边解析。
/// - 插件只消费当前命令 stdin 中的内容事件，不根据文件名或路径自行扩大读取范围。
///
/// 边界条件：
/// - 非法事件行计入跳过数量，避免单行协议噪声导致整个 SQL 窗口失败。
/// - 该函数不在读取过程中向 stdout 输出进度，避免宿主仍在写 stdin 时 stdout 管道阻塞形成双向等待。
fn read_sql_logs_from_stdin_events(
    reader: &mut dyn BufRead,
) -> Result<(Vec<WeaverSqlLog>, u64, u64), String> {
    let mut raw = String::new();
    let mut sql_logs = Vec::new();
    let mut scanned_lines = 0u64;
    let mut skipped_lines = 0u64;

    loop {
        raw.clear();
        let bytes = reader
            .read_line(&mut raw)
            .map_err(|error| format!("读取宿主日志内容流失败：{error}"))?;
        if bytes == 0 {
            break;
        }
        match serde_json::from_str::<PluginStdinEvent>(&raw) {
            Ok(PluginStdinEvent::LogContentLine { line }) => {
                scanned_lines = scanned_lines.saturating_add(1);
                if let Some(sql_log) = parse_sql_log_line(&line) {
                    sql_logs.push(sql_log);
                } else {
                    skipped_lines = skipped_lines.saturating_add(1);
                }
            }
            Ok(PluginStdinEvent::Unsupported) | Err(_) => {
                skipped_lines = skipped_lines.saturating_add(1);
            }
        }
    }

    Ok((sql_logs, scanned_lines, skipped_lines))
}

/// 解析一行 SQL 明细。
///
/// 业务意图：
/// - 泛微 SQL 行前五列固定为毫秒耗时，后续剩余文本整体作为 SQL，不能按空格继续拆分。
/// - 耗时列必须严格以 `ms` 结尾且前缀为数字，防止普通日志行被误识别为 SQL。
fn parse_sql_log_line(line: &str) -> Option<WeaverSqlLog> {
    let (total_ms, rest) = take_ms_field(line)?;
    let (result_parse_ms, rest) = take_ms_field(rest)?;
    let (connection_ms, rest) = take_ms_field(rest)?;
    let (commit_ms, rest) = take_ms_field(rest)?;
    let (release_ms, rest) = take_ms_field(rest)?;
    let sql = rest.trim_start();
    if sql.is_empty() {
        return None;
    }
    Some(WeaverSqlLog {
        total_ms,
        result_parse_ms,
        connection_ms,
        commit_ms,
        release_ms,
        sql: sql.to_string(),
    })
}

/// 读取一个形如 `123ms` 的耗时字段。
fn take_ms_field(input: &str) -> Option<(u64, &str)> {
    let trimmed = input.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    let token_end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    let token = &trimmed[..token_end];
    let value = token.strip_suffix("ms")?.parse::<u64>().ok()?;
    Some((value, &trimmed[token_end..]))
}

/// SQL 明细排序：SQL 总耗时降序，其次 SQL 文本升序。
fn compare_sql_logs(left: &WeaverSqlLog, right: &WeaverSqlLog) -> Ordering {
    right
        .total_ms
        .cmp(&left.total_ms)
        .then_with(|| left.sql.cmp(&right.sql))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造文件元数据测试对象。
    fn file(name: &str) -> PluginLogFile {
        PluginLogFile {
            source_key: format!("local:{name}"),
            display_name: name.to_string(),
            path_label: "/tmp".to_string(),
            source_kind: "local_file".to_string(),
            node_kind: "file".to_string(),
            read_path: None,
        }
    }

    /// 构造带正文读取路径的文件元数据测试对象。
    fn readable_file(name: &str, read_path: &str) -> PluginLogFile {
        PluginLogFile {
            source_key: format!("local:{name}"),
            display_name: name.to_string(),
            path_label: "/tmp".to_string(),
            source_kind: "local_file".to_string(),
            node_kind: "file".to_string(),
            read_path: Some(read_path.to_string()),
        }
    }

    /// 构造带相对路径的日志树快照测试对象。
    fn file_at(path: &str) -> PluginLogFile {
        let display_name = path
            .rsplit(['/', '\\'])
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(path)
            .to_string();
        PluginLogFile {
            source_key: format!("local:{path}"),
            display_name,
            path_label: path.to_string(),
            source_kind: "local_file".to_string(),
            node_kind: "file".to_string(),
            read_path: None,
        }
    }

    #[test]
    fn 十四类默认规则分别命中示例路径() {
        let examples = [
            ("memory", "root/memory_2026-05-23.log"),
            ("pool", "root/pool_20260523_ecology.log"),
            ("init_cache", "root/initCache.properties"),
            ("resin3", "root/conf/resin.conf"),
            ("resin4", "root/conf/resin.properties"),
            ("web_xml", "root/WEB-INF/web.xml"),
            ("ecology", "root/ecology_20260523.log"),
            ("stdout", "root/stdout.20260523.log"),
            ("stderr", "root/stderr.20260523.log"),
            ("jvm_app", "root/jvm-app-0.log"),
            (
                "monitor_thread_40s",
                "root/monitorThread/20260523/thread_101500.log",
            ),
            (
                "monitor_thread_40s",
                "root/monitorThread/20260523/thread_101500.zip",
            ),
            ("thread_3m", "root/2026-05-23/thread_101500.log"),
            ("thread_3m", "root/2026-05-23/thread_101500.zip"),
            ("messages", "root/messages"),
            ("runtime", "root/runtime/2026_05_23/ecology/runtime.log"),
        ];

        for (key, path) in examples {
            let rule = WEAVER_LOG_RULES
                .iter()
                .find(|rule| rule.key == key)
                .expect("测试规则必须存在");
            assert!(
                weaver_rule_matches(rule.default_patterns, path),
                "{key} 应命中 {path}"
            );
        }
    }

    #[test]
    fn glob_支持星号问号双星和日期数字形状() {
        assert!(weaver_pattern_matches(
            "stdout.*.log",
            "root/stdout.abc.log"
        ));
        assert!(weaver_pattern_matches(
            "thread_??????.log",
            "thread_101500.log"
        ));
        assert!(weaver_pattern_matches(
            "runtime/**",
            "root/a/runtime/2026_05_23/a.log"
        ));
        assert!(weaver_pattern_matches(
            "monitorThread/yyyyMMdd/thread_HHmmss.log",
            "root/monitorThread/20260523/thread_101500.log"
        ));
        assert!(weaver_pattern_matches(
            "monitorThread/yyyyMMdd/thread_HHmmss.log",
            "downLog.zip!/2026-05-21/monitorThread/20260521/thread_000038.zip!/thread_000038.log"
        ));
        assert!(weaver_pattern_matches(
            "monitorThread/yyyyMMdd/thread_HHmmss.log",
            "downLog.zip!/2026-05-21/monitorThread/20260521/thread_000038.zip"
        ));
        assert!(!weaver_pattern_matches(
            "monitorThread/yyyyMMdd/thread_HHmmss.log",
            "root/monitorThread/2026-05-23/thread_101500.log"
        ));
        assert!(weaver_pattern_matches("messages", "root/messages"));
        assert!(!weaver_pattern_matches("messages", "root/message12"));
    }

    #[test]
    fn 泛微扫描响应统计总数各类型数量且排序稳定() {
        let response = build_weaver_log_scan_response(
            vec![
                file_at("root/stdout.20260523.log"),
                file_at("root/memory_2026-05-23.log"),
                file_at("root/WEB-INF/web.xml"),
                file_at("root/runtime/2026_05_23/a.log"),
            ],
            BTreeMap::new(),
            |_| {},
        );
        let PluginCommandResponse::OpenWindow { page, .. } = response else {
            panic!("应返回泛微日志分析窗口");
        };
        let table = page.table.expect("扫描结果应包含表格");

        assert_eq!(page.stats[0].value, "4");
        assert_eq!(page.stats[1].value, "4");
        assert_eq!(
            table.headers,
            vec!["日志类型".to_string(), "相对路径".to_string()]
        );
        assert_eq!(table.rows.len(), 4);
        assert_eq!(table.rows[0][0], "内存日志");
        assert_eq!(table.rows[1][0], "web.xml配置文件");
        assert_eq!(table.rows[2][0], "中间件标准输出日志");
        assert_eq!(table.rows[3][0], "runtime日志");
    }

    #[test]
    fn 严格解析六段泛微性能日志文件名() {
        let parsed = parse_weaver_performance_log(&file(
            "1501&chenling&_openapi_Receive_HistoryWaterBill&1778661627833&0&0.log",
        ))
        .expect("合法文件名应能解析");
        assert_eq!(parsed.elapsed_ms, 1501);
        assert_eq!(parsed.user, "chenling");
        assert_eq!(parsed.route, "/openapi/Receive/HistoryWaterBill");
        assert_eq!(parsed.timestamp_ms, 1778661627833);
        assert!(parsed.request_time.contains('-'));
        assert!(parsed.request_time.contains(':'));
    }

    #[test]
    fn 用户名为空仍可解析() {
        let parsed = parse_weaver_performance_log(&file(
            "1741&&_openapi_Receive_HistoryWaterBill&1778669119290&0&0.log",
        ))
        .expect("用户名为空仍是合法格式");
        assert_eq!(parsed.user, "");
    }

    #[test]
    fn 非严格格式会跳过() {
        assert!(parse_weaver_performance_log(&file("bad.log")).is_none());
        assert!(
            parse_weaver_performance_log(&file("bad&user&_openapi_Test&1778661627833&0&0.log",))
                .is_none()
        );
        assert!(
            parse_weaver_performance_log(&file("1501&user&_openapi_Test&123&0&0.log")).is_none()
        );
        assert!(
            parse_weaver_performance_log(&file(
                "1501&user&_openapi_Test&1778661627833&0&0&extra.log",
            ))
            .is_none()
        );
    }

    #[test]
    fn 汇总列表按请求次数降序并计算平均耗时() {
        let response = build_performance_list_response(
            vec![
                file("100&u&_a&1778661627833&0&0.log"),
                file("300&v&_b&1778661627834&0&0.log"),
                file("500&w&_a&1778661627835&0&0.log"),
            ],
            |_| {},
        );
        let PluginCommandResponse::OpenWindow { page, .. } = response else {
            panic!("应返回窗口");
        };
        let table = page.table.unwrap();
        let rows = table.rows;
        assert_eq!(
            table.headers,
            vec![
                "请求地址".to_string(),
                "请求次数".to_string(),
                "平均耗时(ms)".to_string(),
                "操作".to_string()
            ]
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].len(), 4);
        assert_eq!(rows[0][0], "/a");
        assert_eq!(rows[0][1], "2");
        assert_eq!(rows[0][2], "300.0");
        assert_eq!(rows[1][0], "/b");
        assert_eq!(table.row_actions.len(), 2);
        assert_eq!(table.row_actions[0][0].label, "详情");
    }

    #[test]
    fn 详情页按耗时降序并显示用户() {
        let response = build_performance_list_response(
            vec![
                file("100&alice&_a&1778661627833&0&0.log"),
                file("500&bob&_a&1778661627834&0&0.log"),
            ],
            |_| {},
        );
        let PluginCommandResponse::OpenWindow { page, .. } = response else {
            panic!("应返回窗口");
        };
        let table = page.table.unwrap();
        let detail_page = table.row_actions[0][0]
            .page
            .as_ref()
            .expect("详情动作应携带静态页面");
        let detail_table = detail_page.table.as_ref().expect("详情页应包含表格");

        assert_eq!(
            detail_table.headers,
            vec![
                "请求路径".to_string(),
                "耗时(ms)".to_string(),
                "用户".to_string(),
                "请求时间".to_string(),
                "操作".to_string()
            ]
        );
        assert_eq!(detail_table.rows[0][0], "/a");
        assert_eq!(detail_table.rows[0][1], "500");
        assert_eq!(detail_table.rows[0][2], "bob");
        assert_eq!(detail_table.rows[1][1], "100");
        assert_eq!(detail_table.rows[1][2], "alice");
    }

    #[test]
    fn 请求详情为可读取日志生成显示_sql_延迟命令() {
        let response = build_performance_list_response(
            vec![readable_file(
                "100&alice&_a&1778661627833&0&0.log",
                "/tmp/a.log",
            )],
            |_| {},
        );
        let PluginCommandResponse::OpenWindow { page, .. } = response else {
            panic!("应返回窗口");
        };
        let summary_table = page.table.expect("汇总页应包含表格");
        let detail_page = summary_table.row_actions[0][0]
            .page
            .as_ref()
            .expect("详情动作应携带页面");
        let detail_table = detail_page.table.as_ref().expect("详情页应包含表格");
        let sql_action = &detail_table.row_actions[0][0];

        assert_eq!(sql_action.label, "显示SQL");
        assert!(sql_action.page.is_none());
        assert_eq!(
            sql_action
                .command
                .as_ref()
                .map(|command| command.command_id.as_str()),
            Some("show_request_sql")
        );
    }

    #[test]
    fn sql_行按五个耗时字段解析并按总耗时排序() {
        let first = parse_sql_log_line(
            "0ms 0ms 0ms 0ms 0ms select datavalue from cloudstore_ecode where type = ?",
        )
        .expect("合法 SQL 行应能解析");
        assert_eq!(first.total_ms, 0);
        assert_eq!(
            first.sql,
            "select datavalue from cloudstore_ecode where type = ?"
        );
        assert!(parse_sql_log_line("0ms 0ms 0ms 0ms").is_none());
        assert!(parse_sql_log_line("bad 0ms 0ms 0ms 0ms select 1").is_none());

        let mut rows = vec![
            parse_sql_log_line("100ms 1ms 2ms 3ms 4ms select a").unwrap(),
            parse_sql_log_line("900ms 1ms 2ms 3ms 4ms select b").unwrap(),
        ];
        rows.sort_by(compare_sql_logs);
        assert_eq!(rows[0].total_ms, 900);
    }

    #[test]
    fn sql_窗口读取单个日志并展示排序结果() {
        let path = std::env::temp_dir().join(format!(
            "weaver-logext-sql-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            "10ms 1ms 2ms 3ms 4ms select slow\nbad line\n1ms 0ms 0ms 0ms 0ms select fast\n",
        )
        .expect("测试日志应能写入临时目录");
        let mut data = BTreeMap::new();
        data.insert("route".to_string(), "/a".to_string());
        let response = build_sql_list_response(
            readable_file(
                "100&alice&_a&1778661627833&0&0.log",
                &path.to_string_lossy(),
            ),
            data,
            &mut io::Cursor::new(Vec::<u8>::new()),
            |_| {},
        );
        let _ = std::fs::remove_file(&path);
        let PluginCommandResponse::OpenWindow { page, .. } = response else {
            panic!("应返回 SQL 窗口");
        };
        let table = page.table.expect("SQL 窗口应包含表格");

        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0][0], "10");
        assert_eq!(table.rows[0][5], "select slow");
        assert_eq!(table.rows[1][0], "1");
        assert_eq!(page.stats[0].value, "/a");
    }

    #[test]
    fn sql_窗口支持宿主_stdin_内容流() {
        let mut data = BTreeMap::new();
        data.insert("route".to_string(), "/stream".to_string());
        let raw = concat!(
            "{\"event\":\"log_content_line\",\"line\":\"2ms 0ms 0ms 0ms 0ms select second\"}\n",
            "{\"event\":\"log_content_line\",\"line\":\"9ms 0ms 0ms 0ms 0ms select first\"}\n",
            "{\"event\":\"log_content_line\",\"line\":\"bad\"}\n"
        );
        let response = build_sql_list_response(
            file("100&alice&_stream&1778661627833&0&0.log"),
            data,
            &mut io::Cursor::new(raw.as_bytes()),
            |_| {},
        );
        let PluginCommandResponse::OpenWindow { page, .. } = response else {
            panic!("应返回 SQL 窗口");
        };
        let table = page.table.expect("SQL 窗口应包含表格");

        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0][0], "9");
        assert_eq!(table.rows[0][5], "select first");
        assert_eq!(page.stats[0].value, "/stream");
    }

    #[test]
    fn 性能列表解析会上报开始推进和整理进度() {
        let mut progresses = Vec::new();
        let response = build_performance_list_response(
            vec![
                file("100&u&_a&1778661627833&0&0.log"),
                file("bad.log"),
                file("300&u&_b&1778661627834&0&0.log"),
            ],
            |progress| progresses.push(progress),
        );
        let PluginCommandResponse::OpenWindow { page, .. } = response else {
            panic!("应返回窗口");
        };
        assert_eq!(page.table.unwrap().rows.len(), 2);
        assert!(progresses.len() >= 3);
        assert_eq!(progresses[0].done, 0);
        assert_eq!(progresses[0].total, Some(3));
        assert_eq!(progresses.last().map(|progress| progress.done), Some(3));
    }
}
