// Java 线程日志分析业务功能域。
//
// 业务意图：
// - 该模块独立维护 thread dump 快照解析、过滤规则、线程状态模型和时间线矩阵构建。
// - GPUI 窗口只消费这里产出的 `ThreadAnalysisData`，不再拥有日志解析规则或过滤匹配语义。
//
// 边界条件：
// - 读取失败、编码失败、缺失状态行、单文件/多文件默认可见规则都保持迁移前行为不变。
// - 路径来源继续使用 `LogFileSource`，确保本地文件和压缩包成员点击跳转行为稳定。

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use crate::{
    config::normalize_thread_analysis_filter_text,
    log_document::{EncodingChoice, decode_log_bytes, read_log_source_bytes},
    log_source::LogFileSource,
    theme::EffectiveTheme,
};

/// 线程日志分析结果。
///
/// 业务意图：
/// - 独立窗口只负责渲染已经解析好的时间线数据，不在绘制阶段重新扫描日志正文。
/// - 多个文件的 Java thread dump 会合并成同一条时间轴，便于比较线程在不同快照中的状态变化。
#[derive(Clone)]
pub(crate) struct ThreadAnalysisData {
    /// 分析标题。
    pub(crate) title: String,
    /// 面向用户的摘要。
    pub(crate) summary: String,
    /// 后台解析中的进度快照。
    ///
    /// 业务意图：
    /// - 线程日志分析会读取和解码多个来源，耗时期间独立窗口需要展示确定进度，避免用户误以为界面卡住。
    /// - `None` 表示已经完成或失败到最终摘要态，窗口只展示结果矩阵。
    pub(crate) progress: Option<ThreadAnalysisProgress>,
    /// 横轴快照标签。
    pub(crate) snapshots: Vec<ThreadSnapshot>,
    /// 纵轴线程名，按命中次数从高到低排序，次数相同再按首次出现顺序排序。
    pub(crate) thread_names: Vec<String>,
    /// 线程名到每个快照详情的矩阵。
    ///
    /// 业务意图：
    /// - 单个色块既要展示状态，也要支持悬浮查看线程片段、单击回到主窗口定位原始日志行。
    /// - 因此矩阵保存可定位的单元详情，而不是只保存颜色所需的状态枚举。
    pub(crate) matrix: Vec<Vec<Option<Arc<ThreadTimelineCell>>>>,
}

/// 线程日志分析后台解析进度。
///
/// 业务意图：
/// - 进度以“文件”为主单位，因为读取、解码和 thread dump 解析都按日志来源顺序执行。
/// - 同时记录已经解析出的快照和线程样本数量，让用户能看到大文件解析确实在推进，而不是只有百分比变化。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThreadAnalysisProgress {
    /// 总文件数。
    pub(crate) total_files: usize,
    /// 已完成读取、解码和解析的文件数。
    pub(crate) processed_files: usize,
    /// 已跳过的不可读或不可解码文件数。
    pub(crate) skipped_files: usize,
    /// 当前正在处理的文件名。
    pub(crate) current_file: Option<String>,
    /// 已识别的 thread dump 快照数。
    pub(crate) parsed_snapshots: usize,
    /// 已识别的线程样本数。
    pub(crate) parsed_threads: usize,
}

impl ThreadAnalysisProgress {
    /// 创建线程分析初始进度。
    ///
    /// 边界条件：
    /// - 空来源不会启动分析，但纯函数仍把空总数视为完成，避免进度条出现 NaN 或除零。
    pub(crate) fn new(total_files: usize) -> Self {
        Self {
            total_files,
            processed_files: 0,
            skipped_files: 0,
            current_file: None,
            parsed_snapshots: 0,
            parsed_threads: 0,
        }
    }

    /// 返回当前文件处理进度比例。
    pub(crate) fn ratio(&self) -> f32 {
        if self.total_files == 0 {
            1.0
        } else {
            (self.processed_files as f32 / self.total_files as f32).clamp(0.0, 1.0)
        }
    }

    /// 返回进度条右侧用户可见文案。
    pub(crate) fn label(&self) -> String {
        let percent = self.ratio() * 100.0;
        format!(
            "{} / {} 文件 · {:.0}% · {} 个快照 · {} 个线程",
            self.processed_files.min(self.total_files),
            self.total_files,
            percent,
            self.parsed_snapshots,
            self.parsed_threads
        )
    }

    /// 返回当前解析阶段说明。
    pub(crate) fn message(&self) -> String {
        match &self.current_file {
            Some(file) if self.processed_files < self.total_files => {
                format!("正在解析：{file}")
            }
            _ if self.processed_files >= self.total_files => "正在整理分析结果...".to_string(),
            _ => "正在准备线程日志分析...".to_string(),
        }
    }
}

/// 单个 thread dump 快照。
///
/// 业务意图：
/// - Java thread dump 通常由时间戳和 `Full thread dump` 标记组成；如果没有时间戳则使用快照序号兜底。
#[derive(Clone)]
pub(crate) struct ThreadSnapshot {
    /// 横轴展示标签。
    pub(crate) label: String,
    /// 当前快照所属的日志文件序号。
    ///
    /// 业务意图：
    /// - 该字段保留快照与选中来源的对应关系，便于后续摘要、调试或恢复跨文件统计时区分快照序号和来源文件序号。
    /// - 当前默认过滤规则按线程样本出现次数判断，不再直接依赖该字段。
    pub(crate) source_index: usize,
    /// 当前快照所属日志来源。
    ///
    /// 业务意图：
    /// - 单击分析色块需要在主窗口打开对应本地文件或压缩包成员，因此必须保留真实来源，不能只保留展示名。
    pub(crate) source: LogFileSource,
    /// 当前快照内识别出的线程状态。
    pub(crate) threads: Vec<ThreadStateSample>,
}

/// 单个线程在某个快照中的状态。
#[derive(Clone)]
pub(crate) struct ThreadStateSample {
    /// Java 线程名。
    pub(crate) name: String,
    /// Java thread dump 线程 ID。
    ///
    /// 业务意图：
    /// - HotSpot 线程头通常同时包含 `#123` 和 `tid=0x...`；这里优先记录更适合人工核对的 `#123`，
    ///   兼容缺失 `#` 的日志时再记录 `tid`。
    pub(crate) thread_id: Option<String>,
    /// Java 线程状态。
    pub(crate) state: ThreadStateKind,
    /// 线程头在解码后日志中的零基行号。
    ///
    /// 业务意图：
    /// - 单击色块回主窗口时需要跳转到线程头，而不是只打开文件或跳到状态行。
    pub(crate) line_index: usize,
    /// 线程头开始的前 5 行日志预览。
    ///
    /// 边界条件：
    /// - 文件末尾不足 5 行时只保留实际存在的行；预览只用于悬浮气泡，不参与状态分析。
    pub(crate) preview_lines: Vec<String>,
    /// 线程头开始直到下一个线程头或下一个快照前的完整堆栈片段。
    ///
    /// 业务意图：
    /// - 设置页允许用户粘贴完整线程堆栈过滤无效线程，过滤必须基于完整片段，不能只看悬浮气泡的前 5 行预览。
    pub(crate) stack_lines: Vec<String>,
}

/// 线程分析时间线中的可交互色块数据。
///
/// 业务意图：
/// - 渲染阶段需要快速拿到颜色、气泡内容和跳转目标；把这些信息在构建矩阵时固化，可避免点击时扫描大文件。
#[derive(Clone)]
pub(crate) struct ThreadTimelineCell {
    /// Java 线程状态。
    pub(crate) state: ThreadStateKind,
    /// 当前快照展示时间。
    pub(crate) time_label: String,
    /// 完整 Java 线程名。
    pub(crate) thread_name: String,
    /// Java thread dump 线程 ID。
    pub(crate) thread_id: Option<String>,
    /// 原始日志来源。
    pub(crate) source: LogFileSource,
    /// 线程头零基行号。
    pub(crate) line_index: usize,
    /// 线程头开始的前 5 行日志预览。
    pub(crate) preview_lines: Vec<String>,
    /// 线程头开始直到下一个线程头或下一个快照前的完整堆栈片段。
    ///
    /// 业务意图：
    /// - 点击时间线色块会打开独立堆栈详情窗口，详情窗口必须展示完整原始片段，而不是悬浮气泡的 5 行预览。
    /// - 该字段在矩阵构建阶段从解析样本复制，点击时不再回读日志文件，避免压缩包和大文件随机读取影响交互。
    pub(crate) stack_lines: Vec<String>,
}

/// 线程头已识别但状态行尚未出现时的临时解析状态。
///
/// 业务意图：
/// - Java thread dump 的线程名、ID 位于线程头，状态位于后续行；只有两者都存在时才生成有效样本。
/// - 临时结构避免在解析循环中用多个并行 `Option` 字段，降低状态错配风险。
pub(crate) struct ThreadStateSamplePending {
    /// Java 线程名。
    pub(crate) name: String,
    /// Java thread dump 线程 ID。
    pub(crate) thread_id: Option<String>,
    /// 线程头零基行号。
    pub(crate) line_index: usize,
    /// 线程头开始的完整片段。
    ///
    /// 业务意图：
    /// - 解析过程中先完整收集线程片段，最终生成样本时再截取前 5 行作为气泡预览，避免预览和过滤片段来源不一致。
    pub(crate) stack_lines: Vec<String>,
    /// 已解析到的 Java 线程状态。
    ///
    /// 边界条件：
    /// - 部分异常 thread dump 可能只有线程头没有状态行；这类线程不会生成样本，避免污染状态时间线。
    pub(crate) state: Option<ThreadStateKind>,
}

/// 线程日志分析过滤规则类型。
///
/// 业务意图：
/// - 线程分析过滤同时支持“线程名通配”和“完整堆栈片段”两种方式；显式类型可以避免匹配阶段反复猜测规则含义。
/// - 设置页仍使用一个多行文本框保存配置，解析后再拆成稳定类型，保持旧配置文件兼容。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThreadAnalysisFilterRuleKind {
    /// 按线程名匹配，支持 `*` 作为任意长度通配符。
    ThreadNamePattern,
    /// 按线程完整堆栈中的连续非空行匹配。
    StackLines,
}

/// 线程日志分析过滤规则。
///
/// 业务意图：
/// - 设置页中一段粘贴的堆栈会转换为堆栈规则；规则中的非空行必须连续命中同一个线程完整堆栈才过滤。
/// - 单行线程名或线程名通配符会转换为线程名规则，用于过滤 `C2 CompilerThread*` 这类 JVM 常驻线程噪声。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThreadAnalysisFilterRule {
    /// 当前规则匹配方式。
    pub(crate) kind: ThreadAnalysisFilterRuleKind,
    /// 已去除首尾空白的非空规则行。
    ///
    /// 边界条件：
    /// - 线程名规则只使用第一行作为模式；堆栈规则保留多行连续片段。
    pub(crate) lines: Vec<String>,
}

/// Java thread dump 中常见线程状态。
///
/// 业务意图：
/// - 状态枚举驱动时间线色块，未知状态仍保留为 `Other`，避免新 JVM 文案导致整份分析失败。
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum ThreadStateKind {
    /// RUNNABLE。
    Runnable,
    /// BLOCKED。
    Blocked,
    /// WAITING。
    Waiting,
    /// TIMED_WAITING。
    TimedWaiting,
    /// NEW。
    New,
    /// TERMINATED。
    Terminated,
    /// 未识别或其它 JVM 状态。
    Other,
}

impl ThreadStateKind {
    /// 从 thread dump 状态文本解析状态枚举。
    pub(crate) fn parse(text: &str) -> Self {
        if text.contains("TIMED_WAITING") {
            Self::TimedWaiting
        } else if text.contains("RUNNABLE") {
            Self::Runnable
        } else if text.contains("BLOCKED") {
            Self::Blocked
        } else if text.contains("WAITING") {
            Self::Waiting
        } else if text.contains("NEW") {
            Self::New
        } else if text.contains("TERMINATED") {
            Self::Terminated
        } else {
            Self::Other
        }
    }

    /// 返回 UI 展示文案。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Runnable => "RUNNABLE",
            Self::Blocked => "BLOCKED",
            Self::Waiting => "WAITING",
            Self::TimedWaiting => "TIMED_WAITING",
            Self::New => "NEW",
            Self::Terminated => "TERMINATED",
            Self::Other => "OTHER",
        }
    }

    /// 返回当前主题下的状态色。
    ///
    /// 业务意图：
    /// - 时间线色块需要在明暗主题下都有足够对比度，同时让阻塞、等待和运行态能快速区分。
    pub(crate) fn color(self, theme: EffectiveTheme) -> u32 {
        match (theme, self) {
            (_, Self::Runnable) => 0x22c55e,
            (_, Self::Blocked) => 0xef4444,
            (_, Self::Waiting) => 0xf59e0b,
            (_, Self::TimedWaiting) => 0x38bdf8,
            (_, Self::New) => 0xa78bfa,
            (_, Self::Terminated) => 0x94a3b8,
            (EffectiveTheme::Light, Self::Other) => 0x64748b,
            (EffectiveTheme::Dark, Self::Other) => 0x94a3b8,
        }
    }
}

/// 读取并分析多个日志来源中的 Java thread dump，并在每个文件边界上汇报进度。
///
/// 业务意图：
/// - UI 层可以把进度快照写入独立窗口，避免大批量日志解析期间只显示静态“正在分析”文案。
/// - 进度回调只在文件开始和文件结束时触发，不在每一行解析时触发，避免超大日志解析时频繁跨线程同步影响吞吐。
pub(crate) fn analyze_thread_dump_sources_with_progress<F>(
    sources: &[LogFileSource],
    filter_rules: &[ThreadAnalysisFilterRule],
    mut report_progress: F,
) -> ThreadAnalysisData
where
    F: FnMut(ThreadAnalysisProgress),
{
    let mut snapshots = Vec::new();
    let mut skipped_files = 0usize;
    let mut progress = ThreadAnalysisProgress::new(sources.len());
    report_progress(progress.clone());
    for (source_index, source) in sources.iter().enumerate() {
        let source_name = source.display_name();
        progress.current_file = Some(source_name.clone());
        report_progress(progress.clone());
        let result = read_log_source_bytes(source)
            .and_then(|bytes| decode_log_bytes(&bytes, EncodingChoice::Auto, &source_name));
        match result {
            Ok(document) => {
                let parsed_snapshots = parse_thread_dump_snapshots(
                    &document.lines,
                    &source_name,
                    source_index,
                    source,
                );
                progress.parsed_threads += parsed_snapshots
                    .iter()
                    .map(|snapshot| snapshot.threads.len())
                    .sum::<usize>();
                progress.parsed_snapshots += parsed_snapshots.len();
                snapshots.extend(parsed_snapshots);
            }
            Err(_) => {
                skipped_files += 1;
                progress.skipped_files = skipped_files;
            }
        }
        progress.processed_files = source_index + 1;
        report_progress(progress.clone());
    }
    progress.current_file = None;
    report_progress(progress);

    build_thread_analysis_data(sources.len(), skipped_files, snapshots, filter_rules)
}

/// 从解码后的日志行中解析 Java thread dump 快照。
///
/// 业务意图：
/// - Java thread dump 以 `Full thread dump` 作为快照边界，线程头通常以双引号线程名开头，
///   状态行包含 `java.lang.Thread.State:`；解析这两个稳定特征即可形成线程状态时间线。
///
/// 边界条件：
/// - 线程日志常见格式会先输出打印时间，再输出 `Full thread dump`；这里优先把打印时间作为横轴标签。
/// - 如果缺失状态行，当前线程不会加入快照，避免用未知状态污染时间线。
pub(crate) fn parse_thread_dump_snapshots(
    lines: &[String],
    source_name: &str,
    source_index: usize,
    source: &LogFileSource,
) -> Vec<ThreadSnapshot> {
    let mut snapshots = Vec::new();
    let mut current_snapshot: Option<ThreadSnapshot> = None;
    let mut pending_thread: Option<ThreadStateSamplePending> = None;
    let mut last_timestamp: Option<String> = None;

    for (line_index, line) in lines.iter().enumerate() {
        if line.contains("Full thread dump") {
            if let (Some(snapshot), Some(pending)) =
                (current_snapshot.as_mut(), pending_thread.take())
            {
                finish_pending_thread_sample(snapshot, pending);
            }
            if let Some(snapshot) = current_snapshot.take()
                && !snapshot.threads.is_empty()
            {
                snapshots.push(snapshot);
            }
            let label = last_timestamp
                .clone()
                .unwrap_or_else(|| format!("{} #{}", source_name, snapshots.len() + 1));
            current_snapshot = Some(ThreadSnapshot {
                label,
                source_index,
                source: source.clone(),
                threads: Vec::new(),
            });
            pending_thread = None;
            continue;
        }
        if let Some(timestamp) = extract_thread_dump_timestamp(line) {
            last_timestamp = Some(timestamp);
        }

        let Some(snapshot) = current_snapshot.as_mut() else {
            continue;
        };
        if let Some((thread_name, thread_id)) = parse_thread_header_details(line) {
            if let Some(pending) = pending_thread.take() {
                finish_pending_thread_sample(snapshot, pending);
            }
            pending_thread = Some(ThreadStateSamplePending {
                name: thread_name,
                thread_id,
                line_index,
                stack_lines: vec![line.clone()],
                state: None,
            });
            continue;
        }
        if let Some(pending) = pending_thread.as_mut() {
            pending.stack_lines.push(line.clone());
            if let Some(state_text) = line.split("java.lang.Thread.State:").nth(1) {
                pending.state = Some(ThreadStateKind::parse(state_text));
            }
        }
    }

    if let (Some(snapshot), Some(pending)) = (current_snapshot.as_mut(), pending_thread.take()) {
        finish_pending_thread_sample(snapshot, pending);
    }
    if let Some(snapshot) = current_snapshot
        && !snapshot.threads.is_empty()
    {
        snapshots.push(snapshot);
    }

    snapshots
}

/// 将已收集完的线程片段写入当前快照。
///
/// 业务意图：
/// - 线程头、状态行和后续堆栈帧分散在多行；只有遇到下一个线程或快照边界时才知道完整片段。
/// - 没有状态行的片段不生成样本，避免未知状态污染时间线；有状态行的片段保留完整堆栈供设置过滤匹配。
pub(crate) fn finish_pending_thread_sample(
    snapshot: &mut ThreadSnapshot,
    pending: ThreadStateSamplePending,
) {
    let Some(state) = pending.state else {
        return;
    };
    let preview_lines = thread_stack_preview_lines(&pending.stack_lines);
    snapshot.threads.push(ThreadStateSample {
        name: pending.name,
        thread_id: pending.thread_id,
        state,
        line_index: pending.line_index,
        preview_lines,
        stack_lines: pending.stack_lines,
    });
}

/// 提取单个线程片段的前 5 行预览。
///
/// 业务意图：
/// - 悬浮气泡用于查看当前色块对应线程的原始上下文，不能把下一个线程头或下一个 dump 快照混入预览。
/// - 预览限制为 5 行，避免超长堆栈在气泡中占满窗口。
pub(crate) fn thread_stack_preview_lines(stack_lines: &[String]) -> Vec<String> {
    stack_lines.iter().take(5).cloned().collect()
}

/// 提取 thread dump 附近的时间戳文案。
///
/// 业务意图：
/// - 用户要求横轴为时间线；常见日志会在 dump 前输出 `YYYY-MM-DD HH:MM:SS` 或 JVM 日期行。
/// - 不引入时间解析依赖，只提取稳定前缀作为显示标签，避免因时区或本地化月份解析失败丢失标签。
pub(crate) fn extract_thread_dump_timestamp(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some(timestamp) = extract_thread_dump_timestamp_after_marker(trimmed, "打印时间") {
        return Some(timestamp);
    }
    if let Some(timestamp) = extract_thread_dump_timestamp_after_marker(trimmed, "print time") {
        return Some(timestamp);
    }
    if let Some(timestamp) = extract_thread_dump_timestamp_after_marker(trimmed, "dump time") {
        return Some(timestamp);
    }
    if let Some(timestamp) = extract_leading_datetime_label(trimmed) {
        return Some(timestamp);
    }
    if trimmed.len() >= 24
        && trimmed
            .chars()
            .take(3)
            .all(|character| character.is_ascii_alphabetic())
        && trimmed.as_bytes().get(3) == Some(&b' ')
        && trimmed.as_bytes().get(7) == Some(&b' ')
        && trimmed.contains(':')
    {
        return Some(trimmed.chars().take(24).collect());
    }
    None
}

/// 从包含“打印时间”标记的日志行中提取时间部分。
///
/// 业务意图：
/// - 线程日志可能用 `线程日志打印时间：2026-...` 这类前缀描述 dump 生成时间，
///   横轴应展示真正的打印时间，而不是整行说明文字。
pub(crate) fn extract_thread_dump_timestamp_after_marker(
    line: &str,
    marker: &str,
) -> Option<String> {
    let lower_line = line.to_ascii_lowercase();
    let marker_index = lower_line.find(&marker.to_ascii_lowercase())?;
    let after_marker = &line[marker_index + marker.len()..];
    let trimmed = after_marker
        .trim_start_matches(|character: char| {
            character.is_whitespace()
                || matches!(character, ':' | '：' | '=' | '-' | '>' | '】' | ']')
        })
        .trim();
    extract_leading_datetime_label(trimmed)
}

/// 提取行首常见时间标签。
///
/// 边界条件：
/// - 当前不做严格日期合法性校验，只识别日志中稳定的 `YYYY-MM-DD HH:MM:SS` 展示形态。
pub(crate) fn extract_leading_datetime_label(text: &str) -> Option<String> {
    if text.len() >= 19
        && text.as_bytes().get(4) == Some(&b'-')
        && text.as_bytes().get(7) == Some(&b'-')
        && text.as_bytes().get(10) == Some(&b' ')
        && text.as_bytes().get(13) == Some(&b':')
        && text.as_bytes().get(16) == Some(&b':')
    {
        Some(text[..19].to_string())
    } else {
        None
    }
}

/// 从 Java thread dump 线程头中解析线程名和线程 ID。
///
/// 边界条件：
/// - 标准 HotSpot 线程头以 `"线程名"` 开头；不符合该形态的行直接忽略。
/// - 线程 ID 优先使用 `#123` 形式，缺失时回退到 `tid=0x...`，保证不同 JVM 输出都能提供可核对标识。
pub(crate) fn parse_thread_header_details(line: &str) -> Option<(String, Option<String>)> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix('"')?;
    let end = rest.find('"')?;
    let thread_name = rest[..end].to_string();
    let metadata = rest[end + 1..].trim();
    let thread_id = metadata
        .split_whitespace()
        .find_map(|part| part.strip_prefix('#').map(|id| format!("#{id}")))
        .or_else(|| {
            metadata
                .split_whitespace()
                .find_map(|part| part.strip_prefix("tid=").map(|id| format!("tid={id}")))
        });
    Some((thread_name, thread_id))
}

/// 解析线程日志分析过滤配置文本。
///
/// 业务意图：
/// - 设置页允许用户用空行分隔多段堆栈；每段堆栈去除行首尾空白后形成一条连续片段匹配规则。
/// - 线程名过滤已经拆到独立输入框，堆栈过滤入口必须始终生成 `StackLines`，避免单行线程头或方法片段被误当作线程名通配。
/// - 空段和空行不形成规则，避免用户粘贴时多余空白导致所有线程都不匹配或产生无意义规则。
pub(crate) fn parse_thread_analysis_filter_rules(raw: &str) -> Vec<ThreadAnalysisFilterRule> {
    let normalized = normalize_thread_analysis_filter_text(raw);
    let mut rules = Vec::new();
    let mut current_lines = Vec::new();
    for line in normalized.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !current_lines.is_empty() {
                rules.push(ThreadAnalysisFilterRule {
                    kind: ThreadAnalysisFilterRuleKind::StackLines,
                    lines: std::mem::take(&mut current_lines),
                });
            }
        } else {
            current_lines.push(trimmed.to_string());
        }
    }
    if !current_lines.is_empty() {
        rules.push(ThreadAnalysisFilterRule {
            kind: ThreadAnalysisFilterRuleKind::StackLines,
            lines: current_lines,
        });
    }
    rules
}

/// 解析线程日志分析线程名过滤配置文本。
///
/// 业务意图：
/// - 设置页现在把线程名过滤拆成独立输入框；该输入框只表达线程名通配，不再和堆栈片段混排。
/// - 为了方便从说明或旧配置中复制，除逐行规则外也接受英文逗号分隔，例如 `C1*,C2*` 会形成两条规则。
///
/// 边界条件：
/// - 空行、连续逗号和纯空白片段都会被忽略，避免误生成空模式；单独 `*` 仍作为用户显式配置保留。
pub(crate) fn parse_thread_analysis_name_filter_rules(raw: &str) -> Vec<ThreadAnalysisFilterRule> {
    normalize_thread_analysis_filter_text(raw)
        .lines()
        .flat_map(|line| line.split(','))
        .filter_map(|part| {
            let pattern = part.trim();
            (!pattern.is_empty()).then(|| ThreadAnalysisFilterRule {
                kind: ThreadAnalysisFilterRuleKind::ThreadNamePattern,
                lines: vec![pattern.to_string()],
            })
        })
        .collect()
}

/// 判断某个线程完整堆栈是否命中过滤规则。
///
/// 业务意图：
/// - “无效线程”通常由一段稳定堆栈片段识别；要求规则行连续出现可以降低只凭单行类名误过滤其它线程的风险。
pub(crate) fn thread_stack_matches_filter_rule(
    stack_lines: &[String],
    rule: &ThreadAnalysisFilterRule,
) -> bool {
    if rule.kind != ThreadAnalysisFilterRuleKind::StackLines {
        return false;
    }
    if rule.lines.is_empty() || stack_lines.len() < rule.lines.len() {
        return false;
    }
    stack_lines.windows(rule.lines.len()).any(|window| {
        window
            .iter()
            .map(|line| line.trim())
            .eq(rule.lines.iter().map(String::as_str))
    })
}

/// 判断某个线程名是否命中过滤规则。
///
/// 业务意图：
/// - 线程名过滤用于提前移除 JVM 常驻线程，例如 `C1 CompilerThread*`、`Service Thread` 和 `Attach Listener`。
/// - 匹配保持大小写敏感，避免把业务线程中大小写不同的名称意外过滤掉；`*` 只表示任意长度文本。
pub(crate) fn thread_name_matches_filter_rule(
    thread_name: &str,
    rule: &ThreadAnalysisFilterRule,
) -> bool {
    if rule.kind != ThreadAnalysisFilterRuleKind::ThreadNamePattern {
        return false;
    }
    let Some(pattern) = rule.lines.first() else {
        return false;
    };
    wildcard_pattern_matches_text(pattern, thread_name)
}

/// 使用简单 `*` 通配符匹配文本。
///
/// 业务意图：
/// - 线程名过滤只需要前缀、后缀或中间片段匹配，不引入正则依赖，避免设置页把普通 `.`、`[` 等线程名字符解释成正则语法。
///
/// 边界条件：
/// - 空模式不匹配任何线程；单独 `*` 匹配所有线程，保留给高级用户临时清空分析结果使用。
/// - 多个 `*` 会被当成多个任意长度间隔处理，算法只按片段顺序扫描，不会产生指数级回溯。
pub(crate) fn wildcard_pattern_matches_text(pattern: &str, text: &str) -> bool {
    if pattern.is_empty() {
        return false;
    }
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return pattern == text;
    }

    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');
    let parts = pattern
        .split('*')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return true;
    }

    let mut search_start = 0usize;
    for (part_index, part) in parts.iter().enumerate() {
        let Some(found_offset) = text[search_start..].find(part) else {
            return false;
        };
        let found_index = search_start + found_offset;
        if part_index == 0 && anchored_start && found_index != 0 {
            return false;
        }
        search_start = found_index + part.len();
    }

    if anchored_end {
        let Some(last_part) = parts.last() else {
            return true;
        };
        text.ends_with(last_part)
    } else {
        true
    }
}

/// 判断某个线程是否应被线程日志分析过滤规则移除。
fn thread_sample_matches_filter_rules(
    sample: &ThreadStateSample,
    filter_rules: &[ThreadAnalysisFilterRule],
) -> bool {
    filter_rules.iter().any(|rule| {
        thread_name_matches_filter_rule(&sample.name, rule)
            || thread_stack_matches_filter_rule(&sample.stack_lines, rule)
    })
}

/// 构建线程分析窗口可直接渲染的数据矩阵。
///
/// 业务意图：
/// - 解析阶段按快照保存线程列表；渲染阶段需要按线程名聚合成二维矩阵，横轴为快照，纵轴为线程。
/// - 默认只保留在选中日志快照中出现超过一次的线程，过滤单次线程，降低纵轴噪声。
/// - 纵轴按命中次数从高到低排序，优先把持续出现的线程放到顶部，便于用户先看高频问题线程。
pub(crate) fn build_thread_analysis_data(
    source_count: usize,
    skipped_files: usize,
    mut snapshots: Vec<ThreadSnapshot>,
    filter_rules: &[ThreadAnalysisFilterRule],
) -> ThreadAnalysisData {
    // `source_index` 仍是快照来源定位元数据；虽然当前“只出现一次”过滤不再按文件数判断，
    // 构建阶段仍读取一次以保持字段参与主流程，避免后续恢复跨文件统计时误删该边界信息。
    let _has_snapshot_source_index = snapshots
        .iter()
        .any(|snapshot| snapshot.source_index < source_count);
    let mut filtered_threads = 0usize;
    if !filter_rules.is_empty() {
        for snapshot in &mut snapshots {
            let before = snapshot.threads.len();
            snapshot
                .threads
                .retain(|sample| !thread_sample_matches_filter_rules(sample, filter_rules));
            filtered_threads += before.saturating_sub(snapshot.threads.len());
        }
    }
    let _has_snapshot_labels = snapshots.iter().any(|snapshot| !snapshot.label.is_empty());
    let visible_thread_name_set = default_visible_thread_names(&snapshots, source_count);
    let all_thread_name_set = snapshots
        .iter()
        .flat_map(|snapshot| snapshot.threads.iter().map(|sample| sample.name.clone()))
        .collect::<BTreeSet<_>>();
    let auto_filtered_threads = all_thread_name_set
        .iter()
        .filter(|thread_name| !visible_thread_name_set.contains(*thread_name))
        .count();
    let total_filtered_threads = filtered_threads.saturating_add(auto_filtered_threads);
    let mut thread_stats = HashMap::<String, (usize, usize)>::new();
    let mut next_first_seen_order = 0usize;
    for snapshot in &snapshots {
        for sample in &snapshot.threads {
            if !visible_thread_name_set.contains(&sample.name) {
                continue;
            }
            let entry = thread_stats.entry(sample.name.clone()).or_insert_with(|| {
                let first_seen_order = next_first_seen_order;
                next_first_seen_order = next_first_seen_order.saturating_add(1);
                (0, first_seen_order)
            });
            entry.0 = entry.0.saturating_add(1);
        }
    }
    let mut thread_names = thread_stats.into_iter().collect::<Vec<_>>();
    thread_names.sort_by(
        |(left_name, (left_hits, left_order)), (right_name, (right_hits, right_order))| {
            right_hits
                .cmp(left_hits)
                .then_with(|| left_order.cmp(right_order))
                .then_with(|| left_name.cmp(right_name))
        },
    );
    let thread_names = thread_names
        .into_iter()
        .map(|(thread_name, _)| thread_name)
        .collect::<Vec<_>>();

    let thread_index_by_name = thread_names
        .iter()
        .enumerate()
        .map(|(index, name)| (name.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut matrix = vec![vec![None; snapshots.len()]; thread_names.len()];
    for (snapshot_index, snapshot) in snapshots.iter().enumerate() {
        for sample in &snapshot.threads {
            if let Some(thread_index) = thread_index_by_name.get(&sample.name) {
                matrix[*thread_index][snapshot_index] = Some(Arc::new(ThreadTimelineCell {
                    state: sample.state,
                    time_label: snapshot.label.clone(),
                    thread_name: sample.name.clone(),
                    thread_id: sample.thread_id.clone(),
                    source: snapshot.source.clone(),
                    line_index: sample.line_index,
                    preview_lines: sample.preview_lines.clone(),
                    stack_lines: sample.stack_lines.clone(),
                }));
            }
        }
    }

    let summary = if snapshots.is_empty() {
        format!(
            "未识别到 Java thread dump 快照，已跳过 {} 个无法读取或解码的文件",
            skipped_files
        )
    } else {
        format!(
            "{} 个文件，{} 个快照，{} 个线程，跳过 {} 个文件，过滤 {} 个线程",
            source_count,
            snapshots.len(),
            thread_names.len(),
            skipped_files,
            total_filtered_threads
        )
    };
    ThreadAnalysisData {
        title: "线程日志分析".to_string(),
        summary,
        progress: None,
        snapshots,
        thread_names,
        matrix,
    }
}

/// 返回线程分析默认可见线程名集合。
///
/// 业务意图：
/// - 用户要求选中日志中只出现一次的线程默认过滤，避免短暂线程或单次噪声撑大线程分析纵轴。
/// - 判断单位是线程样本出现次数，不再要求跨不同文件；同一文件多个快照中重复出现也视为值得展示。
/// - `_source_count` 保留在签名中，兼容旧调用点和测试语义；当前规则只依赖解析后的快照样本。
pub(crate) fn default_visible_thread_names(
    snapshots: &[ThreadSnapshot],
    _source_count: usize,
) -> HashSet<String> {
    let mut sample_count_by_thread = HashMap::<String, usize>::new();
    for snapshot in snapshots {
        for sample in &snapshot.threads {
            *sample_count_by_thread
                .entry(sample.name.clone())
                .or_insert(0) += 1;
        }
    }

    sample_count_by_thread
        .into_iter()
        .filter_map(|(thread_name, sample_count)| (sample_count > 1).then_some(thread_name))
        .collect()
}

/// 根据状态集合计算可见线程行下标。
///
/// 业务意图：
/// - 状态过滤发生在线程行维度；只有某个线程在当前勾选状态下出现超过一次，才保留在纵轴中。
/// - 这条规则和“选中日志中只出现一次的线程默认过滤”保持一致，但统计口径改成当前窗口实际可见的数据。
/// - 线程分析窗口的图例允许用户只看 RUNNABLE、WAITING 等部分状态，因此最终展示顺序必须按“当前可见状态”
///   的命中次数重新排序；否则默认只显示 RUNNABLE 时，可能把只有一个绿色色块的线程排在高频 RUNNABLE 线程前面。
///
/// 边界条件：
/// - 空矩阵或没有勾选任何状态时返回空列表，避免 UI 渲染无意义的空行。
/// - 当前状态下只有一个命中的线程会被隐藏；如果用户切换图例后累计可见命中超过一次，该线程会重新显示。
/// - 命中次数相同时保留 `analysis.thread_names` 的原始顺序，原始顺序已经按全量命中数和首次出现顺序稳定排序。
pub(crate) fn visible_thread_indexes_for_state_kinds(
    analysis: &ThreadAnalysisData,
    visible_state_kinds: &HashSet<ThreadStateKind>,
) -> Vec<usize> {
    let mut visible_rows = analysis
        .matrix
        .iter()
        .enumerate()
        .filter_map(|(row_index, cells)| {
            // 统计当前状态筛选条件下真正会显示为色块的命中数量；隐藏状态不参与排序，
            // 这样用户切换图例后看到的行顺序始终对应当前画面里的命中密度。
            let visible_hit_count = cells
                .iter()
                .flatten()
                .filter(|cell| visible_state_kinds.contains(&cell.state))
                .count();
            (visible_hit_count > 1).then_some((row_index, visible_hit_count))
        })
        .collect::<Vec<_>>();

    visible_rows.sort_by(|(left_index, left_hits), (right_index, right_hits)| {
        right_hits
            .cmp(left_hits)
            .then_with(|| left_index.cmp(right_index))
    });

    visible_rows
        .into_iter()
        .map(|(row_index, _visible_hit_count)| row_index)
        .collect()
}
