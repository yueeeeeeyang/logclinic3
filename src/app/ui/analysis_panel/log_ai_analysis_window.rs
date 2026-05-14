// 日志智能分析独立窗口。
//
// 业务意图：
// - 用户从日志树右键触发智能分析后，需要一个不遮挡主日志浏览上下文的独立窗口持续展示流式结果。
// - 窗口只保存当前会话内的分析状态，不写入数据库；关闭窗口即表示用户放弃本次可视结果。
//
// 关键约束：
// - 后台任务可能正在读取大日志或等待模型响应，窗口停止和关闭时必须设置取消标记，避免后台继续无意义发送请求。
// - 思考过程只展示模型服务显式返回的 reasoning 字段，与正式分析结果分开显示。

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// 日志智能分析窗口默认宽度。
pub(in crate::app) const LOG_AI_ANALYSIS_WINDOW_WIDTH: f32 = 980.0;
/// 日志智能分析窗口默认高度。
pub(in crate::app) const LOG_AI_ANALYSIS_WINDOW_HEIGHT: f32 = 720.0;
/// 日志智能分析窗口列表内容最大宽度。
const LOG_AI_ANALYSIS_CONTENT_MAX_WIDTH: f32 = 1560.0;
/// 追问输入区单行高度。
const LOG_AI_ANALYSIS_FOLLOW_UP_INPUT_LINE_HEIGHT: f32 = 20.0;
/// 追问输入历史最多保留的撤回快照数。
const LOG_AI_ANALYSIS_FOLLOW_UP_UNDO_LIMIT: usize = 64;

/// 追问输入区中的单行排版缓存。
///
/// 业务意图：
/// - 追问输入需要支持光标、选区、鼠标命中和 IME 候选窗口定位，因此必须保存每行的真实字形布局。
struct LogAiAnalysisFollowUpInputLineLayout {
    /// 当前可视行对应的原始文本 UTF-8 字节范围。
    byte_range: Range<usize>,
    /// 当前行的 GPUI 字形布局。
    line: ShapedLine,
    /// 当前行在窗口中的绘制边界。
    bounds: Bounds<Pixels>,
}

/// 追问输入区绘制状态。
struct LogAiAnalysisFollowUpInputPrepaint {
    /// 当前帧需要绘制的所有文本行。
    lines: Vec<LogAiAnalysisFollowUpInputPaintLine>,
    /// 当前选择范围对应的高亮矩形。
    selections: Vec<PaintQuad>,
    /// 当前光标矩形。
    cursor: Option<PaintQuad>,
}

/// 追问输入区单行绘制数据。
struct LogAiAnalysisFollowUpInputPaintLine {
    /// 当前行对应的原始文本范围。
    byte_range: Range<usize>,
    /// 当前行边界。
    bounds: Bounds<Pixels>,
    /// 已排版的文本行。
    line: ShapedLine,
}

/// 追问输入框撤回/重做使用的轻量快照。
#[derive(Clone, Debug, PartialEq, Eq)]
struct LogAiAnalysisFollowUpInputSnapshot {
    /// 输入框正文。
    text: String,
    /// 当前选择范围。
    selection_range: Range<usize>,
    /// 输入法组合文本范围。
    marked_range: Option<Range<usize>>,
}

/// 判断是否需要展示总进度和批次导航。
///
/// 业务意图：
/// - 单批次分析结果等同最终结果，继续展示进度条和批次切换只会增加阅读负担。
fn should_show_log_ai_analysis_progress(total_chunks: usize) -> bool {
    total_chunks > 1
}

/// 计算总进度条比例。
///
/// 业务意图：
/// - 多批次时进度按已完成批次数计算；批次完成但最终汇总仍在生成时保持分块进度，窗口文案单独提示“正在生成最终汇总”。
/// - 完成状态强制展示 100%，避免汇总完成后因为计数延迟显示为 99%。
fn log_ai_analysis_progress_fraction(
    status: LogAiAnalysisWindowStatus,
    completed_chunks: usize,
    total_chunks: usize,
) -> f32 {
    if total_chunks == 0 {
        return 0.0;
    }
    if status == LogAiAnalysisWindowStatus::Complete {
        return 1.0;
    }
    (completed_chunks.min(total_chunks) as f32 / total_chunks as f32).clamp(0.0, 1.0)
}

/// 返回总进度卡片的阶段文案。
///
/// 业务意图：
/// - 用户需要区分“日志批次仍在分析”和“批次已完成但最终汇总仍在生成”，否则 100% 附近的等待容易误判为卡住。
fn log_ai_analysis_progress_label(
    status: LogAiAnalysisWindowStatus,
    summary_status: LogAiAnalysisSectionStatus,
    completed_chunks: usize,
    total_chunks: usize,
) -> &'static str {
    if total_chunks == 0 {
        return "等待生成批次";
    }
    if status == LogAiAnalysisWindowStatus::Complete {
        return "已完成";
    }
    if completed_chunks >= total_chunks && summary_status == LogAiAnalysisSectionStatus::Streaming {
        return "正在生成最终汇总";
    }
    match status {
        LogAiAnalysisWindowStatus::Paused => "已暂停，等待继续",
        LogAiAnalysisWindowStatus::Stopped => "已停止",
        LogAiAnalysisWindowStatus::Failed => "分析失败",
        _ => "正在分析日志批次",
    }
}

/// 把当前批次序号限制在有效范围内。
///
/// 边界条件：
/// - 后台刚开始准备时可能还没有任何批次；此时返回 0，调用方会结合总数避免渲染越界。
fn clamp_log_ai_analysis_chunk_index(index: usize, total_chunks: usize) -> usize {
    if total_chunks == 0 {
        0
    } else {
        index.min(total_chunks.saturating_sub(1))
    }
}

/// 日志智能分析独立窗口根视图。
pub(in crate::app) struct LogAiAnalysisWindowView {
    /// 主窗口视图实体。
    ///
    /// 业务意图：
    /// - 独立窗口需要读取主题和系统外观，保持与主窗口一致；不直接修改主窗口日志状态。
    pub(in crate::app) main_view: Entity<MainView>,
    /// 当前使用的模型配置。
    pub(in crate::app) profile: ModelProfile,
    /// 当前分析目标。
    pub(in crate::app) targets: Vec<LogAiAnalysisTarget>,
    /// 窗口展示状态。
    state: LogAiAnalysisWindowState,
    /// 后台任务事件接收器。
    receiver: Option<std::sync::mpsc::Receiver<LogAiAnalysisRuntimeEvent>>,
    /// 后台任务取消标记。
    cancel: Option<Arc<AtomicBool>>,
    /// 主滚动区域句柄。
    scroll_handle: ScrollHandle,
    /// 追问输入框焦点。
    ///
    /// 业务意图：
    /// - 追问输入只属于当前智能分析窗口，关闭窗口即丢弃，不复用主 AI 对话输入状态。
    follow_up_input_focus: gpui::FocusHandle,
    /// 主窗口状态变更订阅。
    _main_view_subscription: gpui::Subscription,
}

impl Drop for LogAiAnalysisWindowView {
    /// 窗口实体释放时停止后台任务。
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl LogAiAnalysisWindowView {
    /// 创建日志智能分析窗口。
    pub(in crate::app) fn new(
        main_view: Entity<MainView>,
        profile: ModelProfile,
        targets: Vec<LogAiAnalysisTarget>,
        context: &mut Context<Self>,
    ) -> Self {
        let observed_main_view = main_view.clone();
        let main_view_subscription = context.observe(&observed_main_view, |_, _, context| {
            context.notify();
        });
        let mut view = Self {
            main_view,
            profile,
            targets,
            state: LogAiAnalysisWindowState::new(),
            receiver: None,
            cancel: None,
            scroll_handle: ScrollHandle::new(),
            follow_up_input_focus: context.focus_handle(),
            _main_view_subscription: main_view_subscription,
        };
        view.start_analysis_from(0, Vec::new(), context);
        view
    }

    /// 替换为新的分析任务并复用当前窗口。
    pub(in crate::app) fn restart(
        &mut self,
        profile: ModelProfile,
        targets: Vec<LogAiAnalysisTarget>,
        context: &mut Context<Self>,
    ) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        self.profile = profile;
        self.targets = targets;
        self.state = LogAiAnalysisWindowState::new();
        self.receiver = None;
        self.cancel = None;
        self.scroll_handle = ScrollHandle::new();
        self.follow_up_input_focus = context.focus_handle();
        self.start_analysis_from(0, Vec::new(), context);
        context.notify();
    }

    /// 从指定分块开始启动后台分析。
    ///
    /// 业务意图：
    /// - 首次分析从 0 开始；达到批次数上限后继续按钮会从暂停位置重新构造分块并跳过已完成批次。
    fn start_analysis_from(
        &mut self,
        start_index: usize,
        previous_summaries: Vec<LogAiAnalysisCompletedChunk>,
        context: &mut Context<Self>,
    ) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let profile = self.profile.clone();
        let targets = self.targets.clone();
        let cancel_for_task = cancel.clone();
        self.receiver = Some(receiver);
        self.cancel = Some(cancel);
        self.state.status = LogAiAnalysisWindowStatus::Running;
        self.state.message = format!("正在准备 {} 个日志来源...", self.targets.len());

        context
            .background_spawn(async move {
                run_log_ai_analysis(
                    profile,
                    targets,
                    start_index,
                    previous_summaries,
                    cancel_for_task,
                    sender,
                );
            })
            .detach();
        self.schedule_event_poll(context);
    }

    /// 安排窗口轮询后台事件。
    fn schedule_event_poll(&self, context: &mut Context<Self>) {
        context
            .spawn(async move |view, app| {
                loop {
                    app.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                    let keep_polling = view
                        .update(app, |view, context| {
                            view.drain_events(context);
                            view.receiver.is_some()
                        })
                        .unwrap_or(false);
                    if !keep_polling {
                        break;
                    }
                }
            })
            .detach();
    }

    /// 处理后台事件队列。
    fn drain_events(&mut self, context: &mut Context<Self>) {
        let mut received_event = false;
        while let Some(receiver) = self.receiver.as_ref() {
            let event = match receiver.try_recv() {
                Ok(event) => {
                    received_event = true;
                    event
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    received_event = true;
                    LogAiAnalysisRuntimeEvent::Done
                }
            };
            self.apply_event(event);
        }
        if received_event {
            context.notify();
        }
    }

    /// 应用单个后台事件。
    fn apply_event(&mut self, event: LogAiAnalysisRuntimeEvent) {
        match event {
            LogAiAnalysisRuntimeEvent::Prepared {
                ranges,
                start_index,
            } => {
                if self.state.chunks.is_empty() {
                    self.state.chunks = ranges
                        .into_iter()
                        .enumerate()
                        .map(|(index, range)| LogAiAnalysisChunkViewState::new(index, range))
                        .collect();
                    self.state.total_chunks = self.state.chunks.len();
                }
                self.state.next_chunk_index = start_index;
                self.state.active_chunk_index =
                    start_index.min(self.state.chunks.len().saturating_sub(1));
                self.state.message = format!(
                    "已拆分为 {} 批，正在从第 {} 批开始分析。",
                    self.state.chunks.len(),
                    start_index.saturating_add(1)
                );
            }
            LogAiAnalysisRuntimeEvent::ChunkStarted { index } => {
                self.state.current_chunk_index = Some(index);
                self.state.next_chunk_index = index;
                self.state.active_chunk_index =
                    index.min(self.state.chunks.len().saturating_sub(1));
                self.state.user_selected_chunk = false;
                self.state.message = format!(
                    "正在分析第 {} / {} 批",
                    index.saturating_add(1),
                    self.state.chunks.len().max(1)
                );
                if let Some(chunk) = self.state.chunks.get_mut(index) {
                    chunk.status = LogAiAnalysisSectionStatus::Streaming;
                }
            }
            LogAiAnalysisRuntimeEvent::ChunkRetrying {
                index,
                attempt,
                max_attempts,
                message,
            } => {
                self.state.current_chunk_index = Some(index);
                self.state.active_chunk_index =
                    index.min(self.state.chunks.len().saturating_sub(1));
                self.state.user_selected_chunk = false;
                self.state.message = format!(
                    "第 {} 批请求失败，正在重试 {}/{}：{}",
                    index.saturating_add(1),
                    attempt,
                    max_attempts,
                    message
                );
                if let Some(chunk) = self.state.chunks.get_mut(index) {
                    // 重试会重新发送同一批日志，必须清掉上一次半截流式输出，避免用户看到重复或混合内容。
                    chunk.status = LogAiAnalysisSectionStatus::Streaming;
                    chunk.reasoning.clear();
                    chunk.response.clear();
                    chunk.evidence_snippets.clear();
                    chunk.reasoning_expanded = false;
                }
            }
            LogAiAnalysisRuntimeEvent::ChunkReasoningDelta { index, delta } => {
                if let Some(chunk) = self.state.chunks.get_mut(index) {
                    chunk.reasoning.push_str(&delta);
                    if chunk.response.is_empty() {
                        chunk.reasoning_expanded = true;
                    }
                }
            }
            LogAiAnalysisRuntimeEvent::ChunkDelta { index, delta } => {
                if let Some(chunk) = self.state.chunks.get_mut(index) {
                    if chunk.response.is_empty() && !chunk.reasoning.is_empty() {
                        chunk.reasoning_expanded = false;
                    }
                    chunk.response.push_str(&delta);
                }
            }
            LogAiAnalysisRuntimeEvent::ChunkFinished {
                index,
                evidence_snippets,
            } => {
                if let Some(chunk) = self.state.chunks.get_mut(index) {
                    chunk.status = LogAiAnalysisSectionStatus::Complete;
                    chunk.reasoning_expanded = false;
                    chunk.evidence_snippets = evidence_snippets;
                }
                self.state.completed_chunks = self.state.completed_chunks.saturating_add(1);
                self.state.next_chunk_index = index.saturating_add(1);
            }
            LogAiAnalysisRuntimeEvent::SummaryStarted => {
                self.state.summary.status = LogAiAnalysisSectionStatus::Streaming;
                self.state.summary.reasoning_expanded = true;
                self.state.message = "正在生成最终汇总...".to_string();
            }
            LogAiAnalysisRuntimeEvent::SummaryRetrying {
                attempt,
                max_attempts,
                message,
            } => {
                self.state.summary.status = LogAiAnalysisSectionStatus::Streaming;
                self.state.summary.reasoning.clear();
                self.state.summary.response.clear();
                self.state.summary.reasoning_expanded = false;
                self.state.message = format!(
                    "最终汇总请求失败，正在重试 {}/{}：{}",
                    attempt, max_attempts, message
                );
            }
            LogAiAnalysisRuntimeEvent::SummaryReasoningDelta(delta) => {
                self.state.summary.reasoning.push_str(&delta);
                if self.state.summary.response.is_empty() {
                    self.state.summary.reasoning_expanded = true;
                }
            }
            LogAiAnalysisRuntimeEvent::SummaryDelta(delta) => {
                if self.state.summary.response.is_empty()
                    && !self.state.summary.reasoning.is_empty()
                {
                    self.state.summary.reasoning_expanded = false;
                }
                self.state.summary.response.push_str(&delta);
            }
            LogAiAnalysisRuntimeEvent::SummaryFinished => {
                self.state.summary.status = LogAiAnalysisSectionStatus::Complete;
                self.state.summary.reasoning_expanded = false;
            }
            LogAiAnalysisRuntimeEvent::FollowUpStarted { index } => {
                if let Some(follow_up) = self.state.follow_ups.get_mut(index) {
                    follow_up.status = LogAiAnalysisSectionStatus::Streaming;
                    follow_up.reasoning.clear();
                    follow_up.response.clear();
                    follow_up.error_message = None;
                    follow_up.reasoning_expanded = true;
                }
                self.state.message = format!("正在回答第 {} 个追问...", index + 1);
            }
            LogAiAnalysisRuntimeEvent::FollowUpRetrying {
                index,
                attempt,
                max_attempts,
                message,
            } => {
                if let Some(follow_up) = self.state.follow_ups.get_mut(index) {
                    follow_up.status = LogAiAnalysisSectionStatus::Streaming;
                    follow_up.reasoning.clear();
                    follow_up.response.clear();
                    follow_up.error_message = None;
                    follow_up.reasoning_expanded = false;
                }
                self.state.message = format!(
                    "第 {} 个追问请求失败，正在重试 {}/{}：{}",
                    index + 1,
                    attempt,
                    max_attempts,
                    message
                );
            }
            LogAiAnalysisRuntimeEvent::FollowUpReasoningDelta { index, delta } => {
                if let Some(follow_up) = self.state.follow_ups.get_mut(index) {
                    follow_up.reasoning.push_str(&delta);
                    if follow_up.response.is_empty() {
                        follow_up.reasoning_expanded = true;
                    }
                }
            }
            LogAiAnalysisRuntimeEvent::FollowUpDelta { index, delta } => {
                if let Some(follow_up) = self.state.follow_ups.get_mut(index) {
                    if follow_up.response.is_empty() && !follow_up.reasoning.is_empty() {
                        follow_up.reasoning_expanded = false;
                    }
                    follow_up.response.push_str(&delta);
                }
            }
            LogAiAnalysisRuntimeEvent::FollowUpFinished { index } => {
                if let Some(follow_up) = self.state.follow_ups.get_mut(index) {
                    follow_up.status = LogAiAnalysisSectionStatus::Complete;
                    follow_up.reasoning_expanded = false;
                }
                self.state.message = "分析完成".to_string();
                self.receiver = None;
                self.cancel = None;
            }
            LogAiAnalysisRuntimeEvent::FollowUpFailed { index, message } => {
                if let Some(follow_up) = self.state.follow_ups.get_mut(index) {
                    follow_up.status = LogAiAnalysisSectionStatus::Failed;
                    follow_up.error_message = Some(message.clone());
                    follow_up.reasoning_expanded = false;
                }
                self.state.message = message;
                self.receiver = None;
                self.cancel = None;
            }
            LogAiAnalysisRuntimeEvent::Paused {
                next_index,
                total,
                message,
            } => {
                self.state.status = LogAiAnalysisWindowStatus::Paused;
                self.state.next_chunk_index = next_index;
                self.state.message = message;
                self.state.total_chunks = total;
                self.receiver = None;
                self.cancel = None;
            }
            LogAiAnalysisRuntimeEvent::Done => {
                self.state.status = LogAiAnalysisWindowStatus::Complete;
                self.state.current_chunk_index = None;
                self.state.message = "分析完成".to_string();
                self.receiver = None;
                self.cancel = None;
            }
            LogAiAnalysisRuntimeEvent::Stopped => {
                self.state.status = LogAiAnalysisWindowStatus::Stopped;
                self.state.current_chunk_index = None;
                self.state.message = "已停止分析".to_string();
                self.receiver = None;
                self.cancel = None;
            }
            LogAiAnalysisRuntimeEvent::Error(message) => {
                self.state.status = LogAiAnalysisWindowStatus::Failed;
                self.state.current_chunk_index = None;
                self.state.message = message;
                self.receiver = None;
                self.cancel = None;
            }
        }
    }

    /// 停止当前分析任务。
    fn stop_analysis(&mut self, context: &mut Context<Self>) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        self.state.status = LogAiAnalysisWindowStatus::Stopped;
        self.state.message = "正在停止分析...".to_string();
        context.notify();
    }

    /// 继续下一轮分块分析。
    fn continue_analysis(&mut self, context: &mut Context<Self>) {
        if self.state.status != LogAiAnalysisWindowStatus::Paused {
            return;
        }
        let previous_summaries = self.completed_chunk_summaries();
        let start_index = self.state.next_chunk_index;
        self.start_analysis_from(start_index, previous_summaries, context);
        context.notify();
    }

    /// 收集已经完成的批次结果，用于继续后最终汇总。
    fn completed_chunk_summaries(&self) -> Vec<LogAiAnalysisCompletedChunk> {
        self.state
            .chunks
            .iter()
            .filter(|chunk| chunk.status == LogAiAnalysisSectionStatus::Complete)
            .filter(|chunk| !chunk.response.trim().is_empty())
            .map(|chunk| LogAiAnalysisCompletedChunk {
                range: chunk.range.clone(),
                response: chunk.response.clone(),
                evidence_snippets: chunk.evidence_snippets.clone(),
            })
            .collect()
    }

    /// 选择最终汇总下方需要展示的原日志片段。
    ///
    /// 业务意图：
    /// - 优先按最终汇总中出现的证据 ID 过滤；模型没有引用 ID 时回退展示所有本地提取片段。
    fn visible_summary_evidence_snippets(&self) -> Vec<LogAiAnalysisEvidenceSnippet> {
        let completed_chunks = self.completed_chunk_summaries();
        let snippets = all_log_ai_analysis_evidence_snippets(&completed_chunks);
        select_log_ai_analysis_summary_evidence_snippets(&self.state.summary.response, &snippets)
    }

    /// 切换某个批次思考过程展示状态。
    fn toggle_chunk_reasoning(&mut self, index: usize, context: &mut Context<Self>) {
        if let Some(chunk) = self.state.chunks.get_mut(index) {
            chunk.reasoning_expanded = !chunk.reasoning_expanded;
            context.notify();
        }
    }

    /// 切换最终汇总思考过程展示状态。
    fn toggle_summary_reasoning(&mut self, context: &mut Context<Self>) {
        self.state.summary.reasoning_expanded = !self.state.summary.reasoning_expanded;
        context.notify();
    }

    /// 切换追问思考过程展示状态。
    fn toggle_follow_up_reasoning(&mut self, index: usize, context: &mut Context<Self>) {
        if let Some(follow_up) = self.state.follow_ups.get_mut(index) {
            follow_up.reasoning_expanded = !follow_up.reasoning_expanded;
            context.notify();
        }
    }

    /// 切换最终汇总的原日志片段展示状态。
    fn toggle_summary_evidence(&mut self, context: &mut Context<Self>) {
        self.state.summary.evidence_expanded = !self.state.summary.evidence_expanded;
        context.notify();
    }

    /// 判断当前是否可以发送追问。
    fn can_send_follow_up(&self) -> bool {
        self.state.status == LogAiAnalysisWindowStatus::Complete
            && !self.state.follow_up_input.trim().is_empty()
            && !self.is_follow_up_streaming()
    }

    /// 判断是否存在正在生成的追问。
    fn is_follow_up_streaming(&self) -> bool {
        self.state
            .follow_ups
            .iter()
            .any(|follow_up| follow_up.status == LogAiAnalysisSectionStatus::Streaming)
    }

    /// 发送完成后的追问。
    fn send_follow_up(&mut self, context: &mut Context<Self>) {
        if !self.can_send_follow_up() {
            return;
        }
        let question = self.state.follow_up_input.trim().to_string();
        self.state.follow_up_input.clear();
        self.state.follow_up_input_selection_range = 0..0;
        self.state.follow_up_input_marked_range = None;
        self.state.follow_up_input_selection_drag = None;
        self.state.follow_up_input_undo_stack.clear();
        self.state.follow_up_input_redo_stack.clear();
        let index = self.state.follow_ups.len();
        self.state
            .follow_ups
            .push(LogAiAnalysisFollowUpViewState::new(index, question.clone()));

        let profile = self.profile.clone();
        let completed_chunks = self.completed_chunk_summaries();
        let summary_response = self.effective_final_result_text();
        let visible_snippets = self.visible_summary_evidence_snippets();
        let history = self.completed_follow_up_history();
        let (sender, receiver) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_task = cancel.clone();
        self.receiver = Some(receiver);
        self.cancel = Some(cancel);
        self.state.message = format!("正在回答第 {} 个追问...", index + 1);
        context
            .background_spawn(async move {
                run_log_ai_analysis_follow_up(
                    profile,
                    completed_chunks,
                    summary_response,
                    visible_snippets,
                    history,
                    question,
                    index,
                    cancel_for_task,
                    sender,
                );
            })
            .detach();
        self.schedule_event_poll(context);
        context.notify();
    }

    /// 收集已完成追问历史，用于下一轮追问上下文。
    fn completed_follow_up_history(&self) -> Vec<LogAiAnalysisFollowUpExchange> {
        self.state
            .follow_ups
            .iter()
            .filter(|follow_up| follow_up.status == LogAiAnalysisSectionStatus::Complete)
            .filter(|follow_up| !follow_up.response.trim().is_empty())
            .map(|follow_up| LogAiAnalysisFollowUpExchange {
                question: follow_up.question.clone(),
                answer: follow_up.response.clone(),
            })
            .collect()
    }

    /// 返回追问使用的最终结论文本。
    fn effective_final_result_text(&self) -> String {
        if !self.state.summary.response.trim().is_empty() {
            return self.state.summary.response.clone();
        }
        if self.total_chunk_count() == 1 {
            return self
                .state
                .chunks
                .first()
                .map(|chunk| chunk.response.clone())
                .unwrap_or_default();
        }
        String::new()
    }

    /// 读取追问输入区当前文本、选择范围和组合文本范围的快照。
    fn follow_up_input_text_snapshot(&self) -> (String, Range<usize>, Option<Range<usize>>) {
        (
            self.state.follow_up_input.clone(),
            MainView::clamp_search_text_range(
                &self.state.follow_up_input,
                self.state.follow_up_input_selection_range.clone(),
            ),
            self.state.follow_up_input_marked_range.clone(),
        )
    }

    /// 保存追问输入区最近一次多行排版结果。
    fn store_follow_up_input_text_layouts(
        &mut self,
        layouts: Vec<LogAiAnalysisFollowUpInputLineLayout>,
        bounds: Bounds<Pixels>,
    ) {
        self.state.follow_up_input_last_layouts = layouts;
        self.state.follow_up_input_last_bounds = Some(bounds);
    }

    /// 返回追问输入区当前内容需要的可视行数。
    fn follow_up_input_visual_line_count(&self) -> usize {
        MainView::thread_analysis_filter_line_ranges(&self.state.follow_up_input)
            .len()
            .max(1)
    }

    /// 根据窗口坐标返回追问输入区 UTF-8 字节下标。
    fn follow_up_input_index_for_point(&self, position: Point<Pixels>) -> usize {
        if self.state.follow_up_input.is_empty() {
            return 0;
        }
        for layout in &self.state.follow_up_input_last_layouts {
            if position.y >= layout.bounds.top() && position.y <= layout.bounds.bottom() {
                return layout
                    .byte_range
                    .start
                    .saturating_add(
                        layout
                            .line
                            .closest_index_for_x(position.x - layout.bounds.left()),
                    )
                    .min(layout.byte_range.end);
            }
        }
        if let Some(bounds) = &self.state.follow_up_input_last_bounds
            && position.y < bounds.top()
        {
            return 0;
        }
        self.state.follow_up_input.len()
    }

    /// 开始追问输入区鼠标选择。
    fn start_follow_up_input_mouse_selection(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let index = self.follow_up_input_index_for_point(event.position);
        self.state.follow_up_input_marked_range = None;
        match event.click_count {
            0 | 1 => {
                if event.modifiers.shift {
                    self.state.follow_up_input_selection_range.end = index;
                    self.state.follow_up_input_selection_range = MainView::clamp_search_text_range(
                        &self.state.follow_up_input,
                        self.state.follow_up_input_selection_range.clone(),
                    );
                } else {
                    self.state.follow_up_input_selection_range = index..index;
                }
                self.state.follow_up_input_selection_drag =
                    Some(self.state.follow_up_input_selection_range.start);
            }
            2 => {
                self.state.follow_up_input_selection_range =
                    MainView::search_text_word_range_for_index(&self.state.follow_up_input, index);
                self.state.follow_up_input_selection_drag = None;
            }
            _ => {
                self.state.follow_up_input_selection_range = 0..self.state.follow_up_input.len();
                self.state.follow_up_input_selection_drag = None;
            }
        }
        context.notify();
    }

    /// 鼠标拖拽时更新追问输入区选区终点。
    fn update_follow_up_input_mouse_selection(
        &mut self,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) {
        let Some(anchor) = self.state.follow_up_input_selection_drag else {
            return;
        };
        let index = self.follow_up_input_index_for_point(position);
        self.state.follow_up_input_marked_range = None;
        self.state.follow_up_input_selection_range =
            MainView::clamp_search_text_range(&self.state.follow_up_input, anchor..index);
        context.notify();
    }

    /// 结束追问输入区鼠标拖拽选择。
    fn finish_follow_up_input_mouse_selection(&mut self, context: &mut Context<Self>) {
        if self.state.follow_up_input_selection_drag.take().is_some() {
            context.notify();
        }
    }

    /// 返回追问输入区当前选中文本。
    fn selected_follow_up_input_text(&self) -> Option<String> {
        let range = MainView::clamp_search_text_range(
            &self.state.follow_up_input,
            self.state.follow_up_input_selection_range.clone(),
        );
        (range.start < range.end).then(|| self.state.follow_up_input[range].to_string())
    }

    /// 构造追问输入框当前快照。
    fn follow_up_input_snapshot(&self) -> LogAiAnalysisFollowUpInputSnapshot {
        LogAiAnalysisFollowUpInputSnapshot {
            text: self.state.follow_up_input.clone(),
            selection_range: self.state.follow_up_input_selection_range.clone(),
            marked_range: self.state.follow_up_input_marked_range.clone(),
        }
    }

    /// 应用追问输入框快照。
    fn restore_follow_up_input_snapshot(&mut self, snapshot: LogAiAnalysisFollowUpInputSnapshot) {
        self.state.follow_up_input = snapshot.text;
        self.state.follow_up_input_selection_range = MainView::clamp_search_text_range(
            &self.state.follow_up_input,
            snapshot.selection_range,
        );
        self.state.follow_up_input_marked_range = snapshot
            .marked_range
            .map(|range| MainView::clamp_search_text_range(&self.state.follow_up_input, range));
        self.state.follow_up_input_selection_drag = None;
    }

    /// 记录追问输入框撤回快照。
    fn push_follow_up_input_undo_snapshot(&mut self) {
        let snapshot = self.follow_up_input_snapshot();
        if self
            .state
            .follow_up_input_undo_stack
            .last()
            .is_some_and(|last| last == &snapshot)
        {
            return;
        }
        self.state.follow_up_input_undo_stack.push(snapshot);
        if self.state.follow_up_input_undo_stack.len() > LOG_AI_ANALYSIS_FOLLOW_UP_UNDO_LIMIT {
            self.state.follow_up_input_undo_stack.remove(0);
        }
        self.state.follow_up_input_redo_stack.clear();
    }

    /// 用给定文本替换追问输入区当前选区。
    fn replace_follow_up_input_selection(&mut self, replacement: &str) {
        let replacement = replacement.replace("\r\n", "\n").replace('\r', "\n");
        let range = self
            .state
            .follow_up_input_marked_range
            .take()
            .unwrap_or_else(|| {
                MainView::clamp_search_text_range(
                    &self.state.follow_up_input,
                    self.state.follow_up_input_selection_range.clone(),
                )
            });
        let range = MainView::clamp_search_text_range(&self.state.follow_up_input, range);
        self.state
            .follow_up_input
            .replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        self.state.follow_up_input_selection_range = cursor..cursor;
    }

    /// 撤回追问输入框上一次编辑。
    fn undo_follow_up_input(&mut self, context: &mut Context<Self>) {
        let Some(snapshot) = self.state.follow_up_input_undo_stack.pop() else {
            return;
        };
        let current = self.follow_up_input_snapshot();
        self.state.follow_up_input_redo_stack.push(current);
        self.restore_follow_up_input_snapshot(snapshot);
        context.notify();
    }

    /// 重做追问输入框撤回后的编辑。
    fn redo_follow_up_input(&mut self, context: &mut Context<Self>) {
        let Some(snapshot) = self.state.follow_up_input_redo_stack.pop() else {
            return;
        };
        let current = self.follow_up_input_snapshot();
        self.state.follow_up_input_undo_stack.push(current);
        self.restore_follow_up_input_snapshot(snapshot);
        context.notify();
    }

    /// 处理追问输入框按键。
    ///
    /// 业务意图：
    /// - 追问输入要具备基础编辑能力；普通字符和中文 IME 继续交给平台输入协议，避免手写 key_char 破坏输入法。
    fn handle_follow_up_input_key_down(
        &mut self,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if MainView::is_undo_keystroke(&event.keystroke) {
            self.undo_follow_up_input(context);
            context.stop_propagation();
            return;
        }

        if MainView::is_redo_keystroke(&event.keystroke) {
            self.redo_follow_up_input(context);
            context.stop_propagation();
            return;
        }

        if MainView::is_paste_keystroke(&event.keystroke) {
            if let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) {
                self.push_follow_up_input_undo_snapshot();
                self.replace_follow_up_input_selection(&text);
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if MainView::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_follow_up_input_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return;
        }

        if MainView::is_cut_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_follow_up_input_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                self.push_follow_up_input_undo_snapshot();
                self.replace_follow_up_input_selection("");
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if MainView::is_select_all_keystroke(&event.keystroke) {
            self.state.follow_up_input_marked_range = None;
            self.state.follow_up_input_selection_range = 0..self.state.follow_up_input.len();
            context.stop_propagation();
            context.notify();
            return;
        }

        match event.keystroke.key.as_str() {
            "enter" => {
                if event.keystroke.modifiers.shift {
                    self.push_follow_up_input_undo_snapshot();
                    self.replace_follow_up_input_selection("\n");
                } else {
                    self.send_follow_up(context);
                }
                context.stop_propagation();
                context.notify();
            }
            "left" => {
                self.state.follow_up_input_marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.state.follow_up_input_selection_range.end =
                        MainView::previous_search_text_boundary(
                            &self.state.follow_up_input,
                            self.state.follow_up_input_selection_range.end,
                        );
                    self.state.follow_up_input_selection_range = MainView::clamp_search_text_range(
                        &self.state.follow_up_input,
                        self.state.follow_up_input_selection_range.clone(),
                    );
                } else if self.state.follow_up_input_selection_range.start
                    != self.state.follow_up_input_selection_range.end
                {
                    self.state.follow_up_input_selection_range =
                        self.state.follow_up_input_selection_range.start
                            ..self.state.follow_up_input_selection_range.start;
                } else {
                    let cursor = MainView::previous_search_text_boundary(
                        &self.state.follow_up_input,
                        self.state.follow_up_input_selection_range.end,
                    );
                    self.state.follow_up_input_selection_range = cursor..cursor;
                }
                context.stop_propagation();
                context.notify();
            }
            "right" => {
                self.state.follow_up_input_marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.state.follow_up_input_selection_range.end =
                        MainView::next_search_text_boundary(
                            &self.state.follow_up_input,
                            self.state.follow_up_input_selection_range.end,
                        );
                    self.state.follow_up_input_selection_range = MainView::clamp_search_text_range(
                        &self.state.follow_up_input,
                        self.state.follow_up_input_selection_range.clone(),
                    );
                } else if self.state.follow_up_input_selection_range.start
                    != self.state.follow_up_input_selection_range.end
                {
                    self.state.follow_up_input_selection_range =
                        self.state.follow_up_input_selection_range.end
                            ..self.state.follow_up_input_selection_range.end;
                } else {
                    let cursor = MainView::next_search_text_boundary(
                        &self.state.follow_up_input,
                        self.state.follow_up_input_selection_range.end,
                    );
                    self.state.follow_up_input_selection_range = cursor..cursor;
                }
                context.stop_propagation();
                context.notify();
            }
            "up" => {
                self.state.follow_up_input_marked_range = None;
                self.state.follow_up_input_selection_range = 0..0;
                context.stop_propagation();
                context.notify();
            }
            "down" => {
                self.state.follow_up_input_marked_range = None;
                let cursor = self.state.follow_up_input.len();
                self.state.follow_up_input_selection_range = cursor..cursor;
                context.stop_propagation();
                context.notify();
            }
            "backspace" => {
                let before = self.state.follow_up_input.clone();
                if self.state.follow_up_input_selection_range.start
                    != self.state.follow_up_input_selection_range.end
                    || self.state.follow_up_input_marked_range.is_some()
                {
                    self.push_follow_up_input_undo_snapshot();
                    self.replace_follow_up_input_selection("");
                } else if let Some((previous_index, _)) = self.state.follow_up_input
                    [..self.state.follow_up_input_selection_range.end]
                    .char_indices()
                    .next_back()
                {
                    self.push_follow_up_input_undo_snapshot();
                    let cursor = self.state.follow_up_input_selection_range.end;
                    self.state
                        .follow_up_input
                        .replace_range(previous_index..cursor, "");
                    self.state.follow_up_input_selection_range = previous_index..previous_index;
                    self.state.follow_up_input_marked_range = None;
                }
                if before != self.state.follow_up_input {
                    context.notify();
                }
                context.stop_propagation();
            }
            "delete" => {
                let before = self.state.follow_up_input.clone();
                if self.state.follow_up_input_selection_range.start
                    != self.state.follow_up_input_selection_range.end
                    || self.state.follow_up_input_marked_range.is_some()
                {
                    self.push_follow_up_input_undo_snapshot();
                    self.replace_follow_up_input_selection("");
                } else if let Some((next_index, next_character)) = self.state.follow_up_input
                    [self.state.follow_up_input_selection_range.end..]
                    .char_indices()
                    .next()
                {
                    self.push_follow_up_input_undo_snapshot();
                    let start = self.state.follow_up_input_selection_range.end + next_index;
                    let end = start + next_character.len_utf8();
                    self.state.follow_up_input.replace_range(start..end, "");
                    self.state.follow_up_input_selection_range = start..start;
                    self.state.follow_up_input_marked_range = None;
                }
                if before != self.state.follow_up_input {
                    context.notify();
                }
                context.stop_propagation();
            }
            "escape" => {}
            _ => {}
        }
    }

    /// 切换当前展示批次。
    fn switch_active_chunk(&mut self, delta: isize, context: &mut Context<Self>) {
        let total = self.total_chunk_count();
        if total == 0 {
            return;
        }
        let current = clamp_log_ai_analysis_chunk_index(self.state.active_chunk_index, total);
        let next = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current
                .saturating_add(delta as usize)
                .min(total.saturating_sub(1))
        };
        self.state.active_chunk_index = next;
        self.state.user_selected_chunk = true;
        context.notify();
    }

    /// 返回已知总批次数。
    fn total_chunk_count(&self) -> usize {
        self.state.chunks.len().max(self.state.total_chunks)
    }

    /// 是否为多批次分析。
    fn has_multiple_chunks(&self) -> bool {
        should_show_log_ai_analysis_progress(self.total_chunk_count())
    }

    /// 渲染窗口顶部状态栏。
    fn render_header(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let can_stop = matches!(self.state.status, LogAiAnalysisWindowStatus::Running);
        let can_continue = self.state.status == LogAiAnalysisWindowStatus::Paused;
        div()
            .id("log-ai-analysis-header")
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_5()
            .py_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(MainView::render_lucide_icon(
                                Some(Icon::Sparkles),
                                18.0,
                                18.0,
                                palette.accent,
                            ))
                            .child("日志智能分析"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child(format!(
                                "{} · {} · {}",
                                self.profile.name, self.profile.model, self.state.message
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.render_status_badge(palette))
                    .when(can_continue, |actions| {
                        actions.child(self.render_header_button(
                            "继续",
                            Icon::RefreshCw,
                            LogAiAnalysisHeaderAction::Continue,
                            palette,
                            context,
                        ))
                    })
                    .when(can_stop, |actions| {
                        actions.child(self.render_header_button(
                            "停止",
                            Icon::X,
                            LogAiAnalysisHeaderAction::Stop,
                            palette,
                            context,
                        ))
                    }),
            )
    }

    /// 渲染顶部状态徽标。
    fn render_status_badge(&self, palette: AppThemePalette) -> gpui::Div {
        let (label, color) = match self.state.status {
            LogAiAnalysisWindowStatus::Running => ("分析中", palette.accent),
            LogAiAnalysisWindowStatus::Paused => ("已暂停", palette.search_highlight),
            LogAiAnalysisWindowStatus::Complete => ("已完成", palette.accent),
            LogAiAnalysisWindowStatus::Stopped => ("已停止", palette.muted_text),
            LogAiAnalysisWindowStatus::Failed => ("失败", palette.error),
        };
        div()
            .px_2()
            .py_1()
            .rounded(px(999.0))
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(color))
            .bg(rgb(palette.surface))
            .child(label)
    }

    /// 渲染顶部操作按钮。
    fn render_header_button(
        &self,
        label: &'static str,
        icon: Icon,
        action: LogAiAnalysisHeaderAction,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "log-ai-analysis-action-{label}"
            )))
            .flex()
            .items_center()
            .gap_1()
            .px_3()
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .text_sm()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(MainView::render_lucide_icon(
                Some(icon),
                14.0,
                14.0,
                palette.text,
            ))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    match action {
                        LogAiAnalysisHeaderAction::Continue => view.continue_analysis(context),
                        LogAiAnalysisHeaderAction::Stop => view.stop_analysis(context),
                    }
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染分析内容。
    fn render_body(
        &self,
        palette: AppThemePalette,
        theme: EffectiveTheme,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("log-ai-analysis-body")
            .flex()
            .flex_col()
            .items_center()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .scrollbar_width(px(8.0))
            .track_scroll(&self.scroll_handle)
            .bg(rgb(palette.background))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w_full()
                    .max_w(px(LOG_AI_ANALYSIS_CONTENT_MAX_WIDTH))
                    .flex_none()
                    .px_3()
                    .py_4()
                    .child(self.render_overview_card(palette))
                    .when(self.has_multiple_chunks(), |body| {
                        body.child(self.render_progress_card(palette, context))
                    })
                    .when(
                        self.total_chunk_count() == 1 && !self.state.chunks.is_empty(),
                        |body| body.child(self.render_single_result_card(palette, theme, context)),
                    )
                    .when(self.has_multiple_chunks(), |body| {
                        body.child(self.render_active_chunk_card(palette, theme, context))
                    })
                    .when(
                        self.has_multiple_chunks()
                            && (!self.state.summary.response.is_empty()
                                || !self.state.summary.reasoning.is_empty()
                                || self.state.summary.status
                                    == LogAiAnalysisSectionStatus::Streaming),
                        |body| body.child(self.render_summary_card(palette, theme, context)),
                    )
                    .children(
                        self.state
                            .follow_ups
                            .iter()
                            .enumerate()
                            .map(|(index, follow_up)| {
                                self.render_follow_up_card(
                                    index, follow_up, palette, theme, context,
                                )
                            }),
                    )
                    .when(
                        self.state.status == LogAiAnalysisWindowStatus::Complete,
                        |body| body.child(self.render_follow_up_input(palette, context)),
                    ),
            )
    }

    /// 渲染整体概览。
    fn render_overview_card(&self, palette: AppThemePalette) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child("分析范围"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child(format!(
                                "{} 个来源 · {} / {} 批完成",
                                self.targets.len(),
                                self.state.completed_chunks,
                                self.state.chunks.len().max(self.state.total_chunks)
                            )),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .line_height(px(18.0))
                    .text_color(rgb(palette.muted_text))
                    .child(
                        self.targets
                            .iter()
                            .map(|target| target.display_name.clone())
                            .collect::<Vec<_>>()
                            .join("、"),
                    ),
            )
    }

    /// 渲染总进度和批次切换控制。
    fn render_progress_card(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let total = self.total_chunk_count().max(1);
        let completed = self.state.completed_chunks.min(total);
        let progress = log_ai_analysis_progress_fraction(
            self.state.status,
            self.state.completed_chunks,
            total,
        );
        let progress_label = log_ai_analysis_progress_label(
            self.state.status,
            self.state.summary.status,
            self.state.completed_chunks,
            total,
        );
        let active = clamp_log_ai_analysis_chunk_index(self.state.active_chunk_index, total);
        div()
            .id("log-ai-analysis-progress-card")
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child("总进度"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child(format!("{progress_label} · {completed} / {total} 批")),
                    ),
            )
            .child(
                div()
                    .h(px(8.0))
                    .w_full()
                    .rounded(px(999.0))
                    .bg(rgb(palette.panel))
                    .overflow_hidden()
                    .child(
                        div()
                            .h_full()
                            .w(relative(progress.clamp(0.0, 1.0)))
                            .bg(rgb(palette.accent)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(self.render_chunk_nav_button(
                        Icon::ChevronLeft,
                        active > 0,
                        -1,
                        palette,
                        context,
                    ))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(format!("第 {} / {} 批", active + 1, total)),
                    )
                    .child(self.render_chunk_nav_button(
                        Icon::ChevronRight,
                        active + 1 < total,
                        1,
                        palette,
                        context,
                    )),
            )
    }

    /// 渲染批次左右切换按钮。
    fn render_chunk_nav_button(
        &self,
        icon: Icon,
        enabled: bool,
        delta: isize,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "log-ai-analysis-chunk-nav-{delta}"
            )))
            .flex()
            .items_center()
            .justify_center()
            .size(px(30.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .text_color(rgb(if enabled {
                palette.text
            } else {
                palette.muted_text
            }))
            .bg(rgb(palette.panel))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
                    .on_click(context.listener(
                        move |view, _event: &ClickEvent, _window, context| {
                            view.switch_active_chunk(delta, context);
                            context.stop_propagation();
                        },
                    ))
            })
            .child(MainView::render_lucide_icon(
                Some(icon),
                15.0,
                15.0,
                if enabled {
                    palette.text
                } else {
                    palette.muted_text
                },
            ))
    }

    /// 单批次时直接渲染最终分析结果。
    fn render_single_result_card(
        &self,
        palette: AppThemePalette,
        theme: EffectiveTheme,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let chunk = &self.state.chunks[0];
        self.render_analysis_section_card(
            "分析结果",
            Some(chunk.range.label()),
            chunk,
            palette,
            theme,
            context,
            Some(chunk.evidence_snippets.clone()),
        )
    }

    /// 多批次时只渲染当前选中的批次。
    fn render_active_chunk_card(
        &self,
        palette: AppThemePalette,
        theme: EffectiveTheme,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let index = self
            .state
            .active_chunk_index
            .min(self.state.chunks.len().saturating_sub(1));
        if let Some(chunk) = self.state.chunks.get(index) {
            self.render_chunk_card(index, chunk, palette, theme, context)
        } else {
            div()
                .id("log-ai-analysis-active-chunk-empty")
                .p_4()
                .rounded(px(8.0))
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .text_sm()
                .text_color(rgb(palette.muted_text))
                .child("正在准备批次内容")
        }
    }

    /// 渲染最终汇总卡片。
    fn render_summary_card(
        &self,
        palette: AppThemePalette,
        theme: EffectiveTheme,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let evidence_snippets = self.visible_summary_evidence_snippets();
        let referenced_snippets = referenced_log_ai_analysis_evidence_snippets_for_text(
            &self.state.summary.response,
            &evidence_snippets,
        );
        self.render_analysis_section_card(
            "最终汇总",
            None,
            &self.state.summary,
            palette,
            theme,
            context,
            Some(evidence_snippets.clone()),
        )
        .when(
            !evidence_snippets.is_empty() && referenced_snippets.is_empty(),
            |card| {
                card.child(self.render_summary_evidence_panel(
                    "未引用证据片段",
                    evidence_snippets,
                    palette,
                    context,
                ))
            },
        )
    }

    /// 渲染单个分块卡片。
    fn render_chunk_card(
        &self,
        index: usize,
        chunk: &LogAiAnalysisChunkViewState,
        palette: AppThemePalette,
        theme: EffectiveTheme,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.render_analysis_section_card(
            &format!("第 {} 批", index + 1),
            Some(chunk.range.label()),
            chunk,
            palette,
            theme,
            context,
            None,
        )
    }

    /// 渲染批次或汇总卡片。
    fn render_analysis_section_card(
        &self,
        title: &str,
        subtitle: Option<String>,
        section: &impl LogAiAnalysisRenderableSection,
        palette: AppThemePalette,
        theme: EffectiveTheme,
        context: &mut Context<Self>,
        inline_evidence_snippets: Option<Vec<LogAiAnalysisEvidenceSnippet>>,
    ) -> gpui::Stateful<gpui::Div> {
        let status = section.section_status();
        let reasoning = section.reasoning_text();
        let response = section.response_text();
        let reasoning_expanded = section.reasoning_expanded();
        let section_key = section.section_key();
        div()
            .id(SharedString::from(format!(
                "log-ai-analysis-section-{section_key}"
            )))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.text))
                                    .child(title.to_string()),
                            )
                            .children(subtitle.map(|subtitle| {
                                div()
                                    .text_xs()
                                    .line_height(px(18.0))
                                    .text_color(rgb(palette.muted_text))
                                    .child(subtitle)
                            })),
                    )
                    .child(Self::render_section_status(status, palette)),
            )
            .when(!reasoning.is_empty(), |card| {
                card.child(self.render_reasoning_panel(
                    section_key.clone(),
                    reasoning,
                    reasoning_expanded,
                    palette,
                    context,
                ))
            })
            .child(
                div()
                    .id(SharedString::from(format!(
                        "log-ai-analysis-response-scroll-{section_key}"
                    )))
                    .min_h_0()
                    .max_h(px(320.0))
                    .overflow_y_scroll()
                    .scrollbar_width(px(6.0))
                    .child(self.render_response_panel(
                        response,
                        status,
                        palette,
                        theme,
                        inline_evidence_snippets,
                    )),
            )
    }

    /// 渲染分块状态。
    fn render_section_status(
        status: LogAiAnalysisSectionStatus,
        palette: AppThemePalette,
    ) -> gpui::Div {
        let (label, color) = match status {
            LogAiAnalysisSectionStatus::Pending => ("等待", palette.muted_text),
            LogAiAnalysisSectionStatus::Streaming => ("生成中", palette.accent),
            LogAiAnalysisSectionStatus::Complete => ("完成", palette.accent),
            LogAiAnalysisSectionStatus::Failed => ("失败", palette.error),
        };
        div()
            .flex_none()
            .px_2()
            .py_1()
            .rounded(px(999.0))
            .text_xs()
            .text_color(rgb(color))
            .bg(rgb(palette.panel))
            .child(label)
    }

    /// 渲染思考过程折叠面板。
    fn render_reasoning_panel(
        &self,
        section_key: String,
        reasoning: String,
        expanded: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let icon = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        let header_key = section_key.clone();
        let body_key = section_key.clone();
        let action_key = section_key.clone();
        div()
            .id(SharedString::from(format!(
                "log-ai-analysis-reasoning-{section_key}"
            )))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .child(
                div()
                    .id(SharedString::from(format!(
                        "log-ai-analysis-reasoning-header-{header_key}"
                    )))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .child(MainView::render_lucide_icon(
                        Some(icon),
                        14.0,
                        14.0,
                        palette.muted_text,
                    ))
                    .child(MainView::render_lucide_icon(
                        Some(Icon::Sparkles),
                        13.0,
                        13.0,
                        palette.muted_text,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.muted_text))
                            .child("思考过程"),
                    )
                    .on_click(context.listener(
                        move |view, _event: &ClickEvent, _window, context| {
                            if action_key == "summary" {
                                view.toggle_summary_reasoning(context);
                            } else if let Some(index) = action_key
                                .strip_prefix("chunk-")
                                .and_then(|raw| raw.parse::<usize>().ok())
                            {
                                view.toggle_chunk_reasoning(index, context);
                            } else if let Some(index) = action_key
                                .strip_prefix("follow-up-")
                                .and_then(|raw| raw.parse::<usize>().ok())
                            {
                                view.toggle_follow_up_reasoning(index, context);
                            }
                            context.stop_propagation();
                        },
                    )),
            )
            .when(expanded, |panel| {
                panel.child(
                    div()
                        .id(SharedString::from(format!(
                            "log-ai-analysis-reasoning-body-{body_key}"
                        )))
                        .max_h(px(220.0))
                        .overflow_y_scroll()
                        .scrollbar_width(px(6.0))
                        .px_3()
                        .pb_3()
                        .pt_1()
                        .border_t_1()
                        .border_color(rgb(palette.border))
                        .text_xs()
                        .line_height(px(18.0))
                        .whitespace_normal()
                        .text_color(rgb(palette.muted_text))
                        .child(reasoning),
                )
            })
    }

    /// 渲染最终汇总对应的原日志片段折叠面板。
    fn render_summary_evidence_panel(
        &self,
        title: &'static str,
        evidence_snippets: Vec<LogAiAnalysisEvidenceSnippet>,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let expanded = self.state.summary.evidence_expanded;
        let icon = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        div()
            .id("log-ai-analysis-summary-evidence")
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .child(
                div()
                    .id("log-ai-analysis-summary-evidence-header")
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .child(MainView::render_lucide_icon(
                        Some(icon),
                        14.0,
                        14.0,
                        palette.muted_text,
                    ))
                    .child(MainView::render_lucide_icon(
                        Some(Icon::FileText),
                        13.0,
                        13.0,
                        palette.muted_text,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.muted_text))
                            .child(format!("{title} · {} 条", evidence_snippets.len())),
                    )
                    .on_click(context.listener(
                        move |view, _event: &ClickEvent, _window, context| {
                            view.toggle_summary_evidence(context);
                            context.stop_propagation();
                        },
                    )),
            )
            .when(expanded, |panel| {
                panel.child(
                    div()
                        .id("log-ai-analysis-summary-evidence-list")
                        .flex()
                        .flex_col()
                        .gap_2()
                        .max_h(px(360.0))
                        .overflow_y_scroll()
                        .scrollbar_width(px(6.0))
                        .px_3()
                        .pb_3()
                        .pt_1()
                        .border_t_1()
                        .border_color(rgb(palette.border))
                        .children(
                            evidence_snippets
                                .iter()
                                .enumerate()
                                .map(|(index, snippet)| {
                                    self.render_evidence_snippet_card(index, snippet, palette)
                                }),
                        ),
                )
            })
    }

    /// 渲染单条原日志证据片段。
    fn render_evidence_snippet_card(
        &self,
        index: usize,
        snippet: &LogAiAnalysisEvidenceSnippet,
        palette: AppThemePalette,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "log-ai-analysis-evidence-snippet-{index}"
            )))
            .flex()
            .flex_col()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.input))
            .overflow_hidden()
            .child(
                div()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(rgb(palette.border))
                    .text_xs()
                    .line_height(px(18.0))
                    .text_color(rgb(palette.muted_text))
                    .child(snippet.label()),
            )
            .child(
                div()
                    .id(SharedString::from(format!(
                        "log-ai-analysis-evidence-snippet-body-{index}"
                    )))
                    .w_full()
                    .min_w_0()
                    .max_h(px(180.0))
                    .overflow_y_scroll()
                    .scrollbar_width(px(6.0))
                    .px_3()
                    .py_2()
                    .font_family(LOG_VIEWER_FONT_FAMILY)
                    .text_size(px(12.0))
                    .line_height(px(18.0))
                    .text_color(rgb(palette.text))
                    .children(snippet.text.lines().map(|line| {
                        div().whitespace_normal().child(if line.is_empty() {
                            " ".to_string()
                        } else {
                            line.to_string()
                        })
                    })),
            )
    }

    /// 渲染单条追问问答。
    fn render_follow_up_card(
        &self,
        index: usize,
        follow_up: &LogAiAnalysisFollowUpViewState,
        palette: AppThemePalette,
        theme: EffectiveTheme,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "log-ai-analysis-follow-up-card-{index}"
            )))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(format!("追问 {}", index + 1)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .line_height(px(20.0))
                            .text_color(rgb(palette.muted_text))
                            .child(follow_up.question.clone()),
                    ),
            )
            .when(!follow_up.reasoning.is_empty(), |card| {
                card.child(self.render_reasoning_panel(
                    follow_up.section_key(),
                    follow_up.reasoning.clone(),
                    follow_up.reasoning_expanded,
                    palette,
                    context,
                ))
            })
            .child(
                div()
                    .id(SharedString::from(format!(
                        "log-ai-analysis-follow-up-response-scroll-{index}"
                    )))
                    .min_h_0()
                    .max_h(px(300.0))
                    .overflow_y_scroll()
                    .scrollbar_width(px(6.0))
                    .child(self.render_response_panel(
                        follow_up.response_text(),
                        follow_up.status,
                        palette,
                        theme,
                        None,
                    )),
            )
    }

    /// 渲染分析完成后的追问输入区。
    fn render_follow_up_input(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let can_send = self.can_send_follow_up();
        let is_streaming = self.is_follow_up_streaming();
        div()
            .id("log-ai-analysis-follow-up-input-card")
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child("继续追问"),
            )
            .child(
                div()
                    .id("log-ai-analysis-follow-up-input")
                    .min_h(px(82.0))
                    .max_h(px(180.0))
                    .overflow_y_scroll()
                    .scrollbar_width(px(6.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.input))
                    .px_3()
                    .py_2()
                    .text_sm()
                    .line_height(px(20.0))
                    .text_color(rgb(palette.text))
                    .track_focus(&self.follow_up_input_focus)
                    .key_context("log-ai-analysis-follow-up-input")
                    .on_key_down(context.listener(
                        |view, event: &KeyDownEvent, _window, context| {
                            view.handle_follow_up_input_key_down(event, context);
                        },
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, event: &MouseDownEvent, window, context| {
                            view.start_follow_up_input_mouse_selection(event, context);
                            window.focus(&view.follow_up_input_focus);
                            context.stop_propagation();
                        }),
                    )
                    .on_mouse_move(context.listener(
                        |view, event: &MouseMoveEvent, _window, context| {
                            view.update_follow_up_input_mouse_selection(event.position, context);
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.finish_follow_up_input_mouse_selection(context);
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.finish_follow_up_input_mouse_selection(context);
                        }),
                    )
                    .child(LogAiAnalysisFollowUpInputElement {
                        view: context.entity(),
                        focus_handle: self.follow_up_input_focus.clone(),
                        placeholder: "针对结论继续追问，Enter 发送，Shift+Enter 换行",
                        palette,
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(div().text_xs().text_color(rgb(palette.muted_text)).child(
                        if is_streaming {
                            "正在生成追问回复..."
                        } else {
                            "追问只基于本次结论和已展示证据片段"
                        },
                    ))
                    .child(
                        div()
                            .id("log-ai-analysis-follow-up-send")
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_3()
                            .py_1()
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(rgb(palette.border))
                            .bg(rgb(if can_send {
                                palette.accent
                            } else {
                                palette.panel
                            }))
                            .text_sm()
                            .text_color(rgb(if can_send {
                                palette.on_accent
                            } else {
                                palette.muted_text
                            }))
                            .when(can_send, |button| {
                                button.cursor_pointer().on_click(context.listener(
                                    |view, _event: &ClickEvent, _window, context| {
                                        view.send_follow_up(context);
                                        context.stop_propagation();
                                    },
                                ))
                            })
                            .child(MainView::render_lucide_icon(
                                Some(Icon::Send),
                                14.0,
                                14.0,
                                if can_send {
                                    palette.on_accent
                                } else {
                                    palette.muted_text
                                },
                            ))
                            .child("发送"),
                    ),
            )
    }

    /// 渲染正式分析结果。
    fn render_response_panel(
        &self,
        response: String,
        status: LogAiAnalysisSectionStatus,
        palette: AppThemePalette,
        theme: EffectiveTheme,
        inline_evidence_snippets: Option<Vec<LogAiAnalysisEvidenceSnippet>>,
    ) -> gpui::AnyElement {
        if response.trim().is_empty() {
            let placeholder = if status == LogAiAnalysisSectionStatus::Streaming {
                "思考中..."
            } else if status == LogAiAnalysisSectionStatus::Failed {
                "生成失败"
            } else {
                "等待分析"
            };
            return div()
                .text_sm()
                .line_height(px(20.0))
                .text_color(rgb(palette.muted_text))
                .child(placeholder)
                .into_any_element();
        }

        let document = parse_ai_chat_markdown(&response, theme);
        let response_key = format!(
            "log-ai-analysis-response-{}",
            ai_chat_markdown_content_hash(&response)
        );
        if let Some(snippets) = inline_evidence_snippets {
            let mut inserted_ids = std::collections::HashSet::new();
            render_app_markdown_document_with_block_inserts(
                &document,
                &response_key,
                palette,
                |index, block| {
                    let block_text = log_ai_analysis_markdown_block_text(block);
                    referenced_log_ai_analysis_evidence_snippets_for_text(&block_text, &snippets)
                        .into_iter()
                        .filter(|snippet| inserted_ids.insert(snippet.id.clone()))
                        .enumerate()
                        .map(|(snippet_index, snippet)| {
                            self.render_evidence_snippet_card(
                                index.saturating_mul(1000).saturating_add(snippet_index),
                                &snippet,
                                palette,
                            )
                            .into_any_element()
                        })
                        .collect::<Vec<_>>()
                },
            )
            .into_any_element()
        } else {
            render_app_markdown_document(&document, &response_key, palette).into_any_element()
        }
    }
}

/// 提取 Markdown 块的纯文本，用于判断该块是否引用证据 ID。
fn log_ai_analysis_markdown_block_text(block: &AppMarkdownBlock) -> String {
    match block {
        AppMarkdownBlock::Paragraph(inlines) | AppMarkdownBlock::Heading { inlines, .. } => {
            log_ai_analysis_markdown_inlines_text(inlines)
        }
        AppMarkdownBlock::List { items, .. } => items
            .iter()
            .flat_map(|item| item.iter().map(log_ai_analysis_markdown_block_text))
            .collect::<Vec<_>>()
            .join("\n"),
        AppMarkdownBlock::BlockQuote(children) => children
            .iter()
            .map(log_ai_analysis_markdown_block_text)
            .collect::<Vec<_>>()
            .join("\n"),
        AppMarkdownBlock::CodeBlock { lines, .. } => lines
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>()
            .join("\n"),
        AppMarkdownBlock::Table { headers, rows } => headers
            .iter()
            .map(|cell| log_ai_analysis_markdown_inlines_text(cell))
            .chain(rows.iter().flat_map(|row| {
                row.iter()
                    .map(|cell| log_ai_analysis_markdown_inlines_text(cell))
            }))
            .collect::<Vec<_>>()
            .join("\n"),
        AppMarkdownBlock::ThematicBreak => String::new(),
    }
}

/// 提取 Markdown 行内节点纯文本。
fn log_ai_analysis_markdown_inlines_text(inlines: &[AppMarkdownInline]) -> String {
    let mut text = String::new();
    for inline in inlines {
        match inline {
            AppMarkdownInline::Text(value) | AppMarkdownInline::Code(value) => {
                text.push_str(value);
            }
            AppMarkdownInline::Strong(children)
            | AppMarkdownInline::Emphasis(children)
            | AppMarkdownInline::Strikethrough(children) => {
                text.push_str(&log_ai_analysis_markdown_inlines_text(children));
            }
            AppMarkdownInline::Link { label, .. } => {
                text.push_str(&log_ai_analysis_markdown_inlines_text(label));
            }
        }
    }
    text
}

/// 日志智能分析追问自绘输入元素。
///
/// 业务意图：
/// - 独立智能分析窗口不是 `MainView`，不能直接复用 AI 对话输入控件的状态；这里保留同等输入能力，
///   包括光标、选区、复制粘贴、撤回/重做和平台 IME。
struct LogAiAnalysisFollowUpInputElement {
    /// 智能分析窗口实体，用于读取和写回输入状态。
    view: Entity<LogAiAnalysisWindowView>,
    /// 输入框焦点句柄。
    focus_handle: gpui::FocusHandle,
    /// 输入为空时显示的占位文案。
    placeholder: &'static str,
    /// 当前主题调色板。
    palette: AppThemePalette,
}

impl IntoElement for LogAiAnalysisFollowUpInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for LogAiAnalysisFollowUpInputElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<LogAiAnalysisFollowUpInputPrepaint>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        context: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let line_count = self.view.read(context).follow_up_input_visual_line_count();
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = px(
            (line_count as f32 * LOG_AI_ANALYSIS_FOLLOW_UP_INPUT_LINE_HEIGHT)
                .max(LOG_AI_ANALYSIS_FOLLOW_UP_INPUT_LINE_HEIGHT),
        )
        .into();
        (window.request_layout(style, [], context), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        context: &mut App,
    ) -> Self::PrepaintState {
        let (text, selection_range, marked_range) =
            self.view.read(context).follow_up_input_text_snapshot();
        let focused = self.focus_handle.is_focused(window);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = px(LOG_AI_ANALYSIS_FOLLOW_UP_INPUT_LINE_HEIGHT);
        let line_ranges = MainView::thread_analysis_filter_line_ranges(&text);
        let display_ranges = if text.is_empty() {
            std::iter::once(0..0).collect::<Vec<_>>()
        } else {
            line_ranges
        };

        let mut lines = Vec::new();
        let mut selections = Vec::new();
        let mut cursor = None;
        let selection_range = MainView::clamp_search_text_range(&text, selection_range);
        let has_selection = focused && selection_range.start < selection_range.end;
        let cursor_index = selection_range.end;

        for (line_index, byte_range) in display_ranges.into_iter().enumerate() {
            let is_placeholder = text.is_empty();
            let display_text = if is_placeholder {
                SharedString::from(self.placeholder)
            } else {
                SharedString::from(text[byte_range.clone()].to_string())
            };
            let text_color = if is_placeholder {
                rgb(self.palette.muted_text).into()
            } else {
                style.color
            };
            let base_run = TextRun {
                len: display_text.len(),
                font: style.font(),
                color: text_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let runs = if !is_placeholder {
                if let Some(marked_range) = marked_range.clone() {
                    let local_marked_start = marked_range
                        .start
                        .saturating_sub(byte_range.start)
                        .min(byte_range.len());
                    let local_marked_end = marked_range
                        .end
                        .saturating_sub(byte_range.start)
                        .min(byte_range.len());
                    vec![
                        TextRun {
                            len: local_marked_start,
                            ..base_run.clone()
                        },
                        TextRun {
                            len: local_marked_end.saturating_sub(local_marked_start),
                            underline: Some(UnderlineStyle {
                                color: Some(base_run.color),
                                thickness: px(1.0),
                                wavy: false,
                            }),
                            ..base_run.clone()
                        },
                        TextRun {
                            len: display_text.len().saturating_sub(local_marked_end),
                            ..base_run
                        },
                    ]
                    .into_iter()
                    .filter(|run| run.len > 0)
                    .collect()
                } else {
                    vec![base_run]
                }
            } else {
                vec![base_run]
            };
            let line = window
                .text_system()
                .shape_line(display_text, font_size, &runs, None);
            let line_top =
                bounds.top() + px(line_index as f32 * LOG_AI_ANALYSIS_FOLLOW_UP_INPUT_LINE_HEIGHT);
            let line_bounds = Bounds::new(
                point(bounds.left(), line_top),
                size(bounds.right() - bounds.left(), line_height),
            );

            if has_selection && !is_placeholder {
                let start = selection_range
                    .start
                    .max(byte_range.start)
                    .min(byte_range.end);
                let end = selection_range
                    .end
                    .max(byte_range.start)
                    .min(byte_range.end);
                if start < end {
                    let mut selection_color = rgb(self.palette.accent);
                    selection_color.a = 0.32;
                    selections.push(fill(
                        Bounds::from_corners(
                            point(
                                line_bounds.left() + line.x_for_index(start - byte_range.start),
                                line_bounds.top(),
                            ),
                            point(
                                line_bounds.left() + line.x_for_index(end - byte_range.start),
                                line_bounds.bottom(),
                            ),
                        ),
                        selection_color,
                    ));
                }
            }

            if focused
                && !has_selection
                && cursor.is_none()
                && cursor_index >= byte_range.start
                && cursor_index <= byte_range.end
            {
                cursor = Some(fill(
                    Bounds::new(
                        point(
                            line_bounds.left() + line.x_for_index(cursor_index - byte_range.start),
                            line_bounds.top(),
                        ),
                        size(px(1.5), line_bounds.bottom() - line_bounds.top()),
                    ),
                    rgb(self.palette.accent),
                ));
            }

            lines.push(LogAiAnalysisFollowUpInputPaintLine {
                byte_range,
                bounds: line_bounds,
                line,
            });
        }

        Some(LogAiAnalysisFollowUpInputPrepaint {
            lines,
            selections,
            cursor,
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        context: &mut App,
    ) {
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            context,
        );
        let Some(prepaint) = prepaint.take() else {
            return;
        };
        for selection in prepaint.selections {
            window.paint_quad(selection);
        }

        let mut layouts = Vec::new();
        for paint_line in prepaint.lines {
            paint_line
                .line
                .paint(
                    paint_line.bounds.origin,
                    paint_line.bounds.bottom() - paint_line.bounds.top(),
                    window,
                    context,
                )
                .ok();
            layouts.push(LogAiAnalysisFollowUpInputLineLayout {
                byte_range: paint_line.byte_range,
                line: paint_line.line,
                bounds: paint_line.bounds,
            });
        }
        if let Some(cursor) = prepaint.cursor {
            window.paint_quad(cursor);
        }
        if self.focus_handle.is_focused(window) {
            window.request_animation_frame();
        }
        self.view.update(context, |view, _context| {
            view.store_follow_up_input_text_layouts(layouts, bounds);
        });
    }
}

impl EntityInputHandler for LogAiAnalysisWindowView {
    /// 返回指定 UTF-16 范围内的追问输入框文本。
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<String> {
        if !self.follow_up_input_focus.is_focused(window) {
            return None;
        }
        let range =
            MainView::search_input_range_from_utf16(&self.state.follow_up_input, range_utf16);
        adjusted_range.replace(MainView::search_input_range_to_utf16(
            &self.state.follow_up_input,
            range.clone(),
        ));
        Some(self.state.follow_up_input[range].to_string())
    }

    /// 返回追问输入框当前选择范围。
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if !self.follow_up_input_focus.is_focused(window) {
            return None;
        }
        Some(UTF16Selection {
            range: MainView::search_input_range_to_utf16(
                &self.state.follow_up_input,
                self.state.follow_up_input_selection_range.clone(),
            ),
            reversed: false,
        })
    }

    /// 返回输入法当前组合文本范围。
    fn marked_text_range(
        &self,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        if !self.follow_up_input_focus.is_focused(window) {
            return None;
        }
        self.state
            .follow_up_input_marked_range
            .clone()
            .map(|range| MainView::search_input_range_to_utf16(&self.state.follow_up_input, range))
    }

    /// 清除输入法组合文本状态。
    fn unmark_text(&mut self, window: &mut Window, context: &mut Context<Self>) {
        if !self.follow_up_input_focus.is_focused(window) {
            return;
        }
        self.state.follow_up_input_marked_range = None;
        context.notify();
    }

    /// 用平台提交文本替换追问输入框中的指定范围。
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if !self.follow_up_input_focus.is_focused(window) {
            return;
        }
        let replacing_marked_text = self.state.follow_up_input_marked_range.is_some();
        if !replacing_marked_text {
            self.push_follow_up_input_undo_snapshot();
        }
        let replacement = text.replace("\r\n", "\n").replace('\r', "\n");
        let range = range_utf16
            .map(|range| {
                MainView::search_input_range_from_utf16(&self.state.follow_up_input, range)
            })
            .or_else(|| self.state.follow_up_input_marked_range.clone())
            .unwrap_or_else(|| self.state.follow_up_input_selection_range.clone());
        let range = MainView::clamp_search_text_range(&self.state.follow_up_input, range);
        self.state
            .follow_up_input
            .replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        self.state.follow_up_input_selection_range = cursor..cursor;
        self.state.follow_up_input_marked_range = None;
        context.notify();
    }

    /// 用平台组合文本替换追问输入框中的指定范围，并保留组合状态。
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if !self.follow_up_input_focus.is_focused(window) {
            return;
        }
        if self.state.follow_up_input_marked_range.is_none() {
            self.push_follow_up_input_undo_snapshot();
        }
        let replacement = new_text.replace("\r\n", "\n").replace('\r', "\n");
        let range = range_utf16
            .map(|range| {
                MainView::search_input_range_from_utf16(&self.state.follow_up_input, range)
            })
            .or_else(|| self.state.follow_up_input_marked_range.clone())
            .unwrap_or_else(|| self.state.follow_up_input_selection_range.clone());
        let range = MainView::clamp_search_text_range(&self.state.follow_up_input, range);
        self.state
            .follow_up_input
            .replace_range(range.clone(), &replacement);

        if replacement.is_empty() {
            self.state.follow_up_input_marked_range = None;
        } else {
            self.state.follow_up_input_marked_range =
                Some(range.start..range.start + replacement.len());
        }

        let selected_range = new_selected_range_utf16
            .map(|utf16_range| MainView::search_input_range_from_utf16(&replacement, utf16_range))
            .map(|relative_range| {
                range.start + relative_range.start..range.start + relative_range.end
            })
            .unwrap_or_else(|| {
                let cursor = range.start + replacement.len();
                cursor..cursor
            });
        self.state.follow_up_input_selection_range = selected_range;
        context.notify();
    }

    /// 返回指定文本范围在屏幕上的边界，用于放置 IME 候选窗口。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if !self.follow_up_input_focus.is_focused(window) {
            return None;
        }
        let range =
            MainView::search_input_range_from_utf16(&self.state.follow_up_input, range_utf16);
        let cursor = range.start;
        for layout in &self.state.follow_up_input_last_layouts {
            if cursor >= layout.byte_range.start && cursor <= layout.byte_range.end {
                let x = layout
                    .line
                    .x_for_index(cursor.saturating_sub(layout.byte_range.start));
                return Some(Bounds::new(
                    point(layout.bounds.left() + x, layout.bounds.top()),
                    size(px(1.0), layout.bounds.bottom() - layout.bounds.top()),
                ));
            }
        }
        Some(element_bounds)
    }

    /// 根据鼠标位置返回文本插入点。
    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<usize> {
        if !self.follow_up_input_focus.is_focused(window) {
            return None;
        }
        let utf8_index = self.follow_up_input_index_for_point(point);
        Some(MainView::search_input_utf16_offset_from_byte(
            &self.state.follow_up_input,
            utf8_index,
        ))
    }
}

impl Render for LogAiAnalysisWindowView {
    /// 渲染日志智能分析窗口。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let (palette, theme) = {
            let main_view = self.main_view.read(context);
            (main_view.palette(), main_view.effective_theme())
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(palette.background))
            .child(self.render_header(palette, context))
            .child(self.render_body(palette, theme, context))
    }
}

/// 日志智能分析窗口整体状态。
struct LogAiAnalysisWindowState {
    /// 当前整体状态。
    status: LogAiAnalysisWindowStatus,
    /// 顶部状态文案。
    message: String,
    /// 已知全部批次数。
    total_chunks: usize,
    /// 已完成批次数。
    completed_chunks: usize,
    /// 当前正在处理的批次。
    current_chunk_index: Option<usize>,
    /// 下一轮继续时的起始批次。
    next_chunk_index: usize,
    /// 当前展示的批次序号。
    active_chunk_index: usize,
    /// 用户是否手动切换过当前展示批次。
    user_selected_chunk: bool,
    /// 各批次展示状态。
    chunks: Vec<LogAiAnalysisChunkViewState>,
    /// 最终汇总展示状态。
    summary: LogAiAnalysisSummaryViewState,
    /// 完成后的追问输入文本。
    follow_up_input: String,
    /// 追问输入框选择范围。
    follow_up_input_selection_range: Range<usize>,
    /// 追问输入框输入法组合文本范围。
    follow_up_input_marked_range: Option<Range<usize>>,
    /// 追问输入框最近一次绘制的逐行布局。
    follow_up_input_last_layouts: Vec<LogAiAnalysisFollowUpInputLineLayout>,
    /// 追问输入框最近一次整体绘制边界。
    follow_up_input_last_bounds: Option<Bounds<Pixels>>,
    /// 追问输入框拖拽选择锚点。
    follow_up_input_selection_drag: Option<usize>,
    /// 追问输入框撤回栈。
    follow_up_input_undo_stack: Vec<LogAiAnalysisFollowUpInputSnapshot>,
    /// 追问输入框重做栈。
    follow_up_input_redo_stack: Vec<LogAiAnalysisFollowUpInputSnapshot>,
    /// 当前窗口内的追问问答历史。
    follow_ups: Vec<LogAiAnalysisFollowUpViewState>,
}

impl LogAiAnalysisWindowState {
    /// 创建初始窗口状态。
    fn new() -> Self {
        Self {
            status: LogAiAnalysisWindowStatus::Running,
            message: "正在准备日志内容...".to_string(),
            total_chunks: 0,
            completed_chunks: 0,
            current_chunk_index: None,
            next_chunk_index: 0,
            active_chunk_index: 0,
            user_selected_chunk: false,
            chunks: Vec::new(),
            summary: LogAiAnalysisSummaryViewState::new(),
            follow_up_input: String::new(),
            follow_up_input_selection_range: 0..0,
            follow_up_input_marked_range: None,
            follow_up_input_last_layouts: Vec::new(),
            follow_up_input_last_bounds: None,
            follow_up_input_selection_drag: None,
            follow_up_input_undo_stack: Vec::new(),
            follow_up_input_redo_stack: Vec::new(),
            follow_ups: Vec::new(),
        }
    }
}

/// 日志智能分析整体状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogAiAnalysisWindowStatus {
    /// 正在准备、发送或等待模型输出。
    Running,
    /// 达到批次数上限，等待用户继续。
    Paused,
    /// 全部完成。
    Complete,
    /// 用户停止。
    Stopped,
    /// 发生错误。
    Failed,
}

/// 日志智能分析顶部操作。
#[derive(Clone, Copy)]
enum LogAiAnalysisHeaderAction {
    /// 继续下一轮分块分析。
    Continue,
    /// 停止当前分析任务。
    Stop,
}

/// 单个分块或汇总的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogAiAnalysisSectionStatus {
    /// 尚未开始。
    Pending,
    /// 正在流式输出。
    Streaming,
    /// 已完成。
    Complete,
    /// 当前段落生成失败。
    Failed,
}

/// 单个分块展示状态。
struct LogAiAnalysisChunkViewState {
    /// 当前分块在全部分块中的 0 基序号。
    index: usize,
    /// 分块范围。
    range: LogAiAnalysisChunkRange,
    /// 分块生命周期。
    status: LogAiAnalysisSectionStatus,
    /// 显式推理内容。
    reasoning: String,
    /// 正式回复内容。
    response: String,
    /// 该批次本地截取出的原始日志证据片段。
    evidence_snippets: Vec<LogAiAnalysisEvidenceSnippet>,
    /// 思考面板是否展开。
    reasoning_expanded: bool,
}

impl LogAiAnalysisChunkViewState {
    /// 创建分块展示状态。
    fn new(index: usize, range: LogAiAnalysisChunkRange) -> Self {
        Self {
            index,
            range,
            status: LogAiAnalysisSectionStatus::Pending,
            reasoning: String::new(),
            response: String::new(),
            evidence_snippets: Vec::new(),
            reasoning_expanded: false,
        }
    }
}

/// 最终汇总展示状态。
struct LogAiAnalysisSummaryViewState {
    /// 汇总生命周期。
    status: LogAiAnalysisSectionStatus,
    /// 汇总显式推理内容。
    reasoning: String,
    /// 汇总正式回复内容。
    response: String,
    /// 思考面板是否展开。
    reasoning_expanded: bool,
    /// 原日志片段面板是否展开。
    evidence_expanded: bool,
}

impl LogAiAnalysisSummaryViewState {
    /// 创建空汇总状态。
    fn new() -> Self {
        Self {
            status: LogAiAnalysisSectionStatus::Pending,
            reasoning: String::new(),
            response: String::new(),
            reasoning_expanded: false,
            evidence_expanded: false,
        }
    }
}

/// 追问问答展示状态。
struct LogAiAnalysisFollowUpViewState {
    /// 当前追问在窗口会话中的 0 基序号。
    index: usize,
    /// 用户追问内容。
    question: String,
    /// 追问生命周期。
    status: LogAiAnalysisSectionStatus,
    /// 显式推理内容。
    reasoning: String,
    /// 正式回复内容。
    response: String,
    /// 失败时的中文错误文案。
    error_message: Option<String>,
    /// 思考面板是否展开。
    reasoning_expanded: bool,
}

impl LogAiAnalysisFollowUpViewState {
    /// 创建等待流式回复的追问状态。
    fn new(index: usize, question: String) -> Self {
        Self {
            index,
            question,
            status: LogAiAnalysisSectionStatus::Streaming,
            reasoning: String::new(),
            response: String::new(),
            error_message: None,
            reasoning_expanded: true,
        }
    }
}

/// 分块和最终汇总共用的渲染接口。
trait LogAiAnalysisRenderableSection {
    /// 返回渲染 key。
    fn section_key(&self) -> String;
    /// 返回状态。
    fn section_status(&self) -> LogAiAnalysisSectionStatus;
    /// 返回推理内容。
    fn reasoning_text(&self) -> String;
    /// 返回正式回复。
    fn response_text(&self) -> String;
    /// 返回思考面板是否展开。
    fn reasoning_expanded(&self) -> bool;
}

impl LogAiAnalysisRenderableSection for LogAiAnalysisChunkViewState {
    fn section_key(&self) -> String {
        format!("chunk-{}", self.index)
    }

    fn section_status(&self) -> LogAiAnalysisSectionStatus {
        self.status
    }

    fn reasoning_text(&self) -> String {
        self.reasoning.clone()
    }

    fn response_text(&self) -> String {
        self.response.clone()
    }

    fn reasoning_expanded(&self) -> bool {
        self.reasoning_expanded
    }
}

impl LogAiAnalysisRenderableSection for LogAiAnalysisSummaryViewState {
    fn section_key(&self) -> String {
        "summary".to_string()
    }

    fn section_status(&self) -> LogAiAnalysisSectionStatus {
        self.status
    }

    fn reasoning_text(&self) -> String {
        self.reasoning.clone()
    }

    fn response_text(&self) -> String {
        self.response.clone()
    }

    fn reasoning_expanded(&self) -> bool {
        self.reasoning_expanded
    }
}

impl LogAiAnalysisRenderableSection for LogAiAnalysisFollowUpViewState {
    fn section_key(&self) -> String {
        format!("follow-up-{}", self.index)
    }

    fn section_status(&self) -> LogAiAnalysisSectionStatus {
        self.status
    }

    fn reasoning_text(&self) -> String {
        self.reasoning.clone()
    }

    fn response_text(&self) -> String {
        self.error_message
            .clone()
            .unwrap_or_else(|| self.response.clone())
    }

    fn reasoning_expanded(&self) -> bool {
        self.reasoning_expanded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证单批次场景不展示总进度和批次导航，多批次才展示。
    #[test]
    fn 智能分析单批不展示进度条() {
        assert!(!should_show_log_ai_analysis_progress(0));
        assert!(!should_show_log_ai_analysis_progress(1));
        assert!(should_show_log_ai_analysis_progress(2));
    }

    /// 验证总进度计算覆盖运行中、完成和空批次边界。
    #[test]
    fn 智能分析进度比例按批次完成度计算() {
        assert_eq!(
            log_ai_analysis_progress_fraction(LogAiAnalysisWindowStatus::Running, 0, 0),
            0.0
        );
        assert_eq!(
            log_ai_analysis_progress_fraction(LogAiAnalysisWindowStatus::Running, 2, 4),
            0.5
        );
        assert_eq!(
            log_ai_analysis_progress_fraction(LogAiAnalysisWindowStatus::Running, 8, 4),
            1.0
        );
        assert_eq!(
            log_ai_analysis_progress_fraction(LogAiAnalysisWindowStatus::Complete, 3, 4),
            1.0
        );
    }

    /// 验证批次完成但最终汇总生成中时，进度文案不会误写成普通批次分析。
    #[test]
    fn 智能分析进度文案区分最终汇总阶段() {
        assert_eq!(
            log_ai_analysis_progress_label(
                LogAiAnalysisWindowStatus::Running,
                LogAiAnalysisSectionStatus::Streaming,
                4,
                4
            ),
            "正在生成最终汇总"
        );
        assert_eq!(
            log_ai_analysis_progress_label(
                LogAiAnalysisWindowStatus::Complete,
                LogAiAnalysisSectionStatus::Complete,
                4,
                4
            ),
            "已完成"
        );
    }

    /// 验证批次导航会把越界序号限制在可渲染范围内。
    #[test]
    fn 智能分析批次导航会限制越界索引() {
        assert_eq!(clamp_log_ai_analysis_chunk_index(5, 0), 0);
        assert_eq!(clamp_log_ai_analysis_chunk_index(0, 3), 0);
        assert_eq!(clamp_log_ai_analysis_chunk_index(9, 3), 2);
    }
}
