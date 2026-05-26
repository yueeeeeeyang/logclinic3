//! 泛微日志插件 sidecar。
//!
//! 业务意图：
//! - 该二进制作为独立插件工程编译和打包，由宿主通过 stdin/stdout 传递 JSON 请求和响应。
//! - “性能列表解析”先使用宿主传入的文件元数据解析文件名；用户点击“显示SQL”后才读取对应单个日志正文。
//! - “E9日志分析”由日志工具栏触发，当前阶段扫描宿主授权日志树快照中的 memory 和连接池日志并做步骤式诊断输出。
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
    io::{self, BufRead, BufReader, Write},
};

use chrono::{Duration as ChronoDuration, Local, NaiveDateTime, TimeZone};
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
    /// 表格行按钮针对当前插件页面来源快照发起的二次命令。
    ///
    /// 业务意图：
    /// - 性能汇总页不能在首次响应里为每个请求地址内嵌完整详情页，否则大目录会生成巨大的 JSON 并卡住宿主窗口初始化。
    /// - 插件首个响应只保存请求地址；用户点击详情时，宿主把当次授权的日志文件快照补回 `files` 后再次调用插件。
    TableAction {
        /// 行动作 ID。
        action_id: String,
        /// 当前插件页面来源日志快照。
        #[serde(default)]
        files: Vec<PluginLogFile>,
        /// 额外业务字段，例如请求地址。
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
    /// 开始步骤式输出块。
    OutputStepStart {
        /// 步骤快照。
        step: PluginOutputStep,
    },
    /// 向步骤追加文本块。
    OutputStepAppend {
        /// 步骤稳定 ID。
        step_id: String,
        /// 本次追加的纯文本。
        text: String,
    },
    /// 结束步骤式输出块。
    OutputStepFinish {
        /// 步骤稳定 ID。
        step_id: String,
        /// 完成后显示的标题。
        done_text: String,
        /// 步骤最终状态。
        status: PluginOutputStepStatus,
    },
    /// 请求宿主写入指定日志来源正文。
    LogContentRequest {
        /// 当前日志来源稳定键。
        source_key: String,
        /// 当前日志展示路径。
        path_label: Option<String>,
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
    /// 步骤式输出块。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    output_steps: Vec<PluginOutputStep>,
    /// 结果表格。
    table: Option<PluginPageTable>,
}

/// 插件步骤式输出状态。
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PluginOutputStepStatus {
    /// 步骤正在运行。
    Running,
    /// 步骤已正常完成。
    Completed,
    /// 步骤执行完成但存在读取失败。
    Failed,
}

/// 插件步骤式输出块。
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct PluginOutputStep {
    /// 步骤稳定 ID。
    id: String,
    /// 运行中文案。
    loading_text: String,
    /// 完成或失败后的标题文案。
    #[serde(skip_serializing_if = "Option::is_none")]
    done_text: Option<String>,
    /// 当前步骤状态。
    status: PluginOutputStepStatus,
    /// 步骤正文完整快照。
    content: String,
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
    /// 表格命令过滤器。
    ///
    /// 业务意图：
    /// - 用户过滤和时间区间过滤需要重新计算汇总统计，插件通过通用控件声明输入字段，宿主只负责渲染和回传原始文本。
    #[serde(skip_serializing_if = "Option::is_none")]
    command_filter: Option<PluginTableCommandFilter>,
    /// 每行对应的可点击动作；旧宿主忽略未知字段，新宿主会渲染为按钮。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    row_actions: Vec<Vec<PluginTableRowAction>>,
}

/// 表格命令过滤器。
#[derive(Debug, Serialize)]
struct PluginTableCommandFilter {
    /// 插件声明的输入控件。
    controls: Vec<PluginTableCommandFilterControl>,
    /// 应用过滤时执行的插件命令。
    command: PluginTableRowCommand,
}

/// 表格命令过滤器中的单个输入控件。
#[derive(Debug, Serialize)]
struct PluginTableCommandFilterControl {
    /// 回写到命令上下文 data 的字段名。
    key: String,
    /// 当前输入值。
    value: String,
    /// 空输入时的占位文案。
    placeholder: String,
    /// 可选通用图标名。
    #[serde(skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    /// 输入框建议宽度。
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
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
    /// 宿主开始写入指定日志正文。
    LogContentBegin {
        /// 日志来源稳定键。
        source_key: String,
        /// 日志展示路径。
        path_label: String,
    },
    /// 一行日志正文。
    LogContentLine {
        /// 已去掉换行符的日志行文本。
        line: String,
    },
    /// 宿主完成当前日志正文写入。
    LogContentEnd {
        /// 日志来源稳定键。
        source_key: String,
        /// 宿主发送的行数。
        lines: u64,
    },
    /// 宿主读取当前日志正文失败。
    LogContentError {
        /// 日志来源稳定键。
        source_key: String,
        /// 错误说明。
        message: String,
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

/// memory 日志单文件分析结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct WeaverMemoryAnalysis {
    /// 扫描行数。
    scanned_lines: u64,
    /// 跳过行数。
    skipped_lines: u64,
    /// 异常记录。
    issues: Vec<WeaverMemoryIssue>,
    /// 异常时间段，供后续日志类型分析复用。
    problem_ranges: Vec<WeaverProblemTimeRange>,
}

/// memory 日志异常记录。
#[derive(Clone, Debug, PartialEq, Eq)]
struct WeaverMemoryIssue {
    /// 行号，从 1 开始。
    line_number: u64,
    /// 日志时间。
    timestamp: String,
    /// 异常原因。
    reason: String,
    /// 关键值展示。
    value: String,
    /// 原始行摘要。
    line: String,
    /// 截图式上下文行。
    context_lines: Vec<WeaverMemoryContextLine>,
    /// 当前异常行中需要高亮的原始文本片段。
    highlight_terms: Vec<String>,
}

/// memory 异常上下文行。
#[derive(Clone, Debug, PartialEq, Eq)]
struct WeaverMemoryContextLine {
    /// 行号，从 1 开始。
    line_number: u64,
    /// 原始日志行。
    text: String,
    /// 是否为当前异常行。
    is_issue_line: bool,
    /// 当前行需要高亮的原始文本片段。
    highlight_terms: Vec<String>,
}

/// memory 异常展示分组。
///
/// 业务意图：
/// - 高频连续异常逐条输出会生成大量重复上下文，用户很难快速看出异常时间段。
/// - 展示层按“连续异常段”合并相邻异常；即使总跨度超过 1 分钟，只要异常记录持续连续，也展示为一个区间。
/// - 分析结果仍保留单条异常，便于统计和后续问题时间段复用。
#[derive(Clone, Debug, PartialEq, Eq)]
struct WeaverMemoryIssueGroup {
    /// 起始异常行号。
    start_line_number: u64,
    /// 结束异常行号。
    end_line_number: u64,
    /// 起始异常时间。
    start_timestamp: String,
    /// 结束异常时间。
    end_timestamp: String,
    /// 合并后的异常原因。
    reason: String,
    /// 合并后的关键值说明。
    value: String,
    /// 当前分组包含的异常记录数。
    issue_count: usize,
    /// 合并后的截图式上下文行。
    context_lines: Vec<WeaverMemoryContextLine>,
}

/// 异常时间段。
///
/// 业务意图：
/// - memory 先分析出的异常时间窗口会成为后续 stdout、ecology、线程日志等分析步骤的过滤基础。
/// - 当前阶段只在插件进程内存和最终响应构建期间保存，后续补充其它步骤时直接复用该结构。
#[derive(Clone, Debug, PartialEq, Eq)]
struct WeaverProblemTimeRange {
    /// 起始时间，取异常时间点前 60 秒。
    start: NaiveDateTime,
    /// 结束时间，取异常时间点后 60 秒。
    end: NaiveDateTime,
    /// 来源行号。
    line_number: u64,
    /// 异常原因。
    reason: String,
}

/// 第二种 memory 格式的上一条有效采样。
#[derive(Clone, Debug, PartialEq)]
struct WeaverMemoryFormat2Sample {
    /// 采样时间。
    timestamp: NaiveDateTime,
    /// 原始时间文本。
    timestamp_text: String,
    /// FGC 次数。
    fgc_count: f64,
    /// FGCT 累计秒数。
    fgct_seconds: f64,
    /// 连续短间隔 FGC 增长次数。
    ///
    /// 业务意图：
    /// - 单次 Full GC 在 60 秒内增长一次并不一定异常，只有连续采样都增长或一次采样内增长多次才说明 Full GC 过于频繁。
    /// - 该计数只跟随第二种 memory 格式的有效采样，非法行不会中断整个文件，但也不会制造新的连续增长。
    fgc_growth_streak: u32,
}

/// memory 日志文件级格式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WeaverMemoryLogFormat {
    /// 第一种格式，只检查第 4 逻辑列。
    Format1,
    /// 第二种格式，只检查 FGC/FGCT。
    Format2,
}

/// 连接池日志单文件分析结果。
///
/// 业务意图：
/// - 连接池日志反映数据库连接占用趋势，和 memory 问题时间段一样会成为后续综合诊断的基础数据。
/// - 结果按“问题段”保存，避免连接数持续异常时逐行刷屏。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct WeaverPoolAnalysis {
    /// 扫描行数。
    scanned_lines: u64,
    /// 跳过行数。
    skipped_lines: u64,
    /// 连接池异常问题段。
    issues: Vec<WeaverPoolIssue>,
    /// 异常时间段，供后续其它日志分析步骤复用。
    problem_ranges: Vec<WeaverProblemTimeRange>,
}

/// 连接池日志单条有效采样。
///
/// 边界条件：
/// - 原始日志可能使用 tab 或多个空格分隔，解析阶段统一按空白拆分。
/// - 只把前 6 个业务字段结构化，后续忽略字段保留在原始行中用于截图式上下文展示。
#[derive(Clone, Debug, PartialEq, Eq)]
struct WeaverPoolSample {
    /// 原始行号，从 1 开始。
    line_number: u64,
    /// 连接池名称。
    pool_name: String,
    /// 采样时间。
    timestamp: NaiveDateTime,
    /// 原始时间文本。
    timestamp_text: String,
    /// 活跃连接数。
    active_connections: u64,
    /// 默认连接数。
    default_connections: u64,
    /// 最大连接数。
    max_connections: u64,
    /// 活跃连接数字符串，用于高亮截图里的原始列。
    active_token: String,
}

/// 连接池异常问题段。
#[derive(Clone, Debug, PartialEq, Eq)]
struct WeaverPoolIssue {
    /// 起始异常行号。
    start_line_number: u64,
    /// 结束异常行号。
    end_line_number: u64,
    /// 起始异常时间。
    start_timestamp: String,
    /// 结束异常时间。
    end_timestamp: String,
    /// 异常原因。
    reason: String,
    /// 关键值说明。
    value: String,
    /// 截图式上下文行。
    context_lines: Vec<WeaverPoolContextLine>,
}

/// 连接池异常上下文行。
#[derive(Clone, Debug, PartialEq, Eq)]
struct WeaverPoolContextLine {
    /// 原始行号，从 1 开始。
    line_number: u64,
    /// 展开 tab 后的原始日志行。
    text: String,
    /// 是否属于当前问题段的异常采样行。
    is_issue_line: bool,
    /// 当前行需要高亮的原始文本片段。
    highlight_terms: Vec<String>,
}

/// 连接池高活跃连接短突增段。
///
/// 业务意图：
/// - 周期性规则关注“每 3 分钟左右出现一次超过 10 的波峰”，而不是日志每 10 秒采样时同一波峰内的每一行。
/// - 先把 60 秒内连续的 `active > 10` 采样压成一个波峰，可以减少周期识别误报和漏报。
#[derive(Clone, Debug, PartialEq, Eq)]
struct WeaverPoolActiveBurst {
    /// 波峰起始采样。
    start: WeaverPoolSample,
    /// 波峰结束采样。
    end: WeaverPoolSample,
    /// 波峰内活跃连接数最大的采样。
    peak: WeaverPoolSample,
    /// 波峰内需要在截图中高亮的采样行。
    issue_lines: Vec<(u64, String)>,
}

/// 插件输出中日志截图片段的起始标记。
///
/// 业务意图：
/// - 插件协议当前只有文本流，不能直接传 UI 节点；用内部标记把“普通诊断文本”和“带行号日志片段”区分开。
/// - 宿主渲染时会消费这些标记并转换成截图式代码块，用户不会看到标记文本。
const WEAVER_LOG_SNIPPET_BEGIN: &str = "@@LC_LOG_SNIPPET_BEGIN";

/// 插件输出中日志截图片段的结束标记。
const WEAVER_LOG_SNIPPET_END: &str = "@@LC_LOG_SNIPPET_END";

/// 插件输出中单行日志片段的标记前缀。
const WEAVER_LOG_SNIPPET_LINE_PREFIX: &str = "@@LC_LOG_LINE\t";

/// 性能表格用户过滤字段。
const PERFORMANCE_FILTER_USERS_KEY: &str = "filter_users";

/// 性能表格起始请求时间过滤字段。
const PERFORMANCE_FILTER_START_KEY: &str = "filter_start_time";

/// 性能表格结束请求时间过滤字段。
const PERFORMANCE_FILTER_END_KEY: &str = "filter_end_time";

/// 性能表格请求地址字段。
const PERFORMANCE_FILTER_ROUTE_KEY: &str = "route";

/// 按请求地址聚合后的性能摘要。
struct WeaverRouteSummary {
    /// 请求地址。
    route: String,
    /// 当前请求地址的全部请求明细。
    logs: Vec<WeaverPerformanceLog>,
}

/// 性能列表/详情的业务过滤条件。
///
/// 业务意图：
/// - 用户输入保持原样回显，但匹配时需要拆成多个用户片段并做包含匹配。
/// - 时间过滤作用于文件名中的 13 位毫秒时间戳，避免解析展示字符串造成跨平台格式差异。
struct WeaverPerformanceFilter {
    /// 用户过滤原始文本。
    users_raw: String,
    /// 起始时间原始文本。
    start_time_raw: String,
    /// 结束时间原始文本。
    end_time_raw: String,
    /// 归一化后的用户匹配片段，小写，多个片段按 OR 匹配。
    user_tokens: Vec<String>,
    /// 起始时间戳，毫秒，包含边界。
    start_timestamp_ms: Option<i64>,
    /// 结束时间戳，毫秒，包含整秒边界。
    end_timestamp_ms: Option<i64>,
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

impl WeaverPerformanceFilter {
    /// 从插件命令上下文解析性能过滤条件。
    ///
    /// 业务意图：
    /// - 宿主只负责收集输入框原始文本，插件集中处理用户匹配、时间解析和错误文案，避免过滤规则散落在宿主 UI。
    ///
    /// 边界条件：
    /// - 用户为空表示不过滤；多个用户使用英文逗号分隔并按“任一片段包含”匹配。
    /// - 时间格式固定为 `yyyy-MM-dd HH:mm:ss`；结束时间按整秒闭区间处理，包含该秒内的毫秒请求。
    fn from_data(data: &BTreeMap<String, String>) -> Result<Self, String> {
        let users_raw = data
            .get(PERFORMANCE_FILTER_USERS_KEY)
            .cloned()
            .unwrap_or_default();
        let start_time_raw = data
            .get(PERFORMANCE_FILTER_START_KEY)
            .cloned()
            .unwrap_or_default();
        let end_time_raw = data
            .get(PERFORMANCE_FILTER_END_KEY)
            .cloned()
            .unwrap_or_default();
        let user_tokens = users_raw
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        let start_timestamp_ms =
            parse_filter_time_millis(&start_time_raw, "开始时间", false)?;
        let end_timestamp_ms = parse_filter_time_millis(&end_time_raw, "结束时间", true)?;
        if let (Some(start), Some(end)) = (start_timestamp_ms, end_timestamp_ms)
            && start > end
        {
            return Err("开始时间不能晚于结束时间".to_string());
        }
        Ok(Self {
            users_raw,
            start_time_raw,
            end_time_raw,
            user_tokens,
            start_timestamp_ms,
            end_timestamp_ms,
        })
    }

    /// 判断单条性能日志是否满足过滤条件。
    fn matches(&self, log: &WeaverPerformanceLog) -> bool {
        let user_matches = self.user_tokens.is_empty() || {
            let user = log.user.to_lowercase();
            self.user_tokens.iter().any(|token| user.contains(token))
        };
        let start_matches = self
            .start_timestamp_ms
            .is_none_or(|start| log.timestamp_ms >= start);
        let end_matches = self
            .end_timestamp_ms
            .is_none_or(|end| log.timestamp_ms <= end);
        user_matches && start_matches && end_matches
    }

    /// 是否已经填写任一过滤条件。
    fn is_active(&self) -> bool {
        !self.user_tokens.is_empty()
            || self.start_timestamp_ms.is_some()
            || self.end_timestamp_ms.is_some()
    }

    /// 把过滤条件写回插件命令数据。
    fn write_to_data(&self, data: &mut BTreeMap<String, String>) {
        data.insert(
            PERFORMANCE_FILTER_USERS_KEY.to_string(),
            self.users_raw.clone(),
        );
        data.insert(
            PERFORMANCE_FILTER_START_KEY.to_string(),
            self.start_time_raw.clone(),
        );
        data.insert(
            PERFORMANCE_FILTER_END_KEY.to_string(),
            self.end_time_raw.clone(),
        );
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
            "filter_performance_list",
            PluginCommandContext::TableAction {
                action_id,
                files,
                data,
            },
        ) if action_id == "apply_filter" => {
            Ok(build_performance_list_response_with_filter(
                files,
                data,
                emit_progress,
            ))
        }
        (
            "show_performance_detail",
            PluginCommandContext::TableAction {
                action_id,
                files,
                data,
            },
        ) if action_id == "show_detail" => Ok(build_performance_detail_response(
            files,
            data,
            emit_progress,
        )),
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
            content_reader,
            emit_output_step_start,
            emit_output_step_append,
            emit_output_step_finish,
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
        ("show_performance_detail", _) => Err("性能详情只能从性能汇总行按钮触发".to_string()),
        ("filter_performance_list", _) => Err("性能列表过滤只能从性能列表页触发".to_string()),
        ("show_request_sql", _) => Err("显示 SQL 只能从请求详情页的按钮触发".to_string()),
        ("performance_list_parse", PluginCommandContext::Unsupported) => {
            Err("性能列表解析只能从日志目录树右键菜单触发".to_string())
        }
        ("weaver_log_scan", _) => Err("泛微日志分析只能从日志分析页工具栏触发".to_string()),
        (command_id, _) => Err(format!("未知插件命令：{command_id}")),
    }
}

/// 构造 E9 日志分析窗口响应。
///
/// 业务意图：
/// - E9 分析窗口按步骤式流式输出，当前先分析 memory，再分析数据库连接池日志。
/// - 插件只按已实现步骤的规则筛选并读取正文，不再统计 stdout、web.xml、runtime 等其它日志类型，避免读取或展示用户当前不需要的内容。
///
/// 边界条件：
/// - 宿主仍会提供当前已加载日志树快照；插件只遍历这份元数据来发现可分析文件，不自行访问任意路径。
/// - 单个文件读取失败时写入中文错误并继续后续文件，最终对应步骤标记为 Failed。
fn build_weaver_log_scan_response<R, F, G, H>(
    files: Vec<PluginLogFile>,
    settings: BTreeMap<String, String>,
    content_reader: &mut R,
    mut report_step_start: F,
    mut report_step_append: G,
    mut report_step_finish: H,
) -> PluginCommandResponse
where
    R: BufRead,
    F: FnMut(PluginOutputStep),
    G: FnMut(String, String),
    H: FnMut(String, String, PluginOutputStepStatus),
{
    let memory_files = collect_weaver_log_files_by_rule(&files, &settings, "memory");
    let pool_files = collect_weaver_log_files_by_rule(&files, &settings, "pool");
    let mut output_steps = Vec::new();
    output_steps.push(run_memory_analysis_step(
        &memory_files,
        content_reader,
        &mut report_step_start,
        &mut report_step_append,
        &mut report_step_finish,
    ));
    output_steps.push(run_pool_analysis_step(
        &pool_files,
        content_reader,
        &mut report_step_start,
        &mut report_step_append,
        &mut report_step_finish,
    ));

    PluginCommandResponse::OpenWindow {
        title: "E9日志分析".to_string(),
        page: PluginPage {
            title: "E9日志分析".to_string(),
            description: None,
            stats: Vec::new(),
            output_steps,
            table: None,
        },
    }
}

/// 按指定泛微日志规则从宿主快照中筛选候选文件。
///
/// 业务意图：
/// - 宿主只负责限制授权范围和做粗过滤，插件仍需要按自己的规则做最终匹配，保证设置页修改后行为一致。
/// - 返回值保留相对路径，后续输出和正文请求都使用同一条路径，便于用户定位压缩包链路。
fn collect_weaver_log_files_by_rule(
    files: &[PluginLogFile],
    settings: &BTreeMap<String, String>,
    rule_key: &str,
) -> Vec<(PluginLogFile, String)> {
    let Some(rule) = WEAVER_LOG_RULES.iter().find(|rule| rule.key == rule_key) else {
        return Vec::new();
    };
    let patterns = settings
        .get(rule.key)
        .map(String::as_str)
        .unwrap_or(rule.default_patterns);
    files
        .iter()
        .filter_map(|file| {
            let relative_path = plugin_file_relative_path(file);
            weaver_rule_matches(patterns, &relative_path).then(|| (file.clone(), relative_path))
        })
        .collect()
}

/// 执行 memory 日志步骤。
fn run_memory_analysis_step<R, F, G, H>(
    memory_files: &[(PluginLogFile, String)],
    content_reader: &mut R,
    report_step_start: &mut F,
    mut report_step_append: &mut G,
    report_step_finish: &mut H,
) -> PluginOutputStep
where
    R: BufRead,
    F: FnMut(PluginOutputStep),
    G: FnMut(String, String),
    H: FnMut(String, String, PluginOutputStepStatus),
{
    let mut step = PluginOutputStep {
        id: "memory".to_string(),
        loading_text: "正在分析memory日志".to_string(),
        done_text: None,
        status: PluginOutputStepStatus::Running,
        content: String::new(),
    };
    report_step_start(step.clone());

    let mut analyzed_files = 0usize;
    let mut issue_files = 0usize;
    let mut issue_count = 0usize;
    let mut skipped_lines = 0u64;
    let mut read_failures = 0usize;
    let mut problem_ranges = Vec::<WeaverProblemTimeRange>::new();
    if memory_files.is_empty() {
        append_step_text(
            &mut step,
            &mut report_step_append,
            "未扫描到 memory 日志。\n",
        );
    } else {
        let mut summary = format!("扫描到 {} 个 memory 日志：\n", memory_files.len());
        for (_, relative_path) in memory_files {
            summary.push_str("- ");
            summary.push_str(relative_path);
            summary.push('\n');
        }
        append_step_text(&mut step, &mut report_step_append, summary);
    }

    for (file, relative_path) in memory_files.iter() {
        emit_log_content_request(file, relative_path);
        match read_log_content_for_file(content_reader, &file.source_key) {
            Ok(Some(lines)) => {
                let analysis = analyze_memory_lines(lines);
                analyzed_files += 1;
                skipped_lines = skipped_lines.saturating_add(analysis.skipped_lines);
                problem_ranges.extend(analysis.problem_ranges.iter().cloned());
                if !analysis.issues.is_empty() {
                    issue_files += 1;
                    issue_count += analysis.issues.len();
                }
                for issue_group in group_memory_issues_for_display(&analysis.issues) {
                    append_step_text(
                        &mut step,
                        &mut report_step_append,
                        format_memory_issue_group_summary(&issue_group),
                    );
                    append_memory_issue_context(&mut step, &mut report_step_append, &issue_group);
                }
                append_step_text(
                    &mut step,
                    &mut report_step_append,
                    format!(
                        "完成：扫描 {} 行，异常 {} 条，跳过 {} 行\n",
                        analysis.scanned_lines,
                        analysis.issues.len(),
                        analysis.skipped_lines
                    ),
                );
            }
            Ok(None) => {
                read_failures += 1;
                append_step_text(
                    &mut step,
                    &mut report_step_append,
                    "读取失败：宿主没有返回该 memory 日志的正文。\n",
                );
            }
            Err(message) => {
                read_failures += 1;
                append_step_text(
                    &mut step,
                    &mut report_step_append,
                    format!("读取失败：{message}\n"),
                );
            }
        }
    }

    append_step_text(
        &mut step,
        &mut report_step_append,
        format!(
            "\n汇总：匹配 {} 个 memory 日志，成功分析 {} 个，异常文件 {} 个，异常记录 {} 条，问题时间段 {} 个，跳过 {} 行，读取失败 {} 个。\n",
            memory_files.len(),
            analyzed_files,
            issue_files,
            issue_count,
            problem_ranges.len(),
            skipped_lines,
            read_failures
        ),
    );

    let (done_text, final_status) = if read_failures > 0 {
        (
            "memory日志分析完成，但存在读取失败".to_string(),
            PluginOutputStepStatus::Failed,
        )
    } else {
        (
            "memory日志已分析完毕".to_string(),
            PluginOutputStepStatus::Completed,
        )
    };
    step.done_text = Some(done_text.clone());
    step.status = final_status;
    report_step_finish(step.id.clone(), done_text, final_status);
    step
}

/// 执行数据库连接池日志步骤。
fn run_pool_analysis_step<R, F, G, H>(
    pool_files: &[(PluginLogFile, String)],
    content_reader: &mut R,
    report_step_start: &mut F,
    mut report_step_append: &mut G,
    report_step_finish: &mut H,
) -> PluginOutputStep
where
    R: BufRead,
    F: FnMut(PluginOutputStep),
    G: FnMut(String, String),
    H: FnMut(String, String, PluginOutputStepStatus),
{
    let mut step = PluginOutputStep {
        id: "pool".to_string(),
        loading_text: "正在分析连接池日志".to_string(),
        done_text: None,
        status: PluginOutputStepStatus::Running,
        content: String::new(),
    };
    report_step_start(step.clone());

    let mut analyzed_files = 0usize;
    let mut issue_files = 0usize;
    let mut issue_count = 0usize;
    let mut skipped_lines = 0u64;
    let mut read_failures = 0usize;
    let mut problem_ranges = Vec::<WeaverProblemTimeRange>::new();
    if pool_files.is_empty() {
        append_step_text(
            &mut step,
            &mut report_step_append,
            "未扫描到连接池日志。\n",
        );
    } else {
        let mut summary = format!("扫描到 {} 个连接池日志：\n", pool_files.len());
        for (_, relative_path) in pool_files {
            summary.push_str("- ");
            summary.push_str(relative_path);
            summary.push('\n');
        }
        append_step_text(&mut step, &mut report_step_append, summary);
    }

    for (file, relative_path) in pool_files {
        emit_log_content_request(file, relative_path);
        match read_log_content_for_file(content_reader, &file.source_key) {
            Ok(Some(lines)) => {
                let analysis = analyze_pool_lines(lines);
                analyzed_files += 1;
                skipped_lines = skipped_lines.saturating_add(analysis.skipped_lines);
                problem_ranges.extend(analysis.problem_ranges.iter().cloned());
                if !analysis.issues.is_empty() {
                    issue_files += 1;
                    issue_count += analysis.issues.len();
                }
                for issue in &analysis.issues {
                    append_step_text(
                        &mut step,
                        &mut report_step_append,
                        format_pool_issue_summary(issue),
                    );
                    append_pool_issue_context(&mut step, &mut report_step_append, issue);
                }
                append_step_text(
                    &mut step,
                    &mut report_step_append,
                    format!(
                        "完成：扫描 {} 行，异常 {} 段，跳过 {} 行\n",
                        analysis.scanned_lines,
                        analysis.issues.len(),
                        analysis.skipped_lines
                    ),
                );
            }
            Ok(None) => {
                read_failures += 1;
                append_step_text(
                    &mut step,
                    &mut report_step_append,
                    "读取失败：宿主没有返回该连接池日志的正文。\n",
                );
            }
            Err(message) => {
                read_failures += 1;
                append_step_text(
                    &mut step,
                    &mut report_step_append,
                    format!("读取失败：{message}\n"),
                );
            }
        }
    }

    append_step_text(
        &mut step,
        &mut report_step_append,
        format!(
            "\n汇总：匹配 {} 个连接池日志，成功分析 {} 个，异常文件 {} 个，异常段 {} 个，问题时间段 {} 个，跳过 {} 行，读取失败 {} 个。\n",
            pool_files.len(),
            analyzed_files,
            issue_files,
            issue_count,
            problem_ranges.len(),
            skipped_lines,
            read_failures
        ),
    );

    let (done_text, final_status) = if read_failures > 0 {
        (
            "连接池日志分析完成，但存在读取失败".to_string(),
            PluginOutputStepStatus::Failed,
        )
    } else {
        (
            "连接池日志已分析完毕".to_string(),
            PluginOutputStepStatus::Completed,
        )
    };
    step.done_text = Some(done_text.clone());
    step.status = final_status;
    report_step_finish(step.id.clone(), done_text, final_status);
    step
}

/// 追加连接池异常原始日志上下文片段。
///
/// 业务意图：
/// - 连接池告警通常需要确认活跃连接在前后采样中的变化，截图式上下文比单行摘要更适合定位连接耗尽或周期性波动。
/// - 与 memory 使用同一段内部文本协议，宿主无需为每个日志类型实现不同的 UI 协议。
fn append_pool_issue_context<G>(
    step: &mut PluginOutputStep,
    report_step_append: &mut G,
    issue: &WeaverPoolIssue,
) where
    G: FnMut(String, String),
{
    if issue.context_lines.is_empty() {
        return;
    }

    let mut snippet = String::new();
    snippet.push_str(WEAVER_LOG_SNIPPET_BEGIN);
    snippet.push('\n');
    for line in &issue.context_lines {
        let level = if line.is_issue_line {
            "issue"
        } else {
            "normal"
        };
        snippet.push_str(WEAVER_LOG_SNIPPET_LINE_PREFIX);
        snippet.push_str(&line.line_number.to_string());
        snippet.push('\t');
        snippet.push_str(level);
        snippet.push('\t');
        snippet.push_str(&line.highlight_terms.join(","));
        snippet.push('\t');
        snippet.push_str(&line.text.replace('\n', " "));
        snippet.push('\n');
    }
    snippet.push_str(WEAVER_LOG_SNIPPET_END);
    snippet.push('\n');
    append_step_text(step, report_step_append, snippet);
}

/// 追加并实时上报步骤正文。
fn append_step_text<G>(
    step: &mut PluginOutputStep,
    report_step_append: &mut G,
    text: impl Into<String>,
) where
    G: FnMut(String, String),
{
    let text = text.into();
    step.content.push_str(&text);
    report_step_append(step.id.clone(), text);
}

/// 追加 memory 异常原始日志上下文片段。
///
/// 业务意图：
/// - 异常摘要只能说明“为什么报警”，定位现场问题还需要看到异常行前后的原始 jstat 输出。
/// - 这里用文本协议携带行号、异常行标记和高亮片段，宿主负责渲染成截图式日志块，避免插件直接绑定 GPUI 实现。
///
/// 边界条件：
/// - 原始日志可能包含制表符；协议只把前 4 个字段作为结构化字段，剩余内容原样作为日志文本。
/// - 高亮词只来自解析出的数字 token，保持 ASCII 文本，避免跨 UTF-8 字符边界切割。
fn append_memory_issue_context<G>(
    step: &mut PluginOutputStep,
    report_step_append: &mut G,
    issue_group: &WeaverMemoryIssueGroup,
) where
    G: FnMut(String, String),
{
    if issue_group.context_lines.is_empty() {
        return;
    }

    let mut snippet = String::new();
    snippet.push_str(WEAVER_LOG_SNIPPET_BEGIN);
    snippet.push('\n');
    for line in &issue_group.context_lines {
        let level = if line.is_issue_line {
            "issue"
        } else {
            "normal"
        };
        snippet.push_str(WEAVER_LOG_SNIPPET_LINE_PREFIX);
        snippet.push_str(&line.line_number.to_string());
        snippet.push('\t');
        snippet.push_str(level);
        snippet.push('\t');
        snippet.push_str(&line.highlight_terms.join(","));
        snippet.push('\t');
        snippet.push_str(&line.text.replace('\n', " "));
        snippet.push('\n');
    }
    snippet.push_str(WEAVER_LOG_SNIPPET_END);
    snippet.push('\n');
    append_step_text(step, report_step_append, snippet);
}

/// 格式化 memory 异常分组摘要。
fn format_memory_issue_group_summary(issue_group: &WeaverMemoryIssueGroup) -> String {
    if issue_group.issue_count <= 1 || issue_group.start_line_number == issue_group.end_line_number
    {
        return format!(
            "异常行 {} [{}] {}；{}\n",
            issue_group.start_line_number,
            issue_group.start_timestamp,
            issue_group.reason,
            issue_group.value
        );
    }

    format!(
        "异常行 {} ～ {} [{} ～ {}] {}；连续异常 {} 条\n",
        issue_group.start_line_number,
        issue_group.end_line_number,
        issue_group.start_timestamp,
        issue_group.end_timestamp,
        issue_group.reason,
        issue_group.issue_count
    )
}

/// 合并连续 memory 异常展示区间。
///
/// 业务意图：
/// - 同一问题通常会在 jstat 采样中连续出现多行，逐条输出会造成大量重复截图块。
/// - 这里仅合并展示，不改变 `WeaverMemoryAnalysis::issues`，保持统计和后续分析基础数据的粒度。
///
/// 边界条件：
/// - 行号连续的异常总是合并；行号不连续时，仅合并 60 秒内相邻异常，避免明显断开的异常段互相污染。
/// - 异常上下文按行号去重，多个异常行各自保留高亮词。
fn group_memory_issues_for_display(issues: &[WeaverMemoryIssue]) -> Vec<WeaverMemoryIssueGroup> {
    let mut groups = Vec::new();
    let mut current = Vec::<&WeaverMemoryIssue>::new();

    for issue in issues {
        if let Some(previous) = current.last() {
            if memory_issues_are_contiguous_for_display(previous, issue) {
                current.push(issue);
                continue;
            }
            groups.push(build_memory_issue_group(&current));
            current.clear();
        }
        current.push(issue);
    }

    if !current.is_empty() {
        groups.push(build_memory_issue_group(&current));
    }
    groups
}

/// 判断两条相邻异常是否属于同一个连续异常段。
///
/// 业务意图：
/// - 用户希望连续异常合并成区间，避免同一波 memory 抖动重复输出多段截图。
/// - 连续行号表示日志采样本身没有被正常行打断，因此无论总跨度是否超过 1 分钟都应合并。
/// - 对非连续行号，仍允许 60 秒内的相邻异常合并，覆盖采样中夹杂少量正常行但时间上属于同一问题的场景。
///
/// 边界条件：
/// - 时间解析失败时只按行号连续性判断，避免非法时间把不相邻的异常误合并。
/// - 行号使用饱和加法，避免极端大行号溢出。
fn memory_issues_are_contiguous_for_display(
    previous: &WeaverMemoryIssue,
    current: &WeaverMemoryIssue,
) -> bool {
    if previous.line_number.saturating_add(1) == current.line_number {
        return true;
    }

    let Some(previous_timestamp) = parse_memory_timestamp(&previous.timestamp) else {
        return false;
    };
    let Some(current_timestamp) = parse_memory_timestamp(&current.timestamp) else {
        return false;
    };
    current_timestamp
        .signed_duration_since(previous_timestamp)
        .num_seconds()
        .abs()
        <= 60
}

/// 从连续异常记录构建展示分组。
fn build_memory_issue_group(issues: &[&WeaverMemoryIssue]) -> WeaverMemoryIssueGroup {
    let first = issues
        .first()
        .expect("构建 memory 异常分组时必须至少包含一条异常");
    let last = issues
        .last()
        .expect("构建 memory 异常分组时必须至少包含一条异常");
    WeaverMemoryIssueGroup {
        start_line_number: first.line_number,
        end_line_number: last.line_number,
        start_timestamp: first.timestamp.clone(),
        end_timestamp: last.timestamp.clone(),
        reason: merge_memory_issue_reasons(issues),
        value: merge_memory_issue_values(issues),
        issue_count: issues.len(),
        context_lines: merge_memory_issue_context_lines(issues),
    }
}

/// 合并异常原因并保持首次出现顺序。
fn merge_memory_issue_reasons(issues: &[&WeaverMemoryIssue]) -> String {
    let mut reasons = Vec::<String>::new();
    for issue in issues {
        for reason in issue.reason.split('；').map(str::trim) {
            if !reason.is_empty() && !reasons.iter().any(|existing| existing == reason) {
                reasons.push(reason.to_string());
            }
        }
    }
    reasons.join("；")
}

/// 合并异常关键值说明。
fn merge_memory_issue_values(issues: &[&WeaverMemoryIssue]) -> String {
    if issues.len() == 1 {
        return issues
            .first()
            .map(|issue| issue.value.clone())
            .unwrap_or_default();
    }
    format!("连续异常 {} 条", issues.len())
}

/// 合并异常上下文行，重叠区域只显示一次。
fn merge_memory_issue_context_lines(
    issues: &[&WeaverMemoryIssue],
) -> Vec<WeaverMemoryContextLine> {
    let mut by_line = BTreeMap::<u64, WeaverMemoryContextLine>::new();
    for issue in issues {
        for context_line in &issue.context_lines {
            let entry = by_line
                .entry(context_line.line_number)
                .or_insert_with(|| WeaverMemoryContextLine {
                    line_number: context_line.line_number,
                    text: context_line.text.clone(),
                    is_issue_line: false,
                    highlight_terms: Vec::new(),
                });
            if context_line.is_issue_line {
                entry.is_issue_line = true;
                for term in &context_line.highlight_terms {
                    if !entry.highlight_terms.iter().any(|existing| existing == term) {
                        entry.highlight_terms.push(term.clone());
                    }
                }
            }
        }
    }
    by_line.into_values().collect()
}

/// 读取宿主为指定 source_key 返回的日志正文。
///
/// 业务意图：
/// - 插件按文件逐个请求正文，宿主通过 stdin 写入 begin/line/end/error 事件；这里只消费当前文件对应的事件。
/// - 读取函数不自行打开路径，确保 E9 各步骤分析仍受宿主当前日志树授权范围约束。
///
/// 边界条件：
/// - 收到其它 source_key 的事件时忽略，避免未来宿主批量预取时破坏当前解析。
/// - stdin 提前结束返回 `None`，由上层输出可理解的读取失败行。
fn read_log_content_for_file<R: BufRead>(
    reader: &mut R,
    source_key: &str,
) -> Result<Option<Vec<String>>, String> {
    let mut raw = String::new();
    let mut collecting = false;
    let mut lines = Vec::new();
    loop {
        raw.clear();
        let bytes = reader
            .read_line(&mut raw)
            .map_err(|error| format!("读取宿主日志内容流失败：{error}"))?;
        if bytes == 0 {
            return Ok(None);
        }
        match serde_json::from_str::<PluginStdinEvent>(&raw) {
            Ok(PluginStdinEvent::LogContentBegin {
                source_key: event_key,
                path_label,
            }) => {
                // 路径标签由宿主回显给插件，当前读取函数只用 source_key 定位事件归属。
                let _ = path_label;
                collecting = event_key == source_key;
                if collecting {
                    lines.clear();
                }
            }
            Ok(PluginStdinEvent::LogContentLine { line }) if collecting => lines.push(line),
            Ok(PluginStdinEvent::LogContentEnd {
                source_key: event_key,
                lines: sent_lines,
            }) if collecting && event_key == source_key => {
                // 宿主发送行数用于诊断一致性，当前分析以实际收到的内容事件为准。
                let _ = sent_lines;
                return Ok(Some(lines));
            }
            Ok(PluginStdinEvent::LogContentError {
                source_key: event_key,
                message,
            }) if event_key == source_key => return Err(message),
            Ok(_) | Err(_) => {}
        }
    }
}

/// 分析 memory 日志正文。
///
/// 业务意图：
/// - memory 日志来自 jstat 输出，目前现场存在两种格式；第一种关注用户确认的第 4 逻辑列，第二种关注 Full GC 次数和累计耗时变化。
/// - 该函数只做纯文本解析，便于单元测试覆盖，不依赖插件进程或宿主内容流。
fn analyze_memory_lines(lines: Vec<String>) -> WeaverMemoryAnalysis {
    let mut analysis = WeaverMemoryAnalysis::default();
    let mut previous_format2_sample: Option<WeaverMemoryFormat2Sample> = None;
    let log_format = detect_memory_log_format(&lines);

    for (index, line) in lines.iter().enumerate() {
        analysis.scanned_lines = analysis.scanned_lines.saturating_add(1);
        let line_number = index as u64 + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            analysis.skipped_lines = analysis.skipped_lines.saturating_add(1);
            continue;
        }
        let tokens = trimmed.split_whitespace().collect::<Vec<_>>();
        let parsed = match log_format {
            WeaverMemoryLogFormat::Format2 => {
                analyze_memory_format2_line(trimmed, line_number, previous_format2_sample.as_ref())
                    .map(|(issue, sample)| {
                        previous_format2_sample = Some(sample);
                        issue
                    })
            }
            WeaverMemoryLogFormat::Format1 => {
                analyze_memory_format1_tokens(trimmed, line_number, &tokens)
            }
        };
        match parsed {
            Some(Some(mut issue)) => {
                issue.context_lines =
                    build_memory_context_lines(&lines, issue.line_number, &issue.highlight_terms);
                if let Some(timestamp) = parse_memory_timestamp(&issue.timestamp) {
                    analysis.problem_ranges.push(WeaverProblemTimeRange {
                        start: timestamp - ChronoDuration::seconds(60),
                        end: timestamp + ChronoDuration::seconds(60),
                        line_number: issue.line_number,
                        reason: issue.reason.clone(),
                    });
                }
                analysis.issues.push(issue);
            }
            Some(None) => {}
            None => analysis.skipped_lines = analysis.skipped_lines.saturating_add(1),
        }
    }

    analysis
}

/// 判断 memory 日志整份文件应使用哪一种解析规则。
///
/// 业务意图：
/// - 同一个 memory 文件只能按一种 jstat 格式解释，否则第二种格式中的第 4 个采样值超过 90 时会被误报成第一种格式问题。
/// - 先根据整份文件中的有效采样行识别格式，再逐行分析，保证同一文件的诊断口径稳定。
///
/// 边界条件：
/// - 空文件、只有非法行或无法识别的内容默认按第一种格式处理，后续逐行解析会把非法行计入跳过。
/// - 第二种格式可能由制表符变成空格分隔，因此识别时不能只看 `\t`，还要检查 FGC/FGCT 所在的尾部累计列。
fn detect_memory_log_format(lines: &[String]) -> WeaverMemoryLogFormat {
    let mut format2_score = 0usize;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let tokens = trimmed.split_whitespace().collect::<Vec<_>>();
        if tokens.len() < 5 {
            continue;
        }
        let timestamp_text = format!("{} {}", tokens[0], tokens[1]);
        if parse_memory_timestamp(&timestamp_text).is_none() {
            continue;
        }
        if is_memory_format2_line(trimmed, &tokens) {
            format2_score =
                format2_score.saturating_add(if trimmed.contains('\t') { 2 } else { 1 });
        }
        if format2_score >= 2 {
            return WeaverMemoryLogFormat::Format2;
        }
    }

    if format2_score > 0 {
        WeaverMemoryLogFormat::Format2
    } else {
        WeaverMemoryLogFormat::Format1
    }
}

/// 判断当前行是否符合第二种 memory 采样形态。
///
/// 业务意图：
/// - 现场第二种 jstat 日志有时会从制表符保存成普通空格；不能只依赖 `\t` 判断，否则会错误套用第一种“第 4 逻辑列 > 90”规则。
/// - 第二种格式的前几列是百分比/容量采样，尾部包含 FGC 和 FGCT 累计值；这里只做形状识别，真实异常仍交给第二格式分析函数判断。
fn is_memory_format2_line(line: &str, tokens: &[&str]) -> bool {
    if tokens.len() < 10 {
        return false;
    }
    let numeric_values = tokens[2..]
        .iter()
        .filter_map(|token| token.parse::<f64>().ok())
        .collect::<Vec<_>>();
    if numeric_values.len() != tokens.len().saturating_sub(2) {
        return false;
    }
    let Some(fgc_count) = numeric_values.get(numeric_values.len().saturating_sub(3)) else {
        return false;
    };
    let Some(fgct_seconds) = numeric_values.get(numeric_values.len().saturating_sub(2)) else {
        return false;
    };
    let Some(total_seconds) = numeric_values.last() else {
        return false;
    };
    let has_gc_tail = *fgc_count > 0.0 || *fgct_seconds > 0.0 || *total_seconds > 0.0;
    let has_runtime_like_columns = numeric_values
        .iter()
        .take(numeric_values.len().saturating_sub(3))
        .any(|value| *value > 500.0);
    let has_fractional_first_sample = tokens.get(2).is_some_and(|token| token.contains('.'));
    has_gc_tail && (line.contains('\t') || has_runtime_like_columns || has_fractional_first_sample)
}

/// 构建异常行前后各两行的原始日志上下文。
///
/// 业务意图：
/// - 诊断输出需要像截图一样保留上下文，便于用户确认异常行附近 jstat 指标如何变化。
/// - 行号使用原始文件中的 1 基序号，后续跳转或定位时可以直接复用。
fn build_memory_context_lines(
    lines: &[String],
    issue_line_number: u64,
    highlight_terms: &[String],
) -> Vec<WeaverMemoryContextLine> {
    if lines.is_empty() || issue_line_number == 0 {
        return Vec::new();
    }
    let issue_index = issue_line_number.saturating_sub(1) as usize;
    if issue_index >= lines.len() {
        return Vec::new();
    }
    let start = issue_index.saturating_sub(2);
    let end = (issue_index + 3).min(lines.len());
    (start..end)
        .map(|index| {
            let is_issue_line = index == issue_index;
            WeaverMemoryContextLine {
                line_number: index as u64 + 1,
                text: expand_log_tabs_for_display(&lines[index]),
                is_issue_line,
                highlight_terms: if is_issue_line {
                    highlight_terms.to_vec()
                } else {
                    Vec::new()
                },
            }
        })
        .collect()
}

/// 分析第一种 memory 行。
fn analyze_memory_format1_tokens(
    line: &str,
    line_number: u64,
    tokens: &[&str],
) -> Option<Option<WeaverMemoryIssue>> {
    if tokens.len() < 5 {
        return None;
    }
    let timestamp_text = format!("{} {}", tokens[0], tokens[1]);
    parse_memory_timestamp(&timestamp_text)?;
    let value = tokens.get(4)?.parse::<f64>().ok()?;
    Some((value > 90.0).then(|| WeaverMemoryIssue {
        line_number,
        timestamp: timestamp_text,
        reason: "第4列超过90".to_string(),
        value: format!("value={value:.2}"),
        line: truncate_log_line_summary(line),
        context_lines: Vec::new(),
        highlight_terms: vec![tokens[4].to_string()],
    }))
}

/// 分析第二种 memory 行。
fn analyze_memory_format2_line(
    line: &str,
    line_number: u64,
    previous: Option<&WeaverMemoryFormat2Sample>,
) -> Option<(Option<WeaverMemoryIssue>, WeaverMemoryFormat2Sample)> {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    if tokens.len() < 6 {
        return None;
    }
    let timestamp_text = format!("{} {}", tokens[0], tokens[1]);
    let timestamp = parse_memory_timestamp(&timestamp_text)?;
    let fgc_index = tokens.len().checked_sub(3)?;
    let fgct_index = tokens.len().checked_sub(2)?;
    let fgc_count = tokens.get(fgc_index)?.parse::<f64>().ok()?;
    let fgct_seconds = tokens.get(fgct_index)?.parse::<f64>().ok()?;
    let fgc_token = tokens.get(fgc_index)?.to_string();
    let fgct_token = tokens.get(fgct_index)?.to_string();
    let mut reasons = Vec::new();
    let mut values = Vec::new();
    let mut highlight_terms = Vec::new();
    let mut fgc_growth_streak = 0u32;
    if let Some(previous) = previous {
        let seconds = timestamp
            .signed_duration_since(previous.timestamp)
            .num_seconds()
            .abs();
        let fgc_delta = fgc_count - previous.fgc_count;
        let fgct_delta = fgct_seconds - previous.fgct_seconds;
        let is_short_interval_growth = seconds <= 60 && fgc_delta > 0.0;
        if is_short_interval_growth {
            fgc_growth_streak = previous.fgc_growth_streak.saturating_add(1);
        }
        if is_short_interval_growth && (fgc_delta >= 2.0 || fgc_growth_streak >= 2) {
            reasons.push("60秒内FGC连续增长".to_string());
            values.push(format!(
                "FGC {} -> {}，间隔 {} 秒",
                format_memory_number(previous.fgc_count),
                format_memory_number(fgc_count),
                seconds
            ));
            highlight_terms.push(fgc_token.clone());
        }
        if fgct_delta > 2.0 {
            reasons.push("单次FGCT增长超过2秒".to_string());
            values.push(format!("FGCT增量={fgct_delta:.2}s"));
            highlight_terms.push(fgct_token.clone());
        }
    }
    let sample = WeaverMemoryFormat2Sample {
        timestamp,
        timestamp_text: timestamp_text.clone(),
        fgc_count,
        fgct_seconds,
        fgc_growth_streak,
    };
    highlight_terms.sort();
    highlight_terms.dedup();

    let issue = (!reasons.is_empty()).then(|| WeaverMemoryIssue {
        line_number,
        timestamp: timestamp_text,
        reason: reasons.join("；"),
        value: values.join("；"),
        line: truncate_log_line_summary(line),
        context_lines: Vec::new(),
        highlight_terms,
    });
    Some((issue, sample))
}

/// 解析 memory 日志时间戳。
fn parse_memory_timestamp(timestamp: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(timestamp, "%Y-%m-%d %H:%M:%S").ok()
}

/// 格式化 memory 数值，整数不保留小数。
fn format_memory_number(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

/// 截断原始日志行摘要。
fn truncate_log_line_summary(line: &str) -> String {
    const MAX_CHARS: usize = 180;
    let display_line = expand_log_tabs_for_display(line);
    let mut summary = display_line.chars().take(MAX_CHARS).collect::<String>();
    if display_line.chars().count() > MAX_CHARS {
        summary.push_str("...");
    }
    summary
}

/// 将原始日志中的制表符展开为固定 4 个空格。
///
/// 业务意图：
/// - jstat 输出常用 `\t` 分隔列，但 GPUI 文本渲染中制表符宽度不可控，可能让相邻数值看起来黏在一起。
/// - 在插件输出层统一展开，保证异常摘要和上下文截图块都以稳定列距展示。
fn expand_log_tabs_for_display(line: &str) -> String {
    line.replace('\t', "    ")
}

/// 格式化连接池异常摘要。
fn format_pool_issue_summary(issue: &WeaverPoolIssue) -> String {
    if issue.start_line_number == issue.end_line_number {
        return format!(
            "异常行 {} [{}] {}；{}\n",
            issue.start_line_number, issue.start_timestamp, issue.reason, issue.value
        );
    }

    format!(
        "异常行 {} ～ {} [{} ～ {}] {}；{}\n",
        issue.start_line_number,
        issue.end_line_number,
        issue.start_timestamp,
        issue.end_timestamp,
        issue.reason,
        issue.value
    )
}

/// 分析连接池日志正文。
///
/// 业务意图：
/// - 连接池日志用于发现数据库连接耗尽、长时间高占用和定时任务引发的周期性连接波峰。
/// - 规则输出以问题段为单位，减少持续异常时的重复噪音，同时保留原始行上下文供用户核对。
///
/// 边界条件：
/// - 表头、空行、列数不足、时间或数字非法的行计入跳过，不中断后续分析。
/// - 多个连接池名称交错出现时，连续段和周期段不会跨连接池名称合并。
fn analyze_pool_lines(lines: Vec<String>) -> WeaverPoolAnalysis {
    let mut analysis = WeaverPoolAnalysis::default();
    let mut samples = Vec::<WeaverPoolSample>::new();
    for (index, line) in lines.iter().enumerate() {
        analysis.scanned_lines = analysis.scanned_lines.saturating_add(1);
        let line_number = index as u64 + 1;
        match parse_pool_sample(line, line_number) {
            Some(sample) => samples.push(sample),
            None => analysis.skipped_lines = analysis.skipped_lines.saturating_add(1),
        }
    }

    let mut issues = Vec::<WeaverPoolIssue>::new();
    issues.extend(build_pool_active_over_threshold_issues(
        &samples,
        &lines,
        50,
        "活跃连接数超过50",
        true,
    ));
    issues.extend(build_pool_active_over_threshold_issues(
        &samples,
        &lines,
        20,
        "活跃连接数连续超过20超过1分钟",
        false,
    ));
    issues.extend(build_pool_periodic_active_issues(&samples, &lines));
    issues.sort_by(|left, right| {
        left.start_line_number
            .cmp(&right.start_line_number)
            .then_with(|| left.end_line_number.cmp(&right.end_line_number))
            .then_with(|| left.reason.cmp(&right.reason))
    });

    for issue in issues {
        if let (Some(start), Some(end)) = (
            parse_memory_timestamp(&issue.start_timestamp),
            parse_memory_timestamp(&issue.end_timestamp),
        ) {
            analysis.problem_ranges.push(WeaverProblemTimeRange {
                start: start - ChronoDuration::seconds(60),
                end: end + ChronoDuration::seconds(60),
                line_number: issue.start_line_number,
                reason: issue.reason.clone(),
            });
        }
        analysis.issues.push(issue);
    }

    analysis
}

/// 解析连接池日志单行采样。
fn parse_pool_sample(line: &str, line_number: u64) -> Option<WeaverPoolSample> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let tokens = trimmed.split_whitespace().collect::<Vec<_>>();
    if tokens.len() < 6 {
        return None;
    }
    let timestamp_text = format!("{} {}", tokens[1], tokens[2]);
    let timestamp = parse_memory_timestamp(&timestamp_text)?;
    let active_token = tokens.get(3)?.to_string();
    let active_connections = active_token.parse::<u64>().ok()?;
    let default_connections = tokens.get(4)?.parse::<u64>().ok()?;
    let max_connections = tokens.get(5)?.parse::<u64>().ok()?;
    Some(WeaverPoolSample {
        line_number,
        pool_name: tokens[0].to_string(),
        timestamp,
        timestamp_text,
        active_connections,
        default_connections,
        max_connections,
        active_token,
    })
}

/// 构建连接池活跃连接阈值问题段。
///
/// 业务意图：
/// - `active > 50` 是即时严重告警，只要一个连续段中出现就输出。
/// - `active > 20` 需要持续至少 60 秒才告警，避免单次短波动误报。
fn build_pool_active_over_threshold_issues(
    samples: &[WeaverPoolSample],
    lines: &[String],
    threshold: u64,
    reason: &str,
    alert_immediately: bool,
) -> Vec<WeaverPoolIssue> {
    let mut issues = Vec::new();
    let mut segment = Vec::<WeaverPoolSample>::new();
    for sample in samples {
        if sample.active_connections > threshold {
            if segment
                .first()
                .is_some_and(|first| first.pool_name != sample.pool_name)
            {
                push_pool_threshold_segment_issue(
                    &segment,
                    lines,
                    threshold,
                    reason,
                    alert_immediately,
                    &mut issues,
                );
                segment.clear();
            }
            segment.push(sample.clone());
        } else {
            push_pool_threshold_segment_issue(
                &segment,
                lines,
                threshold,
                reason,
                alert_immediately,
                &mut issues,
            );
            segment.clear();
        }
    }
    push_pool_threshold_segment_issue(
        &segment,
        lines,
        threshold,
        reason,
        alert_immediately,
        &mut issues,
    );
    issues
}

/// 将一个阈值连续段转换为问题段。
fn push_pool_threshold_segment_issue(
    segment: &[WeaverPoolSample],
    lines: &[String],
    threshold: u64,
    reason: &str,
    alert_immediately: bool,
    issues: &mut Vec<WeaverPoolIssue>,
) {
    let (Some(first), Some(last)) = (segment.first(), segment.last()) else {
        return;
    };
    let duration_seconds = last
        .timestamp
        .signed_duration_since(first.timestamp)
        .num_seconds()
        .max(0);
    if !alert_immediately && duration_seconds < 60 {
        return;
    }
    let max_active = segment
        .iter()
        .map(|sample| sample.active_connections)
        .max()
        .unwrap_or(first.active_connections);
    let issue_lines = segment
        .iter()
        .map(|sample| (sample.line_number, sample.active_token.clone()))
        .collect::<Vec<_>>();
    let value = if alert_immediately {
        format!(
            "连接池 {}，active 最大 {}，阈值 {}，默认 {}，最大 {}",
            first.pool_name, max_active, threshold, first.default_connections, first.max_connections
        )
    } else {
        format!(
            "连接池 {}，持续 {} 秒，active 最大 {}，阈值 {}，默认 {}，最大 {}",
            first.pool_name,
            duration_seconds,
            max_active,
            threshold,
            first.default_connections,
            first.max_connections
        )
    };
    issues.push(build_pool_issue(
        first,
        last,
        reason.to_string(),
        value,
        issue_lines,
        lines,
    ));
}

/// 构建连接池周期性高活跃问题段。
///
/// 业务意图：
/// - 用户确认“每 3 分钟左右 active > 10”应告警；这里按 150～210 秒作为“左右”的判定窗口。
/// - 至少需要 3 个周期性波峰，避免两次偶发高连接被误判为周期性任务。
fn build_pool_periodic_active_issues(
    samples: &[WeaverPoolSample],
    lines: &[String],
) -> Vec<WeaverPoolIssue> {
    let bursts = build_pool_active_bursts(samples, 10);
    let mut issues = Vec::new();
    let mut group = Vec::<WeaverPoolActiveBurst>::new();
    for burst in bursts {
        if let Some(previous) = group.last() {
            let interval_seconds = burst
                .start
                .timestamp
                .signed_duration_since(previous.start.timestamp)
                .num_seconds();
            if previous.start.pool_name == burst.start.pool_name
                && (150..=210).contains(&interval_seconds)
            {
                group.push(burst);
                continue;
            }
            push_pool_periodic_group_issue(&group, lines, &mut issues);
            group.clear();
        }
        group.push(burst);
    }
    push_pool_periodic_group_issue(&group, lines, &mut issues);
    issues
}

/// 将 active 超过阈值的短时间连续采样压缩为波峰。
fn build_pool_active_bursts(
    samples: &[WeaverPoolSample],
    threshold: u64,
) -> Vec<WeaverPoolActiveBurst> {
    let mut bursts = Vec::new();
    let mut current: Option<WeaverPoolActiveBurst> = None;
    for sample in samples.iter().filter(|sample| sample.active_connections > threshold) {
        match current.as_mut() {
            Some(burst)
                if burst.start.pool_name == sample.pool_name
                    && sample
                        .timestamp
                        .signed_duration_since(burst.end.timestamp)
                        .num_seconds()
                        <= 60 =>
            {
                burst.end = sample.clone();
                burst
                    .issue_lines
                    .push((sample.line_number, sample.active_token.clone()));
                if sample.active_connections > burst.peak.active_connections {
                    burst.peak = sample.clone();
                }
            }
            Some(_) => {
                if let Some(finished) = current.take() {
                    bursts.push(finished);
                }
                current = Some(new_pool_active_burst(sample));
            }
            None => current = Some(new_pool_active_burst(sample)),
        }
    }
    if let Some(finished) = current {
        bursts.push(finished);
    }
    bursts
}

/// 构造单个连接池高活跃波峰。
fn new_pool_active_burst(sample: &WeaverPoolSample) -> WeaverPoolActiveBurst {
    WeaverPoolActiveBurst {
        start: sample.clone(),
        end: sample.clone(),
        peak: sample.clone(),
        issue_lines: vec![(sample.line_number, sample.active_token.clone())],
    }
}

/// 将周期性波峰组转换为问题段。
fn push_pool_periodic_group_issue(
    group: &[WeaverPoolActiveBurst],
    lines: &[String],
    issues: &mut Vec<WeaverPoolIssue>,
) {
    if group.len() < 3 {
        return;
    }
    let Some(first) = group.first() else {
        return;
    };
    let Some(last) = group.last() else {
        return;
    };
    let max_active = group
        .iter()
        .map(|burst| burst.peak.active_connections)
        .max()
        .unwrap_or(first.peak.active_connections);
    let issue_lines = group
        .iter()
        .flat_map(|burst| burst.issue_lines.iter().cloned())
        .collect::<Vec<_>>();
    issues.push(build_pool_issue(
        &first.start,
        &last.end,
        "活跃连接数每3分钟左右周期性超过10".to_string(),
        format!(
            "连接池 {}，周期次数 {}，active 峰值最大 {}",
            first.start.pool_name,
            group.len(),
            max_active
        ),
        issue_lines,
        lines,
    ));
}

/// 构建连接池问题段。
fn build_pool_issue(
    first: &WeaverPoolSample,
    last: &WeaverPoolSample,
    reason: String,
    value: String,
    issue_lines: Vec<(u64, String)>,
    lines: &[String],
) -> WeaverPoolIssue {
    WeaverPoolIssue {
        start_line_number: first.line_number,
        end_line_number: last.line_number,
        start_timestamp: first.timestamp_text.clone(),
        end_timestamp: last.timestamp_text.clone(),
        reason,
        value,
        context_lines: build_pool_context_lines(lines, &issue_lines),
    }
}

/// 构建连接池问题段上下文。
///
/// 边界条件：
/// - 周期性问题可能跨越多分钟，不直接展示首尾之间所有行，而是合并每个异常采样前后各 2 行，避免大日志输出过长。
/// - 行号去重后保持升序；同一行多个高亮词会去重，避免 UI 重复高亮。
fn build_pool_context_lines(
    lines: &[String],
    issue_lines: &[(u64, String)],
) -> Vec<WeaverPoolContextLine> {
    let mut issue_terms = BTreeMap::<u64, Vec<String>>::new();
    for (line_number, term) in issue_lines {
        let terms = issue_terms.entry(*line_number).or_default();
        if !terms.iter().any(|existing| existing == term) {
            terms.push(term.clone());
        }
    }

    let mut by_line = BTreeMap::<u64, WeaverPoolContextLine>::new();
    for line_number in issue_terms.keys().copied() {
        if line_number == 0 {
            continue;
        }
        let issue_index = line_number.saturating_sub(1) as usize;
        if issue_index >= lines.len() {
            continue;
        }
        let start = issue_index.saturating_sub(2);
        let end = (issue_index + 3).min(lines.len());
        for index in start..end {
            let current_line_number = index as u64 + 1;
            let highlight_terms = issue_terms
                .get(&current_line_number)
                .cloned()
                .unwrap_or_default();
            by_line
                .entry(current_line_number)
                .or_insert_with(|| WeaverPoolContextLine {
                    line_number: current_line_number,
                    text: expand_log_tabs_for_display(&lines[index]),
                    is_issue_line: !highlight_terms.is_empty(),
                    highlight_terms,
                });
        }
    }
    by_line.into_values().collect()
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
    report_progress: F,
) -> PluginCommandResponse
where
    F: FnMut(PluginCommandProgress),
{
    build_performance_list_response_with_filter(files, BTreeMap::new(), report_progress)
}

/// 构造带业务过滤条件的性能列表解析窗口响应。
///
/// 业务意图：
/// - 汇总页过滤后必须基于过滤后的请求重新计算请求次数和平均耗时，而不是只隐藏已有汇总行。
/// - 插件仍只解析文件名元数据，不读取正文；大文件正文读取继续留给 SQL 下钻命令。
fn build_performance_list_response_with_filter<F>(
    files: Vec<PluginLogFile>,
    data: BTreeMap<String, String>,
    mut report_progress: F,
) -> PluginCommandResponse
where
    F: FnMut(PluginCommandProgress),
{
    let filter = match WeaverPerformanceFilter::from_data(&data) {
        Ok(filter) => filter,
        Err(message) => return PluginCommandResponse::Error { message },
    };
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
    let filtered_rows = rows
        .into_iter()
        .filter(|row| filter.matches(row))
        .collect::<Vec<_>>();
    let filtered_count = filtered_rows.len();
    let summaries = build_route_summaries(filtered_rows);
    let (table_rows, row_actions) = build_summary_table_rows(&summaries, &filter);
    let route_count = summaries.len();
    PluginCommandResponse::OpenWindow {
        title: "性能列表解析".to_string(),
        page: PluginPage {
            title: "性能列表解析".to_string(),
            description: if matched_count == 0 {
                Some("选中范围内没有符合泛微性能日志文件名格式的日志。".to_string())
            } else if filter.is_active() && filtered_count == 0 {
                Some("当前用户或请求时间区间过滤后没有匹配的性能日志。".to_string())
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
                    label: "过滤后请求".to_string(),
                    value: filtered_count.to_string(),
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
            output_steps: Vec::new(),
            table: Some(PluginPageTable {
                headers: vec![
                    "请求地址".to_string(),
                    "请求次数".to_string(),
                    "平均耗时(ms)".to_string(),
                    "操作".to_string(),
                ],
                rows: table_rows,
                command_filter: Some(build_summary_command_filter(&filter)),
                row_actions,
            }),
        },
    }
}

/// 构造单个请求地址的性能详情窗口响应。
///
/// 业务意图：
/// - 汇总页首次返回时只展示每个请求地址的统计值；详情页在用户点击“详情”后按请求地址重新筛选当次授权的日志快照。
/// - 这样可以保留详情下钻能力，同时避免把所有请求地址的明细表格都提前序列化到首个响应中。
///
/// 边界条件：
/// - 请求地址来自行按钮上下文，缺失时直接返回中文错误，避免空条件把整批日志都展示成详情。
/// - 当前只解析文件名元数据，不读取日志正文；SQL 正文仍由更深一层的“显示SQL”按钮按单文件延迟读取。
fn build_performance_detail_response<F>(
    files: Vec<PluginLogFile>,
    data: BTreeMap<String, String>,
    mut report_progress: F,
) -> PluginCommandResponse
where
    F: FnMut(PluginCommandProgress),
{
    let filter = match WeaverPerformanceFilter::from_data(&data) {
        Ok(filter) => filter,
        Err(message) => return PluginCommandResponse::Error { message },
    };
    let route = data
        .get(PERFORMANCE_FILTER_ROUTE_KEY)
        .cloned()
        .unwrap_or_default();
    if route.trim().is_empty() {
        return PluginCommandResponse::Error {
            message: "性能详情缺少请求地址".to_string(),
        };
    }

    let scanned_count = files.len();
    report_progress(PluginCommandProgress {
        message: "正在生成性能详情".to_string(),
        detail: Some(format!("请求地址：{route}")),
        done: 0,
        total: Some(scanned_count as u64),
        unit: Some("文件".to_string()),
    });

    let mut logs = Vec::new();
    for (index, file) in files.into_iter().enumerate() {
        if let Some(log) = parse_weaver_performance_log(&file) {
            if log.route == route && filter.matches(&log) {
                logs.push(log);
            }
        }
        let done = index + 1;
        if should_report_progress(done, scanned_count) {
            report_progress(PluginCommandProgress {
                message: "正在生成性能详情".to_string(),
                detail: Some(file.display_name),
                done: done as u64,
                total: Some(scanned_count as u64),
                unit: Some("文件".to_string()),
            });
        }
    }
    logs.sort_by(compare_detail_logs);
    report_progress(PluginCommandProgress {
        message: "正在整理性能详情".to_string(),
        detail: Some(format!("共匹配 {} 条请求", logs.len())),
        done: scanned_count as u64,
        total: Some(scanned_count as u64),
        unit: Some("文件".to_string()),
    });

    let summary = WeaverRouteSummary {
        route: route.clone(),
        logs,
    };
    PluginCommandResponse::OpenWindow {
        title: format!("请求详情 - {route}"),
        page: build_detail_page(&summary, &filter),
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
    filter: &WeaverPerformanceFilter,
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
        let mut data = BTreeMap::new();
        data.insert(PERFORMANCE_FILTER_ROUTE_KEY.to_string(), summary.route.clone());
        filter.write_to_data(&mut data);
        // 汇总页只返回轻量下钻命令，不再提前构造完整详情页。
        // 大量日志时，每个请求地址内嵌一份详情表格会让首个 JSON 响应和宿主列宽初始化都膨胀；
        // 点击时由宿主补齐当前窗口的日志快照，再让插件按请求地址生成详情。
        row_actions.push(vec![PluginTableRowAction {
            label: "详情".to_string(),
            title: Some(format!("请求详情 - {}", summary.route)),
            page: None,
            command: Some(PluginTableRowCommand {
                command_id: "show_performance_detail".to_string(),
                context: PluginCommandContext::TableAction {
                    action_id: "show_detail".to_string(),
                    files: Vec::new(),
                    data,
                },
            }),
        }]);
    }
    (table_rows, row_actions)
}

/// 构造性能汇总页的声明式过滤器定义。
fn build_summary_command_filter(filter: &WeaverPerformanceFilter) -> PluginTableCommandFilter {
    let mut data = BTreeMap::new();
    filter.write_to_data(&mut data);
    PluginTableCommandFilter {
        controls: build_performance_filter_controls(filter),
        command: PluginTableRowCommand {
            command_id: "filter_performance_list".to_string(),
            context: PluginCommandContext::TableAction {
                action_id: "apply_filter".to_string(),
                files: Vec::new(),
                data,
            },
        },
    }
}

/// 构造请求详情页的声明式过滤器定义。
fn build_detail_performance_filter(
    route: &str,
    filter: &WeaverPerformanceFilter,
) -> PluginTableCommandFilter {
    let mut data = BTreeMap::new();
    data.insert(PERFORMANCE_FILTER_ROUTE_KEY.to_string(), route.to_string());
    filter.write_to_data(&mut data);
    PluginTableCommandFilter {
        controls: build_performance_filter_controls(filter),
        command: PluginTableRowCommand {
            command_id: "show_performance_detail".to_string(),
            context: PluginCommandContext::TableAction {
                action_id: "show_detail".to_string(),
                files: Vec::new(),
                data,
            },
        },
    }
}

/// 构造性能过滤控件声明。
///
/// 业务意图：
/// - 插件把字段名、占位符、图标和宽度完整声明给宿主，宿主无需了解这些字段属于泛微性能日志。
fn build_performance_filter_controls(
    filter: &WeaverPerformanceFilter,
) -> Vec<PluginTableCommandFilterControl> {
    vec![
        PluginTableCommandFilterControl {
            key: PERFORMANCE_FILTER_USERS_KEY.to_string(),
            value: filter.users_raw.clone(),
            placeholder: "用户：alice,bob".to_string(),
            icon: Some("user".to_string()),
            width: Some(180),
        },
        PluginTableCommandFilterControl {
            key: PERFORMANCE_FILTER_START_KEY.to_string(),
            value: filter.start_time_raw.clone(),
            placeholder: "开始：yyyy-MM-dd HH:mm:ss".to_string(),
            icon: None,
            width: Some(220),
        },
        PluginTableCommandFilterControl {
            key: PERFORMANCE_FILTER_END_KEY.to_string(),
            value: filter.end_time_raw.clone(),
            placeholder: "结束：yyyy-MM-dd HH:mm:ss".to_string(),
            icon: None,
            width: Some(220),
        },
    ]
}

/// 构建某个请求地址的详情页。
///
/// 业务意图：
/// - 详情页展示该请求地址下的每一次请求，按耗时从高到低排列，便于定位慢请求对应用户和时间。
/// - SQL 明细通过行内延迟命令读取单个日志正文，不在首个响应中预先解析所有文件，避免大目录结果膨胀。
fn build_detail_page(summary: &WeaverRouteSummary, filter: &WeaverPerformanceFilter) -> PluginPage {
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
        output_steps: Vec::new(),
        table: Some(PluginPageTable {
            headers: vec![
                "请求路径".to_string(),
                "耗时(ms)".to_string(),
                "用户".to_string(),
                "请求时间".to_string(),
                "操作".to_string(),
            ],
            rows: detail_rows,
            command_filter: Some(build_detail_performance_filter(&summary.route, filter)),
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
        Ok(raw) => {
            println!("{raw}");
            let _ = io::stdout().flush();
        }
        Err(error) => eprintln!("序列化插件进度失败：{error}"),
    }
}

/// 输出步骤开始事件。
fn emit_output_step_start(step: PluginOutputStep) {
    let event = PluginCommandEvent::OutputStepStart { step };
    match serde_json::to_string(&event) {
        Ok(raw) => {
            println!("{raw}");
            let _ = io::stdout().flush();
        }
        Err(error) => eprintln!("序列化插件步骤开始事件失败：{error}"),
    }
}

/// 输出步骤正文追加事件。
fn emit_output_step_append(step_id: String, text: String) {
    let event = PluginCommandEvent::OutputStepAppend { step_id, text };
    match serde_json::to_string(&event) {
        Ok(raw) => {
            println!("{raw}");
            let _ = io::stdout().flush();
        }
        Err(error) => eprintln!("序列化插件步骤追加事件失败：{error}"),
    }
}

/// 输出步骤结束事件。
fn emit_output_step_finish(step_id: String, done_text: String, status: PluginOutputStepStatus) {
    let event = PluginCommandEvent::OutputStepFinish {
        step_id,
        done_text,
        status,
    };
    match serde_json::to_string(&event) {
        Ok(raw) => {
            println!("{raw}");
            let _ = io::stdout().flush();
        }
        Err(error) => eprintln!("序列化插件步骤结束事件失败：{error}"),
    }
}

/// 请求宿主写入指定日志来源正文。
fn emit_log_content_request(file: &PluginLogFile, relative_path: &str) {
    let event = PluginCommandEvent::LogContentRequest {
        source_key: file.source_key.clone(),
        path_label: Some(relative_path.to_string()),
    };
    match serde_json::to_string(&event) {
        Ok(raw) => {
            println!("{raw}");
            let _ = io::stdout().flush();
        }
        Err(error) => eprintln!("序列化日志正文请求失败：{error}"),
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

/// 解析性能过滤时间为本地时区毫秒时间戳。
///
/// 业务意图：
/// - 性能文件名只保存毫秒时间戳，用户输入按本机展示时间理解；解析和展示都使用 `Local`，避免同一条件在不同平台格式化路径中分叉。
///
/// 边界条件：
/// - 空文本表示不限制该边界。
/// - 结束时间输入精确到秒，但日志时间戳精确到毫秒；结束边界补到该秒的最后一毫秒，保证闭区间符合用户直觉。
fn parse_filter_time_millis(
    raw: &str,
    label: &str,
    end_of_second: bool,
) -> Result<Option<i64>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let naive = NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S")
        .map_err(|_| format!("{label}格式错误，请使用 yyyy-MM-dd HH:mm:ss"))?;
    let local = Local
        .from_local_datetime(&naive)
        .single()
        .ok_or_else(|| format!("{label}不是有效的本地时间"))?;
    let millis = local.timestamp_millis();
    Ok(Some(if end_of_second { millis + 999 } else { millis }))
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
            output_steps: Vec::new(),
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
                command_filter: None,
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
            Ok(PluginStdinEvent::LogContentBegin { .. })
            | Ok(PluginStdinEvent::LogContentEnd { .. }) => {}
            Ok(PluginStdinEvent::LogContentError { message, .. }) => {
                return Err(format!("宿主读取日志正文失败：{message}"));
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
    use std::io::Cursor;

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

    /// 将毫秒时间戳转换成过滤输入使用的秒级本地时间文本。
    fn filter_second(timestamp_ms: i64) -> String {
        format_request_time_ms(timestamp_ms)
            .chars()
            .take(19)
            .collect()
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

    /// 构造多个文件顺序返回的宿主交互式正文流。
    ///
    /// 业务意图：
    /// - E9 分析会按步骤依次请求 memory、连接池等不同日志正文；测试流必须按请求顺序模拟宿主回写。
    /// - 每个文件都用 begin/line/end 包裹，覆盖插件对 source_key 的隔离读取。
    fn log_content_stream(files: &[(&PluginLogFile, &[&str])]) -> Cursor<String> {
        let mut raw = String::new();
        for (file, lines) in files {
            raw.push_str(
                &serde_json::json!({
                    "event": "log_content_begin",
                    "source_key": &file.source_key,
                    "path_label": &file.path_label,
                })
                .to_string(),
            );
            raw.push('\n');
            for line in *lines {
                raw.push_str(
                    &serde_json::json!({
                        "event": "log_content_line",
                        "line": line,
                    })
                    .to_string(),
                );
                raw.push('\n');
            }
            raw.push_str(
                &serde_json::json!({
                    "event": "log_content_end",
                    "source_key": &file.source_key,
                    "lines": lines.len(),
                })
                .to_string(),
            );
            raw.push('\n');
        }
        Cursor::new(raw)
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
    fn e9扫描只分析memory和连接池并输出步骤流结果() {
        let memory_file = file_at("root/memory_2026-05-23.log");
        let pool_file = file_at("root/pool_20260523_ecology.log");
        let memory_lines = [
            "2026-05-22 00:00:01 100 83.33 42.4 0 0 0 0 0",
            "2026-05-22 00:00:07 100 46.03 91.5 0 0 0 0 0",
        ];
        let pool_lines = [
            "ecology\t2026-05-22 00:00:09\t0\t50\t300\t10994294\t0\t2026-05-18 11:27:43",
            "ecology\t2026-05-22 00:00:19\t51\t50\t300\t10994313\t0\t2026-05-18 11:27:43",
        ];
        let mut content =
            log_content_stream(&[(&memory_file, &memory_lines), (&pool_file, &pool_lines)]);
        let mut step_starts = Vec::new();
        let mut step_appends = Vec::new();
        let mut step_finishes = Vec::new();
        let response = build_weaver_log_scan_response(
            vec![
                file_at("root/stdout.20260523.log"),
                memory_file,
                pool_file,
                file_at("root/WEB-INF/web.xml"),
                file_at("root/runtime/2026_05_23/a.log"),
            ],
            BTreeMap::new(),
            &mut content,
            |step| step_starts.push(step),
            |step_id, text| step_appends.push((step_id, text)),
            |step_id, done_text, status| step_finishes.push((step_id, done_text, status)),
        );
        let PluginCommandResponse::OpenWindow { page, .. } = response else {
            panic!("应返回 E9 日志分析窗口");
        };

        assert!(page.stats.is_empty());
        assert!(page.table.is_none());
        assert_eq!(page.output_steps.len(), 2);
        let memory_step = &page.output_steps[0];
        assert_eq!(memory_step.id, "memory");
        assert_eq!(memory_step.status, PluginOutputStepStatus::Completed);
        assert!(memory_step.content.contains("扫描到 1 个 memory 日志"));
        assert!(memory_step.content.contains("root/memory_2026-05-23.log"));
        assert!(!memory_step.content.contains("stdout.20260523.log"));
        assert!(!memory_step.content.contains("WEB-INF/web.xml"));
        assert!(!memory_step.content.contains("runtime/2026_05_23/a.log"));
        assert!(!memory_step.content.contains("开始分析："));
        assert!(!memory_step.content.contains("原始："));
        assert!(memory_step.content.contains("第4列超过90"));
        assert!(memory_step.content.contains("完成：扫描 2 行"));
        let pool_step = &page.output_steps[1];
        assert_eq!(pool_step.id, "pool");
        assert_eq!(pool_step.status, PluginOutputStepStatus::Completed);
        assert!(pool_step.content.contains("扫描到 1 个连接池日志"));
        assert!(pool_step
            .content
            .contains("root/pool_20260523_ecology.log"));
        assert!(pool_step.content.contains("活跃连接数超过50"));
        assert!(pool_step.content.contains("完成：扫描 2 行"));
        assert_eq!(step_starts.len(), 2);
        assert_eq!(step_appends[0].0, "memory");
        assert!(step_appends.iter().any(|(step_id, _)| step_id == "pool"));
        assert_eq!(step_finishes.len(), 2);
        assert_eq!(step_finishes[0].1, "memory日志已分析完毕");
        assert_eq!(step_finishes[0].2, PluginOutputStepStatus::Completed);
        assert_eq!(step_finishes[1].1, "连接池日志已分析完毕");
        assert_eq!(step_finishes[1].2, PluginOutputStepStatus::Completed);
    }

    #[test]
    fn 连接池活跃连接超过50立即告警() {
        let analysis = analyze_pool_lines(vec![
            "ecology\t2026-05-22 00:00:09\t0\t50\t300\t10994294\t0\t2026-05-18 11:27:43"
                .to_string(),
            "ecology\t2026-05-22 00:00:19\t51\t50\t300\t10994313\t0\t2026-05-18 11:27:43"
                .to_string(),
        ]);

        assert_eq!(analysis.scanned_lines, 2);
        assert_eq!(analysis.skipped_lines, 0);
        assert_eq!(analysis.issues.len(), 1);
        assert_eq!(analysis.issues[0].start_line_number, 2);
        assert!(analysis.issues[0].reason.contains("超过50"));
        assert!(analysis.issues[0].value.contains("active 最大 51"));
    }

    #[test]
    fn 连接池活跃连接连续超过20一分钟告警() {
        let analysis = analyze_pool_lines(vec![
            "ecology 2026-05-22 00:00:00 21 50 300 1 0 2026-05-18 11:27:43".to_string(),
            "ecology 2026-05-22 00:00:10 22 50 300 1 0 2026-05-18 11:27:43".to_string(),
            "ecology 2026-05-22 00:00:20 23 50 300 1 0 2026-05-18 11:27:43".to_string(),
            "ecology 2026-05-22 00:00:30 24 50 300 1 0 2026-05-18 11:27:43".to_string(),
            "ecology 2026-05-22 00:00:40 25 50 300 1 0 2026-05-18 11:27:43".to_string(),
            "ecology 2026-05-22 00:00:50 26 50 300 1 0 2026-05-18 11:27:43".to_string(),
            "ecology 2026-05-22 00:01:00 27 50 300 1 0 2026-05-18 11:27:43".to_string(),
        ]);

        assert_eq!(analysis.issues.len(), 1);
        assert_eq!(analysis.issues[0].start_line_number, 1);
        assert_eq!(analysis.issues[0].end_line_number, 7);
        assert!(analysis.issues[0].reason.contains("连续超过20"));
        assert!(analysis.issues[0].value.contains("持续 60 秒"));
    }

    #[test]
    fn 连接池活跃连接超过20不足一分钟不告警() {
        let analysis = analyze_pool_lines(vec![
            "ecology 2026-05-22 00:00:00 21 50 300 1 0 2026-05-18 11:27:43".to_string(),
            "ecology 2026-05-22 00:00:30 22 50 300 1 0 2026-05-18 11:27:43".to_string(),
            "ecology 2026-05-22 00:00:50 23 50 300 1 0 2026-05-18 11:27:43".to_string(),
        ]);

        assert!(
            analysis.issues.is_empty(),
            "active > 20 但首尾不足 60 秒时不应告警"
        );
    }

    #[test]
    fn 连接池活跃连接每三分钟左右周期性超过10告警() {
        let analysis = analyze_pool_lines(vec![
            "ecology 2026-05-22 00:00:00 11 50 300 1 0 2026-05-18 11:27:43".to_string(),
            "ecology 2026-05-22 00:00:10 12 50 300 1 0 2026-05-18 11:27:43".to_string(),
            "ecology 2026-05-22 00:03:00 13 50 300 1 0 2026-05-18 11:27:43".to_string(),
            "ecology 2026-05-22 00:03:10 14 50 300 1 0 2026-05-18 11:27:43".to_string(),
            "ecology 2026-05-22 00:06:00 15 50 300 1 0 2026-05-18 11:27:43".to_string(),
        ]);

        assert_eq!(analysis.issues.len(), 1);
        assert!(analysis.issues[0].reason.contains("每3分钟左右"));
        assert!(analysis.issues[0].value.contains("周期次数 3"));
        assert_eq!(analysis.problem_ranges.len(), 1);
    }

    #[test]
    fn 连接池非法行计入跳过且不中断后续分析() {
        let analysis = analyze_pool_lines(vec![
            "连接池名称 时间 活跃连接数 默认连接数 最大连接数 忽略 忽略 忽略".to_string(),
            "bad line".to_string(),
            "ecology 2026-05-22 00:00:19 51 50 300 10994313 0 2026-05-18 11:27:43"
                .to_string(),
        ]);

        assert_eq!(analysis.scanned_lines, 3);
        assert_eq!(analysis.skipped_lines, 2);
        assert_eq!(analysis.issues.len(), 1);
    }

    #[test]
    fn memory第一种格式第4逻辑列超过90命中() {
        let analysis = analyze_memory_lines(vec![
            "2026-05-22 00:00:01 100 83.33 42.4 0 0 0 0 0".to_string(),
            "2026-05-22 00:00:07 100 46.03 91.5 0 0 0 0 0".to_string(),
        ]);

        assert_eq!(analysis.scanned_lines, 2);
        assert_eq!(analysis.skipped_lines, 0);
        assert_eq!(analysis.issues.len(), 1);
        assert_eq!(analysis.issues[0].line_number, 2);
        assert!(analysis.issues[0].reason.contains("第4列超过90"));
    }

    #[test]
    fn memory第二种格式识别fgc增长和fgct耗时() {
        let analysis = analyze_memory_lines(vec![
            "2026-05-22 00:00:04\t46.14\t98.48\t62.97\t\t850\t159.6\t4\t1.17\t160.77".to_string(),
            "2026-05-22 00:00:09\t46.14\t98.56\t62.97\t\t850\t159.6\t5\t1.80\t160.77".to_string(),
            "2026-05-22 00:00:14\t46.14\t98.85\t62.97\t\t850\t159.6\t6\t1.95\t160.77".to_string(),
            "2026-05-22 00:00:19\t46.14\t98.85\t62.97\t\t850\t159.6\t6\t4.95\t160.77".to_string(),
            "bad line".to_string(),
        ]);

        assert_eq!(analysis.scanned_lines, 5);
        assert_eq!(analysis.skipped_lines, 1);
        assert_eq!(analysis.issues.len(), 2);
        assert!(analysis.issues[0].reason.contains("FGC连续增长"));
        assert!(analysis.issues[1].reason.contains("FGCT增长超过2秒"));
    }

    #[test]
    fn memory第二种单次fgc增长但fgct不超过2秒不报警() {
        let analysis = analyze_memory_lines(vec![
            "2026-05-22 00:16:25 94.15 19.86 92.8 1290 204.13 11 6.73 210.86"
                .to_string(),
            "2026-05-22 00:16:32 0 16.84 23.94 1291 204.22 12 7.91 212.13".to_string(),
        ]);

        assert_eq!(analysis.scanned_lines, 2);
        assert!(
            analysis.issues.is_empty(),
            "FGC 只增长 1 次且 FGCT 增量约 1.18 秒时不应报警"
        );
    }

    #[test]
    fn memory第二种空格分隔不会套用第4逻辑列阈值() {
        let analysis = analyze_memory_lines(vec![
            "2026-05-22 00:00:04 46.14 98.48 99.97 850 159.6 4 1.17 160.77".to_string(),
            "2026-05-22 00:00:09 46.14 98.56 99.98 850 159.6 4 1.80 160.77".to_string(),
        ]);

        assert_eq!(analysis.scanned_lines, 2);
        assert_eq!(analysis.skipped_lines, 0);
        assert!(
            analysis.issues.is_empty(),
            "第二种格式只应检查 FGC 增长和 FGCT 增量，不应因为第4逻辑列超过90报警"
        );
    }

    #[test]
    fn memory第二种同一文件不会混入第一种告警() {
        let analysis = analyze_memory_lines(vec![
            "2026-05-21 02:22:06 46.41 48.9 34.5 27700 1633.41 58 129.68 1763.09".to_string(),
            "2026-05-21 02:22:11 46.41 48.9 34.5 27700 1633.41 59 129.68 1763.09".to_string(),
            "2026-05-21 05:45:55 6 27.35 90.15 28231 1685.21 61 141.58 1826.79".to_string(),
        ]);

        assert_eq!(analysis.issues.len(), 1);
        assert!(analysis.issues[0].reason.contains("FGCT增长超过2秒"));
        assert!(
            analysis
                .issues
                .iter()
                .all(|issue| !issue.reason.contains("第4列超过90")),
            "第二种 memory 文件内不能再套用第一种第4逻辑列阈值"
        );
    }

    #[test]
    fn memory异常生成上下文和问题时间段() {
        let analysis = analyze_memory_lines(vec![
            "2026-05-22 00:00:01 100 83.33 42.4 0 0 0 0 0".to_string(),
            "2026-05-22 00:00:07 100 46.03 42.39 0 0 0 0 0".to_string(),
            "2026-05-22 00:00:12 100 87.3 91.2 0 0 0 0 0".to_string(),
            "2026-05-22 00:00:17 100 14.62 42.37 0 0 0 0 0".to_string(),
            "2026-05-22 00:00:22 100 14.62 42.37 0 0 0 0 0".to_string(),
        ]);

        assert_eq!(analysis.issues.len(), 1);
        let issue = &analysis.issues[0];
        assert_eq!(
            issue
                .context_lines
                .iter()
                .map(|line| line.line_number)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        assert!(issue.context_lines[2].is_issue_line);
        assert_eq!(issue.context_lines[2].highlight_terms, vec!["91.2"]);
        assert_eq!(analysis.problem_ranges.len(), 1);
        assert_eq!(
            analysis.problem_ranges[0].start,
            parse_memory_timestamp("2026-05-22 00:00:12").unwrap() - ChronoDuration::seconds(60)
        );
        assert_eq!(
            analysis.problem_ranges[0].end,
            parse_memory_timestamp("2026-05-22 00:00:12").unwrap() + ChronoDuration::seconds(60)
        );
    }

    #[test]
    fn memory连续异常即使相邻时间超过一分钟也合并为一个展示区间() {
        let analysis = analyze_memory_lines(vec![
            "2026-05-22 07:57:24 100 20.94 99.94 0 0 0 0 0".to_string(),
            "2026-05-22 07:58:45 100 44.35 99.96 0 0 0 0 0".to_string(),
            "2026-05-22 08:00:12 100 46.61 99.96 0 0 0 0 0".to_string(),
            "2026-05-22 08:00:17 100 54.31 42.39 0 0 0 0 0".to_string(),
            "2026-05-22 08:02:00 100 54.31 99.94 0 0 0 0 0".to_string(),
        ]);

        assert_eq!(analysis.issues.len(), 4);
        let groups = group_memory_issues_for_display(&analysis.issues);

        assert_eq!(
            groups.len(),
            2,
            "前三行异常连续，应合并；第 4 行正常后第 5 行异常，应形成新的异常段"
        );
        assert_eq!(groups[0].start_line_number, 1);
        assert_eq!(groups[0].end_line_number, 3);
        assert_eq!(groups[0].start_timestamp, "2026-05-22 07:57:24");
        assert_eq!(groups[0].end_timestamp, "2026-05-22 08:00:12");
        assert_eq!(groups[0].issue_count, 3);
        assert!(format_memory_issue_group_summary(&groups[0]).contains(
            "异常行 1 ～ 3 [2026-05-22 07:57:24 ～ 2026-05-22 08:00:12]"
        ));
        assert_eq!(groups[1].start_line_number, 5);
        assert_eq!(groups[1].end_line_number, 5);
    }

    #[test]
    fn memory原始日志展示会展开制表符() {
        let analysis = analyze_memory_lines(vec![
            "2026-05-22 00:00:04\t46.14\t98.48\t62.97\t\t850\t159.6\t4\t1.17\t160.77"
                .to_string(),
            "2026-05-22 00:00:09\t46.14\t98.56\t62.97\t\t850\t159.6\t5\t4.00\t160.77"
                .to_string(),
        ]);

        assert_eq!(analysis.issues.len(), 1);
        assert!(
            analysis.issues[0].line.contains("    46.14    "),
            "异常摘要中的原始日志应把 tab 展开为 4 个空格"
        );
        assert!(
            analysis.issues[0].context_lines[1]
                .text
                .contains("    46.14    "),
            "上下文截图行应把 tab 展开为 4 个空格"
        );
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
        assert!(table.row_actions[0][0].page.is_none());
        let command = table.row_actions[0][0]
            .command
            .as_ref()
            .expect("汇总行详情应改为延迟命令，避免首个响应内嵌全部明细");
        assert_eq!(command.command_id, "show_performance_detail");
        let PluginCommandContext::TableAction {
            action_id,
            files,
            data,
        } = &command.context
        else {
            panic!("详情命令应使用表格动作上下文");
        };
        assert_eq!(action_id, "show_detail");
        assert!(files.is_empty());
        assert_eq!(data.get("route").map(String::as_str), Some("/a"));
        assert!(table.command_filter.is_some());
    }

    /// 覆盖性能汇总业务过滤会重新计算次数和平均耗时。
    ///
    /// 业务意图：
    /// - 用户过滤不是隐藏汇总行，而是先筛选原始请求再重新聚合，否则请求次数和平均耗时会误导用户。
    #[test]
    fn 性能汇总按用户和时间过滤后重新聚合() {
        let mut data = BTreeMap::new();
        data.insert(PERFORMANCE_FILTER_USERS_KEY.to_string(), "ali,bob".to_string());
        data.insert(
            PERFORMANCE_FILTER_START_KEY.to_string(),
            filter_second(1778661627000),
        );
        data.insert(
            PERFORMANCE_FILTER_END_KEY.to_string(),
            filter_second(1778661628000),
        );
        let response = build_performance_list_response_with_filter(
            vec![
                file("100&alice&_a&1778661627000&0&0.log"),
                file("500&bob&_a&1778661628000&0&0.log"),
                file("900&carol&_a&1778661629000&0&0.log"),
                file("700&carol&_b&1778661630000&0&0.log"),
            ],
            data,
            |_| {},
        );
        let PluginCommandResponse::OpenWindow { page, .. } = response else {
            panic!("应返回窗口");
        };
        let table = page.table.expect("汇总页应包含表格");
        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.rows[0][0], "/a");
        assert_eq!(table.rows[0][1], "2");
        assert_eq!(table.rows[0][2], "300.0");
        let filter = table.command_filter.expect("汇总页应携带业务过滤器");
        assert_eq!(filter.controls[0].key, PERFORMANCE_FILTER_USERS_KEY);
        assert_eq!(filter.controls[0].value, "ali,bob");
        let action_data = match &table.row_actions[0][0]
            .command
            .as_ref()
            .expect("详情动作应携带命令")
            .context
        {
            PluginCommandContext::TableAction { data, .. } => data,
            _ => panic!("详情命令应使用表格动作上下文"),
        };
        assert_eq!(
            action_data
                .get(PERFORMANCE_FILTER_USERS_KEY)
                .map(String::as_str),
            Some("ali,bob")
        );
    }

    /// 覆盖性能详情延迟命令按请求地址筛选并排序。
    ///
    /// 业务意图：
    /// - 汇总页不再内嵌详情页，因此详情排序规则必须通过新的延迟命令路径继续保持不变。
    #[test]
    fn 详情页按耗时降序并显示用户() {
        let mut data = BTreeMap::new();
        data.insert("route".to_string(), "/a".to_string());
        let response = build_performance_detail_response(
            vec![
                file("100&alice&_a&1778661627833&0&0.log"),
                file("500&bob&_a&1778661627834&0&0.log"),
                file("900&carol&_b&1778661627835&0&0.log"),
            ],
            data,
            |_| {},
        );
        let PluginCommandResponse::OpenWindow { page, .. } = response else {
            panic!("应返回窗口");
        };
        let detail_table = page.table.as_ref().expect("详情页应包含表格");

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
        assert!(detail_table.command_filter.is_some());
    }

    /// 覆盖请求详情继承并应用汇总页过滤条件。
    ///
    /// 业务意图：
    /// - 从汇总页进入详情后，用户和请求时间区间必须继续生效，避免详情页突然展示未过滤的全部请求。
    #[test]
    fn 请求详情继承用户和时间过滤() {
        let mut data = BTreeMap::new();
        data.insert(PERFORMANCE_FILTER_ROUTE_KEY.to_string(), "/a".to_string());
        data.insert(PERFORMANCE_FILTER_USERS_KEY.to_string(), "bob".to_string());
        data.insert(
            PERFORMANCE_FILTER_START_KEY.to_string(),
            filter_second(1778661628000),
        );
        data.insert(
            PERFORMANCE_FILTER_END_KEY.to_string(),
            filter_second(1778661628000),
        );
        let response = build_performance_detail_response(
            vec![
                file("100&alice&_a&1778661627000&0&0.log"),
                file("500&bob&_a&1778661628000&0&0.log"),
                file("900&bob&_a&1778661629000&0&0.log"),
            ],
            data,
            |_| {},
        );
        let PluginCommandResponse::OpenWindow { page, .. } = response else {
            panic!("应返回窗口");
        };
        let detail_table = page.table.expect("详情页应包含表格");
        assert_eq!(detail_table.rows.len(), 1);
        assert_eq!(detail_table.rows[0][1], "500");
        assert_eq!(detail_table.rows[0][2], "bob");
        let filter = detail_table
            .command_filter
            .expect("详情页应携带业务过滤器");
        assert_eq!(filter.controls[0].key, PERFORMANCE_FILTER_USERS_KEY);
        assert_eq!(filter.controls[0].value, "bob");
    }

    /// 覆盖非法时间过滤返回中文错误。
    #[test]
    fn 性能过滤时间格式错误会返回错误() {
        let mut data = BTreeMap::new();
        data.insert(
            PERFORMANCE_FILTER_START_KEY.to_string(),
            "2026/05/01 00:00:00".to_string(),
        );
        let response = build_performance_list_response_with_filter(
            vec![file("100&alice&_a&1778661627833&0&0.log")],
            data,
            |_| {},
        );
        let PluginCommandResponse::Error { message } = response else {
            panic!("时间格式错误应返回插件错误");
        };
        assert!(message.contains("yyyy-MM-dd HH:mm:ss"));
    }

    /// 覆盖详情页仍为可读取请求生成 SQL 延迟命令。
    ///
    /// 业务意图：
    /// - 性能详情改为按需生成后，下一层“显示SQL”仍必须保留单文件延迟读取，避免详情生成时读取正文。
    #[test]
    fn 请求详情为可读取日志生成显示_sql_延迟命令() {
        let mut data = BTreeMap::new();
        data.insert("route".to_string(), "/a".to_string());
        let response = build_performance_detail_response(
            vec![readable_file(
                "100&alice&_a&1778661627833&0&0.log",
                "/tmp/a.log",
            )],
            data,
            |_| {},
        );
        let PluginCommandResponse::OpenWindow { page, .. } = response else {
            panic!("应返回窗口");
        };
        let detail_table = page.table.as_ref().expect("详情页应包含表格");
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
