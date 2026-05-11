// HPROF dump 分析视图。
//
// 业务意图：
// - HPROF dominator tree 是独立于日志正文的重型分析视图，当前嵌入主窗口“HPROF解析”功能页展示进度和结果。
// - 解析任务运行在后台线程，视图只消费进度快照和最终结果，确保 GPUI 主线程不会被大 dump 解析阻塞。
//
// 边界条件：
// - 用户关闭窗口或点击取消时，只能尽快停止解析循环；第三方 dominator 算法阶段无法细粒度中断，因此在进入前后检查取消状态。
// - GPUI 文件选择器不能限制扩展名，后缀和 header 校验失败会在本视图展示中文错误。

use std::{
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
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

/// HPROF dominator tree 中线程行的右键菜单状态。
///
/// 业务意图：
/// - 菜单坐标来自 GPUI 鼠标事件，右键菜单作为分析视图根节点的绝对定位元素渲染，避免依赖平台系统菜单。
#[derive(Clone, Debug)]
struct HprofThreadContextMenu {
    /// 线程对象 ID。
    object_id: HprofObjectId,
    /// 菜单左上角在 HPROF 分析视图内的局部横坐标。
    ///
    /// 业务意图：
    /// - GPUI 鼠标事件给出窗口坐标，但菜单作为 HPROF 内嵌视图的绝对定位子元素渲染，必须先扣除左侧大导航宽度。
    x: f32,
    /// 菜单左上角在 HPROF 分析视图内的局部纵坐标。
    ///
    /// 业务意图：
    /// - HPROF 页顶部还有文件选择操作栏，菜单纵坐标需要扣除这段高度，否则会显示在右键点下方较远的位置。
    y: f32,
}

/// HPROF 分析视图状态。
///
/// 业务意图：
/// - 主窗口内嵌页需要同时表达未选择文件、运行中、完成、失败和取消，状态枚举可以让渲染逻辑保持互斥。
/// - 完成结果只在 GPUI 主线程状态和渲染闭包中共享，使用 `Rc` 可以明确表达该结果不会跨线程共享。
enum HprofAnalysisState {
    /// 尚未选择 dump 文件。
    Idle,
    /// 后台任务正在运行。
    Running {
        /// 最近一次进度快照。
        progress: HprofProgress,
    },
    /// 分析完成。
    Completed {
        /// 完整 dominator 结果。
        result: Rc<HprofDominatorResult>,
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

/// HPROF 分析主窗口内嵌视图。
///
/// 业务意图：
/// - 该视图观察主视图主题状态，保证明暗主题切换后 HPROF 分析结果同步刷新。
/// - 每次选择新 dump 时复用已有实体，取消旧任务并重置滚动、展开状态和结果。
pub(in crate::app) struct HprofAnalysisView {
    /// 主窗口视图实体。
    main_view: Entity<MainView>,
    /// 当前分析文件路径；未选择文件时为 `None`，用于渲染 HPROF 页初始空态。
    file_path: Option<PathBuf>,
    /// 当前视图状态。
    state: HprofAnalysisState,
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

impl HprofAnalysisView {
    /// 返回新建 HPROF 页的初始状态。
    ///
    /// 业务意图：
    /// - 主窗口切到 HPROF 功能页时只能展示空态，不能因为实体创建就读取磁盘或启动后台解析。
    /// - 初始状态集中在纯函数中，便于测试锁定“未选择文件”这一入口行为。
    fn initial_state() -> HprofAnalysisState {
        HprofAnalysisState::Idle
    }

    /// 创建 HPROF 分析视图。
    ///
    /// 业务意图：
    /// - 视图实体会随主窗口功能页存在；创建时不启动后台解析，只有用户在 HPROF 页选择文件后才读取磁盘。
    pub(in crate::app) fn new(main_view: Entity<MainView>, context: &mut Context<Self>) -> Self {
        let observed_main_view = main_view.clone();
        let main_view_subscription = context.observe(&observed_main_view, |_, _, context| {
            context.notify();
        });
        Self {
            main_view,
            file_path: None,
            state: Self::initial_state(),
            scroll_handle: UniformListScrollHandle::new(),
            expanded_node_ids: HashSet::new(),
            thread_context_menu: None,
            thread_details_window: None,
            analysis_generation: 0,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            _main_view_subscription: main_view_subscription,
        }
    }

    /// 启动一个新的 HPROF 分析任务。
    ///
    /// 业务意图：
    /// - 重复点击 HPROF 页文件按钮并选择新文件时复用同一个视图；旧任务必须取消，旧展开状态也不能污染新 dump。
    pub(in crate::app) fn start_new_analysis(
        &mut self,
        file_path: PathBuf,
        context: &mut Context<Self>,
    ) {
        self.cancel_flag.store(true, Ordering::Relaxed);
        self.analysis_generation = self.analysis_generation.saturating_add(1);
        let generation = self.analysis_generation;
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let progress = HprofProgress::new(Self::file_size_for_progress(&file_path));
        let progress_snapshot = Arc::new(Mutex::new(progress.clone()));
        let task_finished = Arc::new(AtomicBool::new(false));

        self.file_path = Some(file_path.clone());
        self.state = HprofAnalysisState::Running { progress };
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
                                && let HprofAnalysisState::Running {
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
    ///
    /// 业务意图：
    /// - 后台线程返回后在 UI 线程把结果包装进完成态；完成态只在 UI 线程内共享，不需要线程安全引用计数。
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
                        Ok(result) => HprofAnalysisState::Completed {
                            result: Rc::new(result),
                        },
                        Err(HprofError::Canceled) => HprofAnalysisState::Canceled {
                            progress: final_progress,
                        },
                        Err(error) => HprofAnalysisState::Failed {
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
            HprofAnalysisState::Running { progress } => Some(progress.clone()),
            HprofAnalysisState::Failed { progress, .. }
            | HprofAnalysisState::Canceled { progress } => progress.clone(),
            HprofAnalysisState::Idle | HprofAnalysisState::Completed { .. } => None,
        };
        if let HprofAnalysisState::Running { .. } = self.state {
            self.state = HprofAnalysisState::Canceled { progress };
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
    /// - 鼠标事件坐标来自主窗口内容区，右键菜单渲染在 HPROF 内嵌视图内，因此必须转换为视图局部坐标。
    fn open_thread_context_menu(
        &mut self,
        object_id: HprofObjectId,
        window_x: f32,
        window_y: f32,
        context: &mut Context<Self>,
    ) {
        let is_thread = matches!(
            &self.state,
            HprofAnalysisState::Completed { result } if result.is_thread_object(object_id)
        );
        let (x, y) = Self::thread_context_menu_position(window_x, window_y);
        self.thread_context_menu = if is_thread {
            Some(HprofThreadContextMenu { object_id, x, y })
        } else {
            None
        };
        context.notify();
    }

    /// 将窗口内容坐标转换为 HPROF 视图内的右键菜单坐标。
    ///
    /// 业务意图：
    /// - HPROF 视图嵌在“左侧大导航 + 顶部 HPROF 操作栏”之后；菜单如果直接使用窗口坐标，会相对右键点整体向右、向下偏移。
    /// - 这里集中转换，避免行右键、后续表格菜单或测试各自硬编码偏移。
    ///
    /// 边界条件：
    /// - 如果事件来自导航区或操作栏上方，坐标会压到 0，避免异常输入导致菜单渲染到负坐标区域。
    fn thread_context_menu_position(window_x: f32, window_y: f32) -> (f32, f32) {
        (
            (window_x - MAIN_NAV_WIDTH).max(0.0),
            (window_y - TOOLBAR_HEIGHT).max(0.0),
        )
    }

    /// 根据右键菜单选择打开线程详情。
    fn show_thread_details(
        &mut self,
        object_id: HprofObjectId,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let details = match &self.state {
            HprofAnalysisState::Completed { result } => {
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

    /// 渲染未选择文件的初始状态。
    ///
    /// 业务意图：
    /// - HPROF 已迁入主窗口大功能页，用户进入该页时不应立即触发磁盘读取；空态只提示下一步选择文件。
    fn render_idle(&self, palette: AppThemePalette) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .size_full()
            .px_4()
            .child(MainView::render_lucide_icon(
                Some(Icon::ChartNoAxesCombined),
                34.0,
                30.0,
                palette.muted_text,
            ))
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child("请选择 HPROF dump 文件"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_text))
                    .child("支持 .hprof 和 .bin，解析会在后台线程执行。"),
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
        result: Rc<HprofDominatorResult>,
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
        let mut metrics = vec![
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
        ];
        if let Some(cache_status) = &result.cache_status {
            metrics.push(self.render_metric("缓存", cache_status.clone(), palette));
        }

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
            .child(div().flex().flex_wrap().gap_2().children(metrics))
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

    /// 渲染线程右键菜单的透明关闭遮罩。
    ///
    /// 业务意图：
    /// - HPROF dominator 表格里的线程行可以打开详情菜单；菜单打开时，菜单外点击应只负责关闭菜单，不能同时展开树节点。
    fn render_thread_context_menu_dismiss_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if self.thread_context_menu.is_none() {
            return div()
                .id("hprof-thread-context-menu-dismiss-overlay-empty")
                .hidden();
        }

        div()
            .id("hprof-thread-context-menu-dismiss-overlay")
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 遮罩吃掉左键按下，避免底层 dominator 树在菜单关闭前同步处理点击。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    // 右键不依赖 click 合成事件；直接关闭旧菜单并截断，避免穿透到其它线程行。
                    view.thread_context_menu = None;
                    context.notify();
                    context.stop_propagation();
                }),
            )
            .on_click(
                context.listener(|view, _event: &ClickEvent, _window, context| {
                    view.thread_context_menu = None;
                    context.notify();
                    // click 仍然必须在遮罩处结束，避免底层控件收到同一次点击。
                    context.stop_propagation();
                }),
            )
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
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // HPROF 线程菜单覆盖在 dominator 表格上，菜单空白区域也不能把事件传回表格行。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 菜单层上的右键只应被当前菜单消费，避免底层行重新打开菜单或切换目标线程。
                    context.stop_propagation();
                }),
            )
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

impl Drop for HprofAnalysisView {
    /// 视图销毁时取消后台任务。
    fn drop(&mut self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }
}

impl Render for HprofAnalysisView {
    /// 渲染 HPROF 分析视图。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.main_view.read(context).palette();

        let body = match &self.state {
            HprofAnalysisState::Idle => self.render_idle(palette).into_any_element(),
            HprofAnalysisState::Running { progress } => self
                .render_running(progress, palette, context)
                .into_any_element(),
            HprofAnalysisState::Completed { result } => self
                .render_completed(Rc::clone(result), palette, context)
                .into_any_element(),
            HprofAnalysisState::Failed { message, progress } => self
                .render_terminal_message(
                    "HPROF 解析失败",
                    message.clone(),
                    progress.as_ref(),
                    palette,
                )
                .into_any_element(),
            HprofAnalysisState::Canceled { progress } => self
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
            // HPROF 已由主窗口页面提供顶部文件选择入口，内嵌视图不再重复渲染独立窗口时代的标题和文件状态栏。
            .child(body)
            .child(self.render_thread_context_menu_dismiss_overlay(context))
            .child(self.render_thread_context_menu(palette, context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证 HPROF 初始状态不会进入解析阶段。
    ///
    /// 业务意图：
    /// - HPROF 页迁入主窗口后，用户点击导航只应看到空态；只有选择文件后才允许后台读取 dump。
    #[test]
    fn hprof_初始状态保持未选择文件() {
        assert!(matches!(
            HprofAnalysisView::initial_state(),
            HprofAnalysisState::Idle
        ));
    }

    /// 验证 HPROF 线程右键菜单坐标会转换为内嵌视图局部坐标。
    ///
    /// 业务意图：
    /// - HPROF 分析结果现在嵌在主窗口右侧，窗口坐标包含左侧大导航和顶部文件选择操作栏；菜单必须扣除这些偏移后才会贴近右键位置。
    #[test]
    fn hprof_线程菜单坐标扣除主导航和操作栏() {
        assert_eq!(
            HprofAnalysisView::thread_context_menu_position(
                MAIN_NAV_WIDTH + 120.0,
                TOOLBAR_HEIGHT + 80.0
            ),
            (120.0, 80.0)
        );
        assert_eq!(
            HprofAnalysisView::thread_context_menu_position(20.0, 10.0),
            (0.0, 0.0)
        );
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
