//! 泛微日志插件 sidecar。
//!
//! 业务意图：
//! - 该二进制作为独立插件工程编译和打包，由宿主通过 stdin/stdout 传递 JSON 请求和响应。
//! - “性能列表解析”先使用宿主传入的文件元数据解析文件名；用户点击“显示SQL”后才读取对应单个日志正文。
//!
//! 边界条件：
//! - 只接收严格六段文件名：`耗时&用户&地址&时间戳&字段5&字段6.log`。
//! - 耗时必须为毫秒数字，时间戳必须为 13 位毫秒数字；用户名允许为空，其余格式错误的文件直接跳过。
//! - SQL 明细只读取宿主传入的 `read_path`，不会扫描任意目录；正文按 UTF-8 有损转换逐行解析，避免异常编码中断插件。

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
        (command_id, _) => Err(format!("未知插件命令：{command_id}")),
    }
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
