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
    hash::{Hash, Hasher},
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
    /// 本次分析输入的日志来源数量。
    ///
    /// 业务意图：
    /// - 线程分析窗口允许临时启停过滤规则，重算结果摘要时必须保留原始文件数量，不能从过滤后的快照反推。
    /// - 该字段只描述本次分析任务，不参与文件系统重新读取，避免窗口内交互触发 IO。
    pub(crate) source_count: usize,
    /// 本次分析中因读取或解码失败而跳过的文件数。
    ///
    /// 边界条件：
    /// - 临时取消过滤规则只重新计算内存中的线程样本，跳过文件数量必须保持后台解析阶段的结果。
    pub(crate) skipped_files: usize,
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
    /// 未应用用户过滤规则的原始线程快照。
    ///
    /// 业务意图：
    /// - 过滤规则弹层中的复选框只对当前窗口临时生效；用户取消某条规则后，被该规则隐藏的线程样本需要能立即恢复。
    /// - 因此分析完成后保留解析得到的原始快照，窗口重算时不重新读取本地文件或压缩包成员。
    ///
    /// 边界条件：
    /// - 进度态和未识别到快照时该集合为空；UI 不应把空集合视为错误。
    pub(crate) raw_snapshots: Vec<ThreadSnapshot>,
    /// 当前窗口中可临时启停的线程分析过滤规则。
    ///
    /// 业务意图：
    /// - 规则来源仍是设置页持久化配置，窗口只保存本次分析的启用状态；切换复选框不回写配置文件。
    pub(crate) filter_rule_states: Vec<ThreadAnalysisFilterRuleState>,
    /// 是否在频率分析中隐藏只出现一次的线程。
    ///
    /// 业务意图：
    /// - 这是线程分析内置的降噪规则，也属于当前结果的有效过滤条件；窗口过滤弹层允许用户临时关闭它来排查短生命周期线程。
    /// - 堆栈并发分析的统计口径仍基于用户规则后的全部样本，不受该频率页降噪规则影响。
    pub(crate) hide_single_occurrence_threads: bool,
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
    /// 并发分析页按规范化堆栈聚合后的统计行。
    ///
    /// 业务意图：
    /// - 频率时间线会默认隐藏只出现一次的线程，而堆栈并发分析需要回答“同一类堆栈在选中日志中同时出现多少”。
    /// - 因此该集合基于用户过滤规则后的全部线程样本构建，不受默认可见线程集合和状态图例影响。
    pub(crate) concurrency_rows: Vec<ThreadConcurrencyRow>,
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

/// 堆栈并发分析中的单个堆栈聚合行。
///
/// 业务意图：
/// - 真实 Java 线程名经常带序号或请求标识，按线程名合并会把同一问题拆散；这里改用规范化堆栈指纹聚合。
/// - 指纹忽略线程头、线程状态、栈帧源码行号和对象地址，保留方法调用序列作为“同类堆栈”的判断依据。
/// - 状态分布保留为独立计数，避免用户只能看到总数而无法区分运行、阻塞和等待样本。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThreadConcurrencyRow {
    /// 规范化堆栈指纹键。
    ///
    /// 业务意图：
    /// - 点击并发行时需要用同一指纹重新从过滤后快照收集完整原始堆栈样本；展示列只显示摘要，不足以精确匹配。
    pub(crate) stack_key: String,
    /// 用户可见的代表性栈帧。
    ///
    /// 业务意图：
    /// - 表格首列不展示完整堆栈，只展示第一个有效方法帧或兜底行，让用户快速判断这一组线程在做什么。
    pub(crate) stack_title: String,
    /// 该堆栈指纹在过滤后全部快照中的样本出现次数。
    pub(crate) total_count: usize,
    /// 单个 thread dump 快照内同一堆栈指纹同时出现的最大数量。
    ///
    /// 业务意图：
    /// - “并发”更关注同一时刻有多少线程停在同一调用栈；总出现次数可能只是多次采样累计，不能单独代表并发规模。
    pub(crate) max_snapshot_concurrency: usize,
    /// RUNNABLE 状态数量。
    pub(crate) runnable_count: usize,
    /// BLOCKED 状态数量。
    pub(crate) blocked_count: usize,
    /// WAITING 状态数量。
    pub(crate) waiting_count: usize,
    /// TIMED_WAITING 状态数量。
    pub(crate) timed_waiting_count: usize,
    /// 其它状态数量。
    ///
    /// 业务意图：
    /// - 并发页只保留五个高价值状态列；NEW、TERMINATED 和无法识别状态统一归入 OTHER，避免表格过宽。
    pub(crate) other_count: usize,
}

impl ThreadConcurrencyRow {
    /// 返回聚合行在 UI 虚拟列表中的稳定标识。
    ///
    /// 业务意图：
    /// - 堆栈指纹可能包含换行和很长的类名，不能直接作为元素 ID 展示；用哈希压缩后只服务当前 UI 标识。
    pub(crate) fn stable_id(&self) -> String {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.stack_key.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// 返回指定状态在并发聚合行中的数量。
    ///
    /// 业务意图：
    /// - UI 表格按状态列渲染，测试也需要直接验证状态分布；集中映射可以避免 NEW/TERMINATED 归类逻辑散落。
    pub(crate) fn count_for_state(&self, state: ThreadStateKind) -> usize {
        match state {
            ThreadStateKind::Runnable => self.runnable_count,
            ThreadStateKind::Blocked => self.blocked_count,
            ThreadStateKind::Waiting => self.waiting_count,
            ThreadStateKind::TimedWaiting => self.timed_waiting_count,
            ThreadStateKind::New | ThreadStateKind::Terminated | ThreadStateKind::Other => {
                self.other_count
            }
        }
    }

    /// 累加一个线程样本状态。
    ///
    /// 边界条件：
    /// - JVM 不同版本可能输出 NEW、TERMINATED 或未知状态；并发页不新增额外列，统一计入 OTHER。
    fn push_state(&mut self, state: ThreadStateKind) {
        self.total_count = self.total_count.saturating_add(1);
        match state {
            ThreadStateKind::Runnable => {
                self.runnable_count = self.runnable_count.saturating_add(1);
            }
            ThreadStateKind::Blocked => {
                self.blocked_count = self.blocked_count.saturating_add(1);
            }
            ThreadStateKind::Waiting => {
                self.waiting_count = self.waiting_count.saturating_add(1);
            }
            ThreadStateKind::TimedWaiting => {
                self.timed_waiting_count = self.timed_waiting_count.saturating_add(1);
            }
            ThreadStateKind::New | ThreadStateKind::Terminated | ThreadStateKind::Other => {
                self.other_count = self.other_count.saturating_add(1);
            }
        }
    }
}

/// 堆栈并发分析构建阶段的内部累加器。
///
/// 业务意图：
/// - 展示行不需要暴露首次出现顺序和每个快照内计数，但排序和最大并发都依赖这些元数据。
/// - 内部结构把构建细节隔离，避免 UI 或序列化层误依赖临时状态。
struct ThreadConcurrencyAccumulator {
    /// 展示给用户的聚合行。
    row: ThreadConcurrencyRow,
    /// 当前堆栈指纹第一次出现在过滤后快照中的顺序。
    first_seen_order: usize,
    /// 每个快照内该堆栈指纹已经累计的样本数量。
    ///
    /// 边界条件：
    /// - 同一快照内多个线程可能拥有完全相同堆栈；这里用于计算“最大单快照并发数”。
    snapshot_counts: HashMap<usize, usize>,
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

/// 线程分析窗口中的单条过滤规则启用状态。
///
/// 业务意图：
/// - 设置页维护长期规则文本，分析窗口只需要在当前结果里临时启停某些规则，用于核对过滤是否过宽。
/// - 将规则内容和启用状态放在一起，可以在 UI 中直接展示“当前生效/已暂停”并在切换时重算结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThreadAnalysisFilterRuleState {
    /// 解析后的过滤规则。
    pub(crate) rule: ThreadAnalysisFilterRule,
    /// 当前窗口内是否启用该规则。
    pub(crate) enabled: bool,
}

impl ThreadAnalysisFilterRuleState {
    /// 基于解析规则创建默认启用的窗口状态。
    pub(crate) fn enabled(rule: ThreadAnalysisFilterRule) -> Self {
        Self {
            rule,
            enabled: true,
        }
    }

    /// 返回适合过滤弹层展示的规则摘要。
    ///
    /// 边界条件：
    /// - 用户可能粘贴很长的完整堆栈，弹层中只展示首行和额外行数，避免宽窗口也被超长规则撑开。
    /// - 空规则理论上不会由解析器产生；这里仍提供兜底文案，防止未来协议扩展造成空白行。
    pub(crate) fn display_label(&self) -> String {
        let kind_label = match self.rule.kind {
            ThreadAnalysisFilterRuleKind::ThreadNamePattern => "线程名",
            ThreadAnalysisFilterRuleKind::StackLines => "堆栈",
        };
        let first_line = self
            .rule
            .lines
            .first()
            .map(|line| line.as_str())
            .unwrap_or("空规则");
        if self.rule.lines.len() <= 1 {
            format!("{kind_label}：{first_line}")
        } else {
            format!(
                "{kind_label}：{first_line}（另 {} 行）",
                self.rule.lines.len().saturating_sub(1)
            )
        }
    }
}

/// 当前日志正文按线程分析规则过滤后的结果。
///
/// 业务意图：
/// - 主日志查看器里的“过滤线程”按钮需要复用线程日志分析窗口的过滤语义，但输出目标不是矩阵，而是当前正文行集合。
/// - 该结构把过滤后的正文和统计信息一起返回，UI 可以只替换当前 tab 的展示内容，不改写原始文件或压缩包成员。
///
/// 边界条件：
/// - 行过滤只隐藏被判定为无效的线程片段，保留 thread dump 的时间戳、`Full thread dump` 边界和其它非线程行。
/// - 统计按线程片段计数，因为同一线程名可能在多个快照中出现，其中某些片段命中堆栈过滤而其它片段仍需保留。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ThreadDumpLineFilterResult {
    /// 过滤后的日志正文行。
    pub(crate) filtered_lines: Vec<String>,
    /// 当前日志中识别到的 thread dump 快照数量。
    pub(crate) snapshot_count: usize,
    /// 被隐藏的线程片段数量。
    pub(crate) hidden_thread_count: usize,
    /// 过滤后仍保留的线程片段数量。
    pub(crate) retained_thread_count: usize,
}

/// 线程正文过滤的轻量摘要。
///
/// 业务意图：
/// - 过滤后的 tab 需要在工具条中提示当前内容已经被裁剪，但不应持有整份过滤结果副本。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ThreadDumpLineFilterSummary {
    /// 当前日志中识别到的 thread dump 快照数量。
    pub(crate) snapshot_count: usize,
    /// 被隐藏的线程片段数量。
    pub(crate) hidden_thread_count: usize,
    /// 过滤后仍保留的线程片段数量。
    pub(crate) retained_thread_count: usize,
}

impl ThreadDumpLineFilterResult {
    /// 返回可保存在 tab 状态中的轻量摘要。
    pub(crate) fn summary(&self) -> ThreadDumpLineFilterSummary {
        ThreadDumpLineFilterSummary {
            snapshot_count: self.snapshot_count,
            hidden_thread_count: self.hidden_thread_count,
            retained_thread_count: self.retained_thread_count,
        }
    }
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

/// 判断当前解码行是否包含可识别的 Java thread dump 快照。
///
/// 业务意图：
/// - 日志查看器工具条只应在当前文件确实是 Java 线程日志时展示“过滤线程”按钮，避免普通应用日志出现无效入口。
/// - 判定逻辑复用线程日志分析解析器，而不是另写只看文件名或关键字的启发式规则，保证按钮可见性和实际过滤能力一致。
///
/// 边界条件：
/// - 空文件、只有 `Full thread dump` 但没有线程状态行、普通异常堆栈都会返回 `false`。
/// - 该函数只消费已经解码的内存行，不读取文件系统；分页大文件暂不在渲染路径同步扫描整份内容。
pub(crate) fn has_java_thread_dump_snapshots(
    lines: &[String],
    source_name: &str,
    source: &LogFileSource,
) -> bool {
    !parse_thread_dump_snapshots(lines, source_name, 0, source).is_empty()
}

/// 使用线程日志分析的同一过滤语义过滤当前日志正文行。
///
/// 业务意图：
/// - “过滤线程”按钮要隐藏的线程集合必须和线程日志分析窗口一致：先应用用户配置的线程名/堆栈规则，
///   再应用默认的“只出现一次线程不展示”规则。
/// - 输出仍是普通日志行列表，主查看器可以继续复用原有虚拟列表、搜索高亮、复制和行号渲染能力。
///
/// 边界条件：
/// - 未识别到 thread dump 时返回 `None`，调用方不应改变当前正文。
/// - 过滤只影响当前 tab 的展示状态，不修改源文件、压缩包成员或持久化配置。
/// - 时间戳和 `Full thread dump` 边界不属于线程片段，即使相邻线程被隐藏也会保留，方便用户判断剩余内容来自哪个快照。
pub(crate) fn filter_thread_dump_lines_with_analysis_rules(
    lines: &[String],
    source_name: &str,
    source: &LogFileSource,
    filter_rules: &[ThreadAnalysisFilterRule],
) -> Option<ThreadDumpLineFilterResult> {
    let snapshots = parse_thread_dump_snapshots(lines, source_name, 0, source);
    if snapshots.is_empty() {
        return None;
    }

    let mut snapshots_after_config_filter = snapshots.clone();
    for snapshot in &mut snapshots_after_config_filter {
        snapshot
            .threads
            .retain(|sample| !thread_sample_matches_filter_rules(sample, filter_rules));
    }
    let visible_thread_name_set = default_visible_thread_names(&snapshots_after_config_filter, 1);

    let mut hidden_lines = vec![false; lines.len()];
    let mut hidden_thread_count = 0usize;
    let mut retained_thread_count = 0usize;
    for snapshot in &snapshots {
        for sample in &snapshot.threads {
            let hidden_by_config = thread_sample_matches_filter_rules(sample, filter_rules);
            let hidden_by_default = !visible_thread_name_set.contains(&sample.name);
            if hidden_by_config || hidden_by_default {
                hidden_thread_count = hidden_thread_count.saturating_add(1);
                mark_thread_sample_lines_hidden(lines.len(), sample, &mut hidden_lines);
            } else {
                retained_thread_count = retained_thread_count.saturating_add(1);
            }
        }
    }

    let filtered_lines = lines
        .iter()
        .enumerate()
        .filter_map(|(line_index, line)| (!hidden_lines[line_index]).then(|| line.clone()))
        .collect::<Vec<_>>();

    Some(ThreadDumpLineFilterResult {
        filtered_lines,
        snapshot_count: snapshots.len(),
        hidden_thread_count,
        retained_thread_count,
    })
}

/// 标记单个线程片段在原日志行集合中的隐藏范围。
///
/// 业务意图：
/// - 线程过滤需要隐藏从线程头到该线程片段末尾的连续行，但不能把下一个快照的时间戳一起隐藏。
/// - 解析器为了悬浮详情保留了较完整的 `stack_lines`，这里按展示过滤场景再裁剪一次边界。
fn mark_thread_sample_lines_hidden(
    total_line_count: usize,
    sample: &ThreadStateSample,
    hidden_lines: &mut [bool],
) {
    let visible_stack_line_count = thread_sample_filter_line_count(&sample.stack_lines);
    let start = sample.line_index.min(total_line_count);
    let end = sample
        .line_index
        .saturating_add(visible_stack_line_count)
        .min(total_line_count);
    for hidden in hidden_lines.iter_mut().take(end).skip(start) {
        *hidden = true;
    }
}

/// 返回线程片段在正文过滤场景中应隐藏的行数。
///
/// 业务意图：
/// - thread dump 常在下一个 `Full thread dump` 前先输出时间戳；解析阶段可能会把该时间戳暂时收进上一条线程片段。
/// - 正文过滤必须保留这些快照边界行，否则用户会失去时间线线索，因此这里从线程片段尾部剔除时间戳及其前置空行。
fn thread_sample_filter_line_count(stack_lines: &[String]) -> usize {
    let mut end = stack_lines.len();
    loop {
        let mut candidate_end = end;
        while candidate_end > 1 && stack_lines[candidate_end - 1].trim().is_empty() {
            candidate_end = candidate_end.saturating_sub(1);
        }
        if candidate_end > 1
            && extract_thread_dump_timestamp(&stack_lines[candidate_end - 1]).is_some()
        {
            end = candidate_end.saturating_sub(1);
        } else {
            break;
        }
    }
    end.max(1)
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

/// 判断线程片段中的一行是否是线程头。
///
/// 业务意图：
/// - Java thread dump 的线程头通常包含线程名、tid、nid、prio 等高度唯一的信息；堆栈聚类时必须排除，
///   否则同一调用栈会被线程名或线程 ID 拆成多个组。
fn is_thread_stack_header_line(trimmed: &str) -> bool {
    trimmed.starts_with('"')
}

/// 判断线程片段中的一行是否是 JVM 状态行。
///
/// 业务意图：
/// - 状态本身已经进入状态分布列，不参与堆栈指纹；否则同一调用栈在 RUNNABLE 和 WAITING 间切换时会被拆组。
fn is_thread_stack_state_line(trimmed: &str) -> bool {
    trimmed.starts_with("java.lang.Thread.State:")
}

/// 归一化日志行中的十六进制地址。
///
/// 业务意图：
/// - 锁对象、native thread id 和对象地址每次 JVM 运行都可能不同；聚类关注调用栈形态，不应被这些地址打散。
/// - 该函数只处理 ASCII 地址模式，不改写中文类名、线程名或其它非 ASCII 文本，避免破坏 UTF-8 边界。
fn normalize_stack_hex_addresses(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < text.len() {
        let remaining = &text[index..];
        if remaining.starts_with("<0x")
            && let Some(end_offset) = remaining.find('>')
        {
            normalized.push_str("<addr>");
            index = index.saturating_add(end_offset + 1);
            continue;
        }
        if remaining.starts_with("0x") {
            let mut end = index + 2;
            while end < text.len() && text.as_bytes()[end].is_ascii_hexdigit() {
                end += 1;
            }
            if end > index + 2 {
                normalized.push_str("0x");
                index = end;
                continue;
            }
        }
        let Some(character) = remaining.chars().next() else {
            break;
        };
        normalized.push(character);
        index += character.len_utf8();
    }
    normalized
}

/// 归一化单条堆栈行。
///
/// 业务意图：
/// - Java 栈帧的源码行号会随版本变化或编译参数变化而不同；同一个方法调用应归为同一类堆栈。
/// - 非 `at` 行（锁等待、ownable synchronizer 等）仍保留语义，只清理地址，避免把锁相关问题完全抹掉。
fn normalize_thread_stack_line(trimmed: &str) -> String {
    let normalized = normalize_stack_hex_addresses(trimmed);
    if let Some(frame) = normalized.strip_prefix("at ") {
        let method = frame
            .split_once('(')
            .map(|(method, _)| method)
            .unwrap_or(frame)
            .trim();
        if !method.is_empty() {
            return format!("at {method}");
        }
    }
    normalized
}

/// 返回堆栈聚类使用的规范化行序列。
///
/// 边界条件：
/// - 完全没有方法帧的线程仍要进入并发分析，因此使用固定空堆栈占位键，避免被丢弃。
fn normalized_thread_stack_lines(stack_lines: &[String]) -> Vec<String> {
    let mut normalized_lines = Vec::new();
    for line in stack_lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if is_thread_stack_header_line(trimmed) {
            continue;
        }
        if is_thread_stack_state_line(trimmed) {
            continue;
        }
        normalized_lines.push(normalize_thread_stack_line(trimmed));
    }
    if normalized_lines.is_empty() {
        normalized_lines.push("<empty-stack>".to_string());
    }
    normalized_lines
}

/// 返回堆栈并发分析聚合键。
///
/// 业务意图：
/// - UI 点击聚合行时需要重新匹配完整样本；把聚合键做成领域层公共函数可以保证构建和点击使用同一套规范化规则。
pub(crate) fn thread_stack_fingerprint_key(stack_lines: &[String]) -> String {
    normalized_thread_stack_lines(stack_lines).join("\n")
}

/// 返回堆栈并发分析的代表性展示文本。
///
/// 业务意图：
/// - 优先展示第一个方法帧；如果线程只包含锁信息或空栈，则展示第一条保留下来的规范化行。
fn thread_stack_fingerprint_title(normalized_lines: &[String]) -> String {
    normalized_lines
        .iter()
        .find(|line| line.starts_with("at "))
        .or_else(|| normalized_lines.first())
        .cloned()
        .unwrap_or_else(|| "<empty-stack>".to_string())
}

/// 构建堆栈并发分析聚合行。
///
/// 业务意图：
/// - 并发分析统计口径是“用户过滤规则之后的全部线程样本”，不能复用频率时间线的默认可见集合，
///   否则只出现一次但对并发排查有价值的线程会被误隐藏。
/// - 排序按最大单快照并发数降序，其次按总出现次数降序；再按首次出现顺序和堆栈摘要兜底，保证多次打开同一批日志顺序一致。
fn build_thread_concurrency_rows(snapshots: &[ThreadSnapshot]) -> Vec<ThreadConcurrencyRow> {
    let mut rows_by_stack = HashMap::<String, ThreadConcurrencyAccumulator>::new();
    let mut next_first_seen_order = 0usize;

    for (snapshot_index, snapshot) in snapshots.iter().enumerate() {
        for sample in &snapshot.threads {
            let normalized_lines = normalized_thread_stack_lines(&sample.stack_lines);
            let stack_key = normalized_lines.join("\n");
            let stack_title = thread_stack_fingerprint_title(&normalized_lines);
            let accumulator = rows_by_stack.entry(stack_key.clone()).or_insert_with(|| {
                let first_seen_order = next_first_seen_order;
                next_first_seen_order = next_first_seen_order.saturating_add(1);
                ThreadConcurrencyAccumulator {
                    row: ThreadConcurrencyRow {
                        stack_key,
                        stack_title,
                        total_count: 0,
                        max_snapshot_concurrency: 0,
                        runnable_count: 0,
                        blocked_count: 0,
                        waiting_count: 0,
                        timed_waiting_count: 0,
                        other_count: 0,
                    },
                    first_seen_order,
                    snapshot_counts: HashMap::new(),
                }
            });
            accumulator.row.push_state(sample.state);
            let snapshot_count = accumulator
                .snapshot_counts
                .entry(snapshot_index)
                .or_insert(0);
            *snapshot_count = snapshot_count.saturating_add(1);
            accumulator.row.max_snapshot_concurrency = accumulator
                .row
                .max_snapshot_concurrency
                .max(*snapshot_count);
        }
    }

    let mut rows = rows_by_stack.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .row
            .max_snapshot_concurrency
            .cmp(&left.row.max_snapshot_concurrency)
            .then_with(|| right.row.total_count.cmp(&left.row.total_count))
            .then_with(|| left.first_seen_order.cmp(&right.first_seen_order))
            .then_with(|| left.row.stack_title.cmp(&right.row.stack_title))
    });

    rows.into_iter()
        .map(|accumulator| accumulator.row)
        .collect()
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
    snapshots: Vec<ThreadSnapshot>,
    filter_rules: &[ThreadAnalysisFilterRule],
) -> ThreadAnalysisData {
    let filter_rule_states = filter_rules
        .iter()
        .cloned()
        .map(ThreadAnalysisFilterRuleState::enabled)
        .collect::<Vec<_>>();
    build_thread_analysis_data_with_filter_states(
        source_count,
        skipped_files,
        snapshots,
        filter_rule_states,
        true,
    )
}

/// 使用当前过滤规则启用状态重建线程分析窗口数据。
///
/// 业务意图：
/// - 线程分析窗口的“过滤”弹层允许临时停用部分规则；重建时必须保留原始解析快照和文件统计，只改变过滤后的矩阵与摘要。
/// - 该函数是纯内存计算，不读取日志来源，也不保存设置，确保窗口内试错操作足够轻量且不会改变长期配置。
pub(crate) fn rebuild_thread_analysis_data_with_filter_states(
    analysis: &ThreadAnalysisData,
    filter_rule_states: Vec<ThreadAnalysisFilterRuleState>,
) -> ThreadAnalysisData {
    rebuild_thread_analysis_data_with_filter_options(
        analysis,
        filter_rule_states,
        analysis.hide_single_occurrence_threads,
    )
}

/// 使用当前过滤规则和内置降噪选项重建线程分析窗口数据。
///
/// 业务意图：
/// - 过滤弹层既可以临时启停用户规则，也可以取消“隐藏只出现一次线程”的内置规则；两类开关统一走同一条重算路径。
pub(crate) fn rebuild_thread_analysis_data_with_filter_options(
    analysis: &ThreadAnalysisData,
    filter_rule_states: Vec<ThreadAnalysisFilterRuleState>,
    hide_single_occurrence_threads: bool,
) -> ThreadAnalysisData {
    build_thread_analysis_data_with_filter_states(
        analysis.source_count,
        analysis.skipped_files,
        analysis.raw_snapshots.clone(),
        filter_rule_states,
        hide_single_occurrence_threads,
    )
}

/// 按指定过滤规则状态构建线程分析窗口可直接渲染的数据矩阵。
///
/// 边界条件：
/// - `raw_snapshots` 必须始终是不带用户过滤的解析结果；函数内部会复制出当前过滤快照，避免启停规则时丢失可恢复样本。
/// - 进度态不会调用该函数；进度数据由 UI 构造空快照占位。
pub(crate) fn build_thread_analysis_data_with_filter_states(
    source_count: usize,
    skipped_files: usize,
    raw_snapshots: Vec<ThreadSnapshot>,
    filter_rule_states: Vec<ThreadAnalysisFilterRuleState>,
    hide_single_occurrence_threads: bool,
) -> ThreadAnalysisData {
    let mut snapshots = raw_snapshots.clone();
    let active_filter_rules = filter_rule_states
        .iter()
        .filter(|state| state.enabled)
        .map(|state| state.rule.clone())
        .collect::<Vec<_>>();
    // `source_index` 仍是快照来源定位元数据；虽然当前“只出现一次”过滤不再按文件数判断，
    // 构建阶段仍读取一次以保持字段参与主流程，避免后续恢复跨文件统计时误删该边界信息。
    let _has_snapshot_source_index = snapshots
        .iter()
        .any(|snapshot| snapshot.source_index < source_count);
    let mut filtered_threads = 0usize;
    if !active_filter_rules.is_empty() {
        for snapshot in &mut snapshots {
            let before = snapshot.threads.len();
            snapshot
                .threads
                .retain(|sample| !thread_sample_matches_filter_rules(sample, &active_filter_rules));
            filtered_threads += before.saturating_sub(snapshot.threads.len());
        }
    }
    let _has_snapshot_labels = snapshots.iter().any(|snapshot| !snapshot.label.is_empty());
    let concurrency_rows = build_thread_concurrency_rows(&snapshots);
    let all_thread_name_set = snapshots
        .iter()
        .flat_map(|snapshot| snapshot.threads.iter().map(|sample| sample.name.clone()))
        .collect::<BTreeSet<_>>();
    let visible_thread_name_set = if hide_single_occurrence_threads {
        default_visible_thread_names(&snapshots, source_count)
    } else {
        all_thread_name_set.iter().cloned().collect::<HashSet<_>>()
    };
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
        source_count,
        skipped_files,
        title: "线程日志分析".to_string(),
        summary,
        progress: None,
        raw_snapshots,
        filter_rule_states,
        hide_single_occurrence_threads,
        snapshots,
        thread_names,
        matrix,
        concurrency_rows,
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
