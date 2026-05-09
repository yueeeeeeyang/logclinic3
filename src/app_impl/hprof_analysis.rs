// HPROF dump 分析独立窗口。
//
// 业务意图：
// - HPROF dominator tree 是独立于日志正文的重型分析视图，应在单独窗口中展示进度和结果，避免遮挡主日志工作区。
// - 解析任务运行在后台线程，窗口只消费进度快照和最终结果，确保 GPUI 主线程不会被大 dump 解析阻塞。
//
// 边界条件：
// - 用户关闭窗口或点击取消时，只能尽快停止解析循环；第三方 dominator 算法阶段无法细粒度中断，因此在进入前后检查取消状态。
// - GPUI 文件选择器不能限制扩展名，后缀和 header 校验失败会在本窗口展示中文错误。

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use super::*;
use crate::hprof::{
    HprofDominatorResult, HprofDominatorRow, HprofError, HprofObjectId, HprofProgress,
    HprofThreadDetails, analyze_hprof_dominator_tree, format_hprof_bytes,
};

/// HPROF 分析窗口进度轮询间隔。
///
/// 业务意图：
/// - 120ms 足够让大文件解析进度看起来连续，又不会因为 UI 高频重绘拖慢后台解析。
const HPROF_PROGRESS_POLL_INTERVAL: Duration = Duration::from_millis(120);

/// HPROF dominator 行高。
///
/// 边界条件：
/// - 行高固定才能交给 `uniform_list` 虚拟渲染；长类名统一截断，不做多行展示。
const HPROF_DOMINATOR_ROW_HEIGHT: f32 = 32.0;

/// HPROF dominator 名称列最小宽度。
///
/// 业务意图：
/// - 名称列需要吃掉固定数值列之外的剩余空间，避免右侧出现空白；最小宽度用于窗口过窄时保持类名仍有基本可读空间。
const HPROF_DOMINATOR_NAME_MIN_WIDTH: f32 = 420.0;
/// HPROF dominator shallow size 列宽。
const HPROF_DOMINATOR_SHALLOW_WIDTH: f32 = 104.0;
/// HPROF dominator retained size 列宽。
const HPROF_DOMINATOR_RETAINED_WIDTH: f32 = 116.0;
/// HPROF dominator 百分比列宽。
const HPROF_DOMINATOR_PERCENT_WIDTH: f32 = 74.0;
/// HPROF dominator 树缩进。
const HPROF_DOMINATOR_TREE_INDENT: f32 = 14.0;
/// HPROF 线程右键菜单宽度。
const HPROF_THREAD_CONTEXT_MENU_WIDTH: f32 = 132.0;
/// HPROF 线程右键菜单项高度。
const HPROF_THREAD_CONTEXT_MENU_ITEM_HEIGHT: f32 = 32.0;
/// HPROF 线程详情独立窗口宽度。
const HPROF_THREAD_DETAILS_WINDOW_WIDTH: f32 = 920.0;
/// HPROF 线程详情独立窗口高度。
const HPROF_THREAD_DETAILS_WINDOW_HEIGHT: f32 = 620.0;
/// HPROF 线程详情属性名列宽。
const HPROF_THREAD_PROPERTY_NAME_WIDTH: f32 = 190.0;
/// HPROF 线程详情属性行高度。
const HPROF_THREAD_PROPERTY_ROW_HEIGHT: f32 = 28.0;
/// HPROF 线程详情栈行高度。
const HPROF_THREAD_STACK_ROW_HEIGHT: f32 = 22.0;

/// HPROF dominator tree 中线程行的右键菜单状态。
///
/// 业务意图：
/// - 菜单坐标来自 GPUI 鼠标事件，右键菜单作为分析窗口根节点的绝对定位元素渲染，避免依赖平台系统菜单。
#[derive(Clone, Debug)]
struct HprofThreadContextMenu {
    /// 线程对象 ID。
    object_id: HprofObjectId,
    /// 菜单左上角窗口坐标。
    x: f32,
    /// 菜单左上角窗口坐标。
    y: f32,
}

/// HPROF 分析窗口状态。
///
/// 业务意图：
/// - 独立窗口需要同时表达运行中、完成、失败和取消，状态枚举可以让渲染逻辑保持互斥。
enum HprofAnalysisWindowState {
    /// 后台任务正在运行。
    Running {
        /// 最近一次进度快照。
        progress: HprofProgress,
    },
    /// 分析完成。
    Completed {
        /// 完整 dominator 结果。
        result: Arc<HprofDominatorResult>,
    },
    /// 分析失败。
    Failed {
        /// 中文错误说明。
        message: String,
        /// 失败前最后一次进度快照。
        progress: Option<HprofProgress>,
    },
    /// 用户取消。
    Canceled {
        /// 取消前最后一次进度快照。
        progress: Option<HprofProgress>,
    },
}

impl HprofAnalysisWindowState {
    /// 返回窗口头部展示的状态文案。
    ///
    /// 业务意图：
    /// - 失败、取消和完成状态需要在标题栏保持稳定中文文案；集中在纯状态方法里，避免渲染代码和测试各自拼装。
    fn status_label(&self) -> String {
        match self {
            Self::Running { progress } => progress.stage.label().to_string(),
            Self::Completed { .. } => "完成".to_string(),
            Self::Failed { .. } => "失败".to_string(),
            Self::Canceled { .. } => "已取消".to_string(),
        }
    }
}

/// HPROF 分析独立窗口根视图。
///
/// 业务意图：
/// - 该窗口观察主视图主题状态，保证明暗主题切换后 HPROF 分析结果同步刷新。
/// - 每次选择新 dump 时复用已有窗口，取消旧任务并重置滚动、展开状态和结果。
pub(super) struct HprofAnalysisWindowView {
    /// 主窗口视图实体。
    main_view: Entity<MainView>,
    /// 当前分析文件路径。
    file_path: PathBuf,
    /// 当前窗口状态。
    state: HprofAnalysisWindowState,
    /// dominator tree 虚拟列表滚动句柄。
    scroll_handle: UniformListScrollHandle,
    /// 当前展开的对象节点 ID。
    expanded_node_ids: HashSet<HprofObjectId>,
    /// 当前打开的线程右键菜单。
    thread_context_menu: Option<HprofThreadContextMenu>,
    /// 当前线程详情独立窗口句柄。
    ///
    /// 业务意图：
    /// - 重复右键不同线程时复用同一个详情窗口并替换内容，避免用户误开大量详情窗口。
    thread_details_window: Option<WindowHandle<HprofThreadDetailsWindowView>>,
    /// 分析任务代次。
    ///
    /// 业务意图：
    /// - 用户可以在旧任务尚未结束时选择新文件；后台回调必须用代次丢弃旧任务结果，避免覆盖新窗口内容。
    analysis_generation: usize,
    /// 当前任务取消标记。
    cancel_flag: Arc<AtomicBool>,
    /// 主窗口状态变更订阅。
    _main_view_subscription: gpui::Subscription,
}

impl HprofAnalysisWindowView {
    /// 创建 HPROF 分析窗口并启动解析。
    pub(super) fn new(
        main_view: Entity<MainView>,
        file_path: PathBuf,
        context: &mut Context<Self>,
    ) -> Self {
        let observed_main_view = main_view.clone();
        let main_view_subscription = context.observe(&observed_main_view, |_, _, context| {
            context.notify();
        });
        let initial_progress = HprofProgress::new(Self::file_size_for_progress(&file_path));
        let mut view = Self {
            main_view,
            file_path: file_path.clone(),
            state: HprofAnalysisWindowState::Running {
                progress: initial_progress,
            },
            scroll_handle: UniformListScrollHandle::new(),
            expanded_node_ids: HashSet::new(),
            thread_context_menu: None,
            thread_details_window: None,
            analysis_generation: 0,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            _main_view_subscription: main_view_subscription,
        };
        view.start_new_analysis(file_path, context);
        view
    }

    /// 启动一个新的 HPROF 分析任务。
    ///
    /// 业务意图：
    /// - 重复点击工具栏并选择新文件时复用同一个窗口；旧任务必须取消，旧展开状态也不能污染新 dump。
    pub(super) fn start_new_analysis(&mut self, file_path: PathBuf, context: &mut Context<Self>) {
        self.cancel_flag.store(true, Ordering::Relaxed);
        self.analysis_generation = self.analysis_generation.saturating_add(1);
        let generation = self.analysis_generation;
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let progress = HprofProgress::new(Self::file_size_for_progress(&file_path));
        let progress_snapshot = Arc::new(Mutex::new(progress.clone()));
        let task_finished = Arc::new(AtomicBool::new(false));

        self.file_path = file_path.clone();
        self.state = HprofAnalysisWindowState::Running { progress };
        self.scroll_handle = UniformListScrollHandle::new();
        self.expanded_node_ids.clear();
        self.thread_context_menu = None;
        self.cancel_flag = cancel_flag.clone();
        context.notify();

        self.spawn_progress_poller(
            generation,
            Arc::clone(&progress_snapshot),
            Arc::clone(&task_finished),
            context,
        );
        self.spawn_analysis_worker(
            generation,
            file_path,
            progress_snapshot,
            task_finished,
            cancel_flag,
            context,
        );
    }

    /// 启动进度轮询任务。
    fn spawn_progress_poller(
        &self,
        generation: usize,
        progress_snapshot: Arc<Mutex<HprofProgress>>,
        task_finished: Arc<AtomicBool>,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |view, app| {
                loop {
                    app.background_executor()
                        .timer(HPROF_PROGRESS_POLL_INTERVAL)
                        .await;
                    let finished = task_finished.load(Ordering::Relaxed);
                    let progress = progress_snapshot.lock().ok().map(|guard| guard.clone());
                    let should_continue = view
                        .update(app, |view, context| {
                            if view.analysis_generation != generation {
                                return false;
                            }
                            if let Some(progress) = progress
                                && let HprofAnalysisWindowState::Running {
                                    progress: current_progress,
                                } = &mut view.state
                            {
                                *current_progress = progress;
                                context.notify();
                            }
                            !finished
                        })
                        .unwrap_or(false);
                    if !should_continue {
                        break;
                    }
                }
            })
            .detach();
    }

    /// 启动后台 HPROF 解析任务。
    fn spawn_analysis_worker(
        &self,
        generation: usize,
        file_path: PathBuf,
        progress_snapshot: Arc<Mutex<HprofProgress>>,
        task_finished: Arc<AtomicBool>,
        cancel_flag: Arc<AtomicBool>,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |view, app| {
                let progress_for_worker = Arc::clone(&progress_snapshot);
                let cancel_for_worker = Arc::clone(&cancel_flag);
                let result = app
                    .background_executor()
                    .spawn(async move {
                        analyze_hprof_dominator_tree(
                            file_path,
                            |progress| {
                                if let Ok(mut current_progress) = progress_for_worker.lock() {
                                    *current_progress = progress;
                                }
                            },
                            cancel_for_worker,
                        )
                    })
                    .await;
                task_finished.store(true, Ordering::Relaxed);
                let final_progress = progress_snapshot.lock().ok().map(|guard| guard.clone());

                view.update(app, |view, context| {
                    if view.analysis_generation != generation {
                        return;
                    }
                    view.scroll_handle = UniformListScrollHandle::new();
                    view.expanded_node_ids.clear();
                    view.thread_context_menu = None;
                    view.state = match result {
                        Ok(result) => HprofAnalysisWindowState::Completed {
                            result: Arc::new(result),
                        },
                        Err(HprofError::Canceled) => HprofAnalysisWindowState::Canceled {
                            progress: final_progress,
                        },
                        Err(error) => HprofAnalysisWindowState::Failed {
                            message: error.to_string(),
                            progress: final_progress,
                        },
                    };
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 返回文件大小用于初始进度展示。
    fn file_size_for_progress(path: &Path) -> u64 {
        std::fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    }

    /// 取消当前分析。
    fn cancel_analysis(&mut self, context: &mut Context<Self>) {
        self.cancel_flag.store(true, Ordering::Relaxed);
        let progress = match &self.state {
            HprofAnalysisWindowState::Running { progress } => Some(progress.clone()),
            HprofAnalysisWindowState::Failed { progress, .. }
            | HprofAnalysisWindowState::Canceled { progress } => progress.clone(),
            HprofAnalysisWindowState::Completed { .. } => None,
        };
        if let HprofAnalysisWindowState::Running { .. } = self.state {
            self.state = HprofAnalysisWindowState::Canceled { progress };
        }
        context.notify();
    }

    /// 切换 dominator 节点展开状态。
    fn toggle_dominator_node(&mut self, object_id: HprofObjectId, context: &mut Context<Self>) {
        if !self.expanded_node_ids.remove(&object_id) {
            self.expanded_node_ids.insert(object_id);
        }
        self.thread_context_menu = None;
        context.notify();
    }

    /// 打开线程行右键菜单。
    ///
    /// 业务意图：
    /// - 只有完成状态下、且对象确认为线程对象时才显示菜单；非线程对象右键不会展示无效命令。
    fn open_thread_context_menu(
        &mut self,
        object_id: HprofObjectId,
        x: f32,
        y: f32,
        context: &mut Context<Self>,
    ) {
        let is_thread = matches!(
            &self.state,
            HprofAnalysisWindowState::Completed { result } if result.is_thread_object(object_id)
        );
        self.thread_context_menu = if is_thread {
            Some(HprofThreadContextMenu { object_id, x, y })
        } else {
            None
        };
        context.notify();
    }

    /// 根据右键菜单选择打开线程详情。
    fn show_thread_details(
        &mut self,
        object_id: HprofObjectId,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let details = match &self.state {
            HprofAnalysisWindowState::Completed { result } => {
                result.thread_details_for_object_id(object_id).cloned()
            }
            _ => None,
        };
        self.thread_context_menu = None;
        if let Some(details) = details {
            self.open_thread_details_window(details, window, context);
        }
        context.notify();
    }

    /// 打开或复用线程详情独立窗口。
    ///
    /// 业务意图：
    /// - MAT 以独立 tab 展示 Thread Details；LogClinic 使用独立窗口承载同样信息，避免挤占 dominator tree 的列宽和滚动空间。
    fn open_thread_details_window(
        &mut self,
        details: HprofThreadDetails,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let window_title = format!("线程详情 - {}", details.thread_name);
        if let Some(thread_window) = self.thread_details_window {
            let reused_title = window_title.clone();
            if thread_window
                .update(context, |thread_view, window, context| {
                    thread_view.update_details(details.clone(), context);
                    window.set_window_title(&reused_title);
                    window.activate_window();
                })
                .is_ok()
            {
                return;
            }
            self.thread_details_window = None;
        }

        let main_view_for_window = self.main_view.clone();
        let owner_view = context.weak_entity();
        let thread_window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(window_title.into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::centered(
                size(
                    px(HPROF_THREAD_DETAILS_WINDOW_WIDTH),
                    px(HPROF_THREAD_DETAILS_WINDOW_HEIGHT),
                ),
                context,
            )),
            is_resizable: true,
            is_minimizable: true,
            window_min_size: Some(size(px(680.0), px(420.0))),
            ..Default::default()
        };

        if let Ok(thread_window) = context.open_window(thread_window_options, move |window, app| {
            window.on_window_should_close(app, move |_, app| {
                if let Some(owner_view) = owner_view.upgrade() {
                    owner_view.update(app, |view, context| {
                        view.thread_details_window = None;
                        context.notify();
                    });
                }
                true
            });
            app.new(|context| {
                HprofThreadDetailsWindowView::new(main_view_for_window, details, context)
            })
        }) {
            self.thread_details_window = Some(thread_window);
        }
    }

    /// 渲染窗口头部。
    fn render_header(&self, palette: AppThemePalette) -> impl IntoElement {
        let status = self.state.status_label();
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(58.0))
            .px_4()
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
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .truncate()
                            .child("HPROF Dominator Tree"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .truncate()
                            .child(self.file_path.display().to_string()),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(rgb(palette.muted_text))
                    .child(status),
            )
    }

    /// 渲染运行中状态。
    fn render_running(
        &self,
        progress: &HprofProgress,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .size_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
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
                                    .child(progress.stage.label()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette.muted_text))
                                    .child(progress.message.clone()),
                            ),
                    )
                    .child(self.render_cancel_button(palette, context)),
            )
            .child(self.render_progress_bar(progress, palette))
            .child(self.render_progress_metrics(progress, palette))
    }

    /// 渲染取消按钮。
    fn render_cancel_button(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("hprof-analysis-cancel-button")
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded(px(5.0))
            .text_xs()
            .text_color(rgb(palette.text))
            .bg(rgb(palette.surface))
            .border_1()
            .border_color(rgb(palette.border))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(MainView::render_lucide_icon(
                Some(Icon::X),
                TOOLBAR_BUTTON_ICON_WIDTH,
                TOOLBAR_BUTTON_ICON_SIZE,
                palette.muted_text,
            ))
            .child("取消")
            .on_click(
                context.listener(|view, _event: &ClickEvent, _window, context| {
                    view.cancel_analysis(context);
                }),
            )
    }

    /// 渲染字节进度条。
    fn render_progress_bar(
        &self,
        progress: &HprofProgress,
        palette: AppThemePalette,
    ) -> impl IntoElement {
        let ratio = progress.byte_ratio();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .h(px(8.0))
                    .w_full()
                    .rounded(px(4.0))
                    .bg(rgb(palette.surface))
                    .child(
                        div()
                            .h(px(8.0))
                            .w(relative(ratio))
                            .rounded(px(4.0))
                            .bg(rgb(palette.accent)),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted_text))
                    .child(format!(
                        "{} / {} ({:.1}%)",
                        format_hprof_bytes(progress.bytes_read),
                        format_hprof_bytes(progress.total_bytes),
                        ratio * 100.0
                    )),
            )
            .when(progress.phase_total > 0, |container| {
                let phase_ratio = progress.phase_ratio();
                let phase_label = if progress.phase_unit.is_empty() {
                    format!(
                        "{} / {} ({:.1}%)",
                        progress.phase_done,
                        progress.phase_total,
                        phase_ratio * 100.0
                    )
                } else {
                    format!(
                        "{} / {} {} ({:.1}%)",
                        progress.phase_done,
                        progress.phase_total,
                        progress.phase_unit,
                        phase_ratio * 100.0
                    )
                };
                container
                    .child(
                        div()
                            .h(px(8.0))
                            .w_full()
                            .rounded(px(4.0))
                            .bg(rgb(palette.surface))
                            .child(
                                div()
                                    .h(px(8.0))
                                    .w(relative(phase_ratio))
                                    .rounded(px(4.0))
                                    .bg(rgb(palette.accent_hover)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child(format!(
                                "{}{}",
                                progress.sub_message,
                                if progress.sub_message.is_empty() {
                                    phase_label
                                } else {
                                    format!("：{phase_label}")
                                }
                            )),
                    )
            })
    }

    /// 渲染进度指标。
    fn render_progress_metrics(
        &self,
        progress: &HprofProgress,
        palette: AppThemePalette,
    ) -> impl IntoElement {
        div().flex().flex_wrap().gap_2().children([
            self.render_metric("记录", progress.record_count.to_string(), palette),
            self.render_metric("对象", progress.object_count.to_string(), palette),
            self.render_metric("类", progress.class_count.to_string(), palette),
            self.render_metric("GC Root", progress.gc_root_count.to_string(), palette),
            self.render_metric("引用边", progress.edge_count.to_string(), palette),
        ])
    }

    /// 渲染单个指标。
    fn render_metric(
        &self,
        label: &'static str,
        value: String,
        palette: AppThemePalette,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded(px(4.0))
            .bg(rgb(palette.surface))
            .text_xs()
            .child(div().text_color(rgb(palette.muted_text)).child(label))
            .child(div().text_color(rgb(palette.text)).child(value))
    }

    /// 渲染完成状态。
    fn render_completed(
        &self,
        result: Arc<HprofDominatorResult>,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let visible_rows = result.visible_rows(&self.expanded_node_ids);
        let row_count = visible_rows.len();
        let rows_for_render = visible_rows.clone();
        let scroll_handle = self.scroll_handle.clone();

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.render_result_summary(result.as_ref(), palette))
            .child(self.render_table_header(palette))
            .child(
                div().relative().flex_1().overflow_hidden().child(
                    uniform_list(
                        "hprof-dominator-tree-list",
                        row_count,
                        context.processor(
                            move |view, range: std::ops::Range<usize>, _window, context| {
                                range
                                    .filter_map(|index| rows_for_render.get(index).cloned())
                                    .map(|row| view.render_dominator_row(row, palette, context))
                                    .collect::<Vec<_>>()
                            },
                        ),
                    )
                    .size_full()
                    .track_scroll(scroll_handle),
                ),
            )
    }

    /// 渲染结果摘要。
    fn render_result_summary(
        &self,
        result: &HprofDominatorResult,
        palette: AppThemePalette,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(format!(
                        "{}，ID {} 字节，{} 个对象，{} 个类，{}",
                        result.header.label,
                        result.header.identifier_size,
                        result.total_objects,
                        result.total_classes,
                        result.file_path.display()
                    )),
            )
            .child(div().flex().flex_wrap().gap_2().children([
                self.render_metric(
                    "总 shallow",
                    format_hprof_bytes(result.total_shallow_size),
                    palette,
                ),
                self.render_metric(
                    "可达 shallow",
                    format_hprof_bytes(result.reachable_shallow_size),
                    palette,
                ),
                self.render_metric(
                    "可达对象",
                    result.reachable_object_count.to_string(),
                    palette,
                ),
                self.render_metric(
                    "不可达对象",
                    result.unreachable_object_count.to_string(),
                    palette,
                ),
                self.render_metric(
                    "不可达 shallow",
                    format_hprof_bytes(result.unreachable_shallow_size),
                    palette,
                ),
                self.render_metric("GC Root", result.gc_root_count.to_string(), palette),
                self.render_metric("引用边", result.edge_count.to_string(), palette),
                self.render_metric("原始引用边", result.raw_edge_count.to_string(), palette),
                self.render_metric(
                    "Reference referent 边",
                    result.reference_edge_stats.description(),
                    palette,
                ),
                self.render_metric(
                    "ClassLoader 合成边",
                    result.synthetic_class_loader_edge_count.to_string(),
                    palette,
                ),
                self.render_metric(
                    "Bootstrap 类 Root",
                    result.synthetic_bootstrap_class_root_count.to_string(),
                    palette,
                ),
                self.render_metric("size model", result.size_model.description(), palette),
            ]))
    }

    /// 渲染表头。
    fn render_table_header(&self, palette: AppThemePalette) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .w_full()
            .h(px(HPROF_DOMINATOR_ROW_HEIGHT))
            .px_4()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(palette.muted_text))
            .child(Self::table_name_cell("对象 / 类名 @ ID"))
            .child(Self::table_cell("Shallow", HPROF_DOMINATOR_SHALLOW_WIDTH))
            .child(Self::table_cell("Retained", HPROF_DOMINATOR_RETAINED_WIDTH))
            .child(Self::table_cell("%", HPROF_DOMINATOR_PERCENT_WIDTH))
    }

    /// 渲染 dominator 单行。
    fn render_dominator_row(
        &self,
        row: HprofDominatorRow,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let object_id = row.node.object_id;
        let can_expand = row.node.direct_child_count > 0;
        let expanded = self.expanded_node_ids.contains(&object_id);
        let icon = if can_expand {
            Some(if expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            })
        } else {
            None
        };

        div()
            .id(SharedString::from(format!(
                "hprof-dominator-row-{object_id}"
            )))
            .flex()
            .items_center()
            .w_full()
            .h(px(HPROF_DOMINATOR_ROW_HEIGHT))
            .px_4()
            .border_b_1()
            .border_color(rgb(palette.border))
            .text_xs()
            .text_color(rgb(palette.text))
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_mouse_down(
                MouseButton::Right,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.open_thread_context_menu(
                        object_id,
                        f32::from(event.position.x),
                        f32::from(event.position.y),
                        context,
                    );
                    context.stop_propagation();
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_w_0()
                    .pl(px(row.depth as f32 * HPROF_DOMINATOR_TREE_INDENT))
                    .child(
                        div()
                            .flex_none()
                            .cursor_pointer()
                            .child(MainView::render_lucide_icon(
                                icon,
                                LOG_TREE_CHEVRON_WIDTH,
                                LOG_TREE_CHEVRON_SIZE,
                                palette.muted_text,
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                context.listener(
                                    move |view, _event: &MouseDownEvent, _window, context| {
                                        view.toggle_dominator_node(object_id, context);
                                        context.stop_propagation();
                                    },
                                ),
                            ),
                    )
                    .child(
                        div()
                            .truncate()
                            .child(format!("{} @ 0x{:x}", row.node.name, object_id)),
                    ),
            )
            .child(Self::table_cell(
                format_hprof_bytes(row.node.shallow_size),
                HPROF_DOMINATOR_SHALLOW_WIDTH,
            ))
            .child(Self::table_cell(
                format_hprof_bytes(row.node.retained_size),
                HPROF_DOMINATOR_RETAINED_WIDTH,
            ))
            .child(Self::table_cell(
                format!("{:.2}", row.node.retained_percent),
                HPROF_DOMINATOR_PERCENT_WIDTH,
            ))
    }

    /// 渲染线程右键菜单。
    fn render_thread_context_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = &self.thread_context_menu else {
            return div().id("hprof-thread-context-menu-empty").hidden();
        };
        let object_id = menu.object_id;
        div()
            .id("hprof-thread-context-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(HPROF_THREAD_CONTEXT_MENU_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .child(
                div()
                    .id(SharedString::from(format!(
                        "hprof-thread-details-menu-{object_id}"
                    )))
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(HPROF_THREAD_CONTEXT_MENU_ITEM_HEIGHT))
                    .px_3()
                    .text_sm()
                    .text_color(rgb(palette.text))
                    .cursor_pointer()
                    .hover(move |item| item.bg(rgb(palette.hover)))
                    .child(MainView::render_lucide_icon(
                        Some(Icon::ListFilter),
                        16.0,
                        15.0,
                        palette.muted_text,
                    ))
                    .child("线程详情")
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(move |view, _event: &MouseDownEvent, window, context| {
                            view.show_thread_details(object_id, window, context);
                            context.stop_propagation();
                        }),
                    ),
            )
    }

    /// 构建固定宽度表格单元。
    fn table_cell(text: impl Into<String>, width: f32) -> gpui::Div {
        div().w(px(width)).flex_none().truncate().child(text.into())
    }

    /// 构建占满剩余空间的对象名称单元。
    ///
    /// 业务意图：
    /// - 数值列宽度固定，名称列使用 `flex_1` 填满剩余容器，避免宽窗口右侧留下不可用空白。
    fn table_name_cell(text: impl Into<String>) -> gpui::Div {
        div()
            .flex_1()
            .min_w(px(HPROF_DOMINATOR_NAME_MIN_WIDTH))
            .truncate()
            .child(text.into())
    }

    /// 渲染失败或取消状态。
    fn render_terminal_message(
        &self,
        title: &'static str,
        message: String,
        progress: Option<&HprofProgress>,
        palette: AppThemePalette,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .p_4()
            .size_full()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(title),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_text))
                    .child(message),
            )
            .when_some(progress, |container, progress| {
                container.child(self.render_progress_metrics(progress, palette))
            })
    }
}

impl Drop for HprofAnalysisWindowView {
    /// 窗口销毁时取消后台任务。
    fn drop(&mut self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }
}

impl Render for HprofAnalysisWindowView {
    /// 渲染 HPROF 分析窗口。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.main_view.read(context).palette();

        let body = match &self.state {
            HprofAnalysisWindowState::Running { progress } => self
                .render_running(progress, palette, context)
                .into_any_element(),
            HprofAnalysisWindowState::Completed { result } => self
                .render_completed(Arc::clone(result), palette, context)
                .into_any_element(),
            HprofAnalysisWindowState::Failed { message, progress } => self
                .render_terminal_message(
                    "HPROF 解析失败",
                    message.clone(),
                    progress.as_ref(),
                    palette,
                )
                .into_any_element(),
            HprofAnalysisWindowState::Canceled { progress } => self
                .render_terminal_message(
                    "HPROF 解析已取消",
                    "当前 dump 的后台解析任务已收到取消信号。".to_string(),
                    progress.as_ref(),
                    palette,
                )
                .into_any_element(),
        };

        div()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    if view.thread_context_menu.take().is_some() {
                        context.notify();
                    }
                }),
            )
            .flex()
            .flex_col()
            .relative()
            .size_full()
            .bg(rgb(palette.background))
            .child(self.render_header(palette))
            .child(body)
            .child(self.render_thread_context_menu(palette, context))
    }
}

/// HPROF 线程详情独立窗口。
///
/// 业务意图：
/// - 线程属性和堆栈信息通常比 dominator tree 行宽，独立窗口能让用户并排对照 MAT/LogClinic 结果，同时不压缩主列表。
struct HprofThreadDetailsWindowView {
    /// 主窗口视图实体，用于同步主题。
    main_view: Entity<MainView>,
    /// 当前展示的线程详情。
    details: HprofThreadDetails,
    /// 线程堆栈虚拟列表滚动句柄。
    stack_scroll_handle: UniformListScrollHandle,
    /// 线程属性区是否展开。
    ///
    /// 业务意图：
    /// - 线程堆栈通常比属性更需要纵向空间，用户检查堆栈时可以收起属性区，把窗口主要空间留给栈帧列表。
    properties_expanded: bool,
    /// 主窗口状态变更订阅。
    _main_view_subscription: gpui::Subscription,
}

impl HprofThreadDetailsWindowView {
    /// 创建线程详情窗口根视图。
    fn new(
        main_view: Entity<MainView>,
        details: HprofThreadDetails,
        context: &mut Context<Self>,
    ) -> Self {
        let observed_main_view = main_view.clone();
        let main_view_subscription = context.observe(&observed_main_view, |_, _, context| {
            context.notify();
        });
        Self {
            main_view,
            details,
            stack_scroll_handle: UniformListScrollHandle::new(),
            properties_expanded: true,
            _main_view_subscription: main_view_subscription,
        }
    }

    /// 替换当前展示线程。
    fn update_details(&mut self, details: HprofThreadDetails, context: &mut Context<Self>) {
        self.details = details;
        self.stack_scroll_handle = UniformListScrollHandle::new();
        context.notify();
    }

    /// 切换线程属性区展开状态。
    fn toggle_properties(&mut self, context: &mut Context<Self>) {
        self.properties_expanded = !self.properties_expanded;
        context.notify();
    }

    /// 生成线程堆栈可滚动行。
    fn thread_stack_lines(details: &HprofThreadDetails) -> Vec<String> {
        if details.stack_frames.is_empty() {
            return vec![
                details.thread_name.clone(),
                details
                    .stack_message
                    .clone()
                    .unwrap_or_else(|| "该 HPROF 未包含此线程的堆栈信息".to_string()),
            ];
        }
        let mut lines = Vec::with_capacity(details.stack_frames.len() + 1);
        lines.push(details.thread_name.clone());
        lines.extend(
            details
                .stack_frames
                .iter()
                .map(|frame| frame.display.clone()),
        );
        lines
    }

    /// 渲染线程详情标题区。
    fn render_header(&self, palette: AppThemePalette) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_4()
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
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .truncate()
                            .child(format!("Thread {}", self.details.thread_name)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .truncate()
                            .child(format!(
                                "{} @ 0x{:x}",
                                self.details.class_name, self.details.object_id
                            )),
                    ),
            )
    }

    /// 渲染线程属性表。
    fn render_properties(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let expanded = self.properties_expanded;
        let icon = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        div()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .cursor_pointer()
                    .hover(move |header| header.bg(rgb(palette.hover)))
                    .child(MainView::render_lucide_icon(
                        Some(icon),
                        LOG_TREE_CHEVRON_WIDTH,
                        LOG_TREE_CHEVRON_SIZE,
                        palette.muted_text,
                    ))
                    .child("Thread Properties")
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseDownEvent, _window, context| {
                            view.toggle_properties(context);
                            context.stop_propagation();
                        }),
                    ),
            )
            .when(expanded, |container| {
                container.child(
                    div()
                        .mx_4()
                        .mb_4()
                        .border_1()
                        .border_color(rgb(palette.border))
                        .children(self.details.properties.iter().enumerate().map(
                            |(index, property)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .h(px(HPROF_THREAD_PROPERTY_ROW_HEIGHT))
                                    .bg(rgb(if index % 2 == 0 {
                                        palette.surface
                                    } else {
                                        palette.background
                                    }))
                                    .child(
                                        div()
                                            .w(px(HPROF_THREAD_PROPERTY_NAME_WIDTH))
                                            .flex_none()
                                            .h_full()
                                            .flex()
                                            .items_center()
                                            .px_2()
                                            .border_r_1()
                                            .border_color(rgb(palette.border))
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(palette.text))
                                            .truncate()
                                            .child(property.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .px_2()
                                            .text_xs()
                                            .text_color(rgb(palette.text))
                                            .truncate()
                                            .child(property.value.clone()),
                                    )
                            },
                        )),
                )
            })
    }

    /// 渲染线程堆栈区域。
    fn render_stack(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let stack_lines = Self::thread_stack_lines(&self.details);
        let stack_count = stack_lines.len();
        let stack_handle = self.stack_scroll_handle.clone();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .flex_1()
            .min_h(px(0.0))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child("Thread Stack"),
            )
            .child(
                div().relative().flex_1().overflow_hidden().child(
                    uniform_list(
                        "hprof-thread-details-stack-list",
                        stack_count,
                        context.processor(
                            move |_view, range: std::ops::Range<usize>, _window, _context| {
                                range
                                    .filter_map(|index| stack_lines.get(index).cloned())
                                    .map(|line| {
                                        div()
                                            .h(px(HPROF_THREAD_STACK_ROW_HEIGHT))
                                            .flex()
                                            .items_center()
                                            .text_xs()
                                            .font_family(LOG_VIEWER_FONT_FAMILY)
                                            .text_color(rgb(palette.text))
                                            .child(line)
                                    })
                                    .collect::<Vec<_>>()
                            },
                        ),
                    )
                    .size_full()
                    .track_scroll(stack_handle),
                ),
            )
    }
}

impl Render for HprofThreadDetailsWindowView {
    /// 渲染 HPROF 线程详情窗口。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.main_view.read(context).palette();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(palette.background))
            .child(self.render_header(palette))
            .child(self.render_properties(palette, context))
            .child(self.render_stack(palette, context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证失败和取消状态使用稳定中文文案。
    ///
    /// 业务意图：
    /// - HPROF 后台任务可能因为文件损坏、权限不足或用户主动取消结束，窗口头部必须展示可理解状态，不能泄漏底层错误枚举。
    #[test]
    fn 失败和取消状态文案稳定() {
        let progress = HprofProgress::new(16);
        let failed = HprofAnalysisWindowState::Failed {
            message: "文件头格式错误".to_string(),
            progress: Some(progress.clone()),
        };
        let canceled = HprofAnalysisWindowState::Canceled {
            progress: Some(progress),
        };

        assert_eq!(failed.status_label(), "失败");
        assert_eq!(canceled.status_label(), "已取消");
    }

    /// 验证线程堆栈行生成会保留线程名，并在缺失栈时给出中文提示。
    ///
    /// 业务意图：
    /// - UI 右侧详情面板使用虚拟列表渲染堆栈，列表输入必须是稳定的纯字符串，避免渲染阶段再判断 HPROF 缺失语义。
    #[test]
    fn 线程详情堆栈行会处理缺失堆栈() {
        let details = HprofThreadDetails {
            object_id: 1,
            class_name: "java.lang.Thread".to_string(),
            thread_name: "worker".to_string(),
            properties: Vec::new(),
            stack_frames: Vec::new(),
            stack_message: Some("该 HPROF 未包含此线程的堆栈信息".to_string()),
        };

        let lines = HprofThreadDetailsWindowView::thread_stack_lines(&details);

        assert_eq!(lines[0], "worker");
        assert_eq!(lines[1], "该 HPROF 未包含此线程的堆栈信息");
    }
}
