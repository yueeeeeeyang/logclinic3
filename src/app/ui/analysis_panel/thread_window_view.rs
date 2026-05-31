// 线程日志分析独立窗口与数据模型。
//
// 业务意图：
// - 该文件从历史 `app.rs` 中拆出，集中维护 Java thread dump 时间线窗口、状态过滤、滚动条和悬浮气泡。
// - 当前作为 `app/ui` 的窗口视图模块运行，通过显式 `pub(in crate::app)` 接口和主视图协作。
//
// 边界条件：
// - 本阶段必须保持行为不变；纯解析逻辑继续留在顶层 `thread_analysis` 业务域，窗口渲染留在本 UI 模块。

use super::*;

/// 线程分析线程名列估算时单个显示列的像素宽度。
///
/// 业务意图：
/// - GPUI 的真实字形测量发生在绘制阶段，但线程名列宽必须在布局阶段稳定给出，否则首帧会先用默认宽度，后续点击窗口才刷新。
/// - 这里按当前 `text_xs` 的实际展示密度取略宽的估算值，让长连接线程名能在结果打开时直接完整显示。
///
/// 边界条件：
/// - 估算只影响列宽，不参与搜索、过滤或线程身份判断；不同系统字体存在少量差异时宁可略宽，也避免继续截断。
const THREAD_ANALYSIS_NAME_COLUMN_ESTIMATED_CHAR_WIDTH: f32 = 7.5;

/// 线程分析线程名列在文本右侧预留的安全像素。
///
/// 业务意图：
/// - 行内线程名元素带有右侧内边距，并且截断渲染需要少量余量；统一加入估算宽度，避免最长线程名末尾贴住时间线色块。
const THREAD_ANALYSIS_NAME_COLUMN_TEXT_PADDING: f32 = 12.0;

/// 堆栈并发分析表格行高。
///
/// 业务意图：
/// - 并发分析可能包含大量堆栈指纹，行高必须固定才能使用 `uniform_list` 做虚拟渲染，避免大日志结果一次性创建全部行。
const THREAD_ANALYSIS_CONCURRENCY_ROW_HEIGHT: f32 = 32.0;

/// 堆栈并发分析总样本列宽度。
const THREAD_ANALYSIS_CONCURRENCY_TOTAL_COLUMN_WIDTH: f32 = 92.0;

/// 堆栈并发分析最大并发列宽度。
const THREAD_ANALYSIS_CONCURRENCY_MAX_COLUMN_WIDTH: f32 = 92.0;

/// 堆栈并发分析状态数量列宽度。
///
/// 业务意图：
/// - 状态列固定宽度能保证表头和虚拟列表行严格对齐，避免滚动时数字列抖动。
const THREAD_ANALYSIS_CONCURRENCY_STATE_COLUMN_WIDTH: f32 = 104.0;

/// 堆栈并发分析代表栈帧列最小宽度。
///
/// 边界条件：
/// - 业务栈帧通常很长，最小宽度需要保留包名和方法名前缀；窗口变窄时仍允许表格整体横向溢出，由 GPUI 裁剪处理。
const THREAD_ANALYSIS_CONCURRENCY_NAME_MIN_WIDTH: f32 = 320.0;

/// 搜索结果面板高度拖动状态。
///
/// 业务意图：
/// - 用户按住面板顶部拖拽条上下拖动时，保存拖动开始点和开始高度，后续移动可稳定换算新高度。
#[derive(Clone, Copy)]
pub(in crate::app) struct SearchResultsResizeDrag {
    /// 拖动开始时鼠标在窗口内容坐标中的纵坐标。
    pub(in crate::app) start_y: Pixels,
    /// 拖动开始时结果面板高度。
    pub(in crate::app) start_height: f32,
}

/// 一次搜索任务的后台输入。
///
/// 业务意图：
/// - 当前文件搜索复用已解码行；当前目录搜索复用加载树中收集到的文件来源。
/// - 选中文件搜索复用左侧树右键时固定下来的来源快照，不依赖当前打开 tab。
/// - 使用枚举可以在启动任务前完成所有 UI 状态校验，后台逻辑只处理明确输入。
/// - 当前文件分支携带已解码文档的堆分配指针，既避免重新读取日志，也避免单个枚举因文档 payload 过大影响任务传递成本。
pub(in crate::app) enum SearchTarget {
    /// 搜索当前文件。
    CurrentFile {
        /// 当前文件来源。
        source: LogFileSource,
        /// 当前文件文档。
        ///
        /// 业务意图：
        /// - 小文件共享已解码行集合，超大文件共享分页文档，避免搜索路径重新把大文件载入内存。
        document: Box<LogTabDocument>,
    },
    /// 搜索当前目录递归来源。
    CurrentDirectory {
        /// 当前目录下所有可打开文件来源。
        sources: Vec<LogFileSource>,
    },
    /// 搜索左侧目录树选中文件快照。
    SelectedFiles {
        /// 右键打开搜索对话框时选中的可打开文件来源。
        sources: Vec<LogFileSource>,
    },
}

/// 线程日志分析独立窗口根视图。
///
/// 业务意图：
/// - 线程日志分析以独立窗口展示，避免覆盖主日志查看上下文。
/// - 窗口观察主视图主题变化，确保明暗主题切换后时间线背景和文字同步刷新。
pub(in crate::app) struct ThreadAnalysisWindowView {
    /// 主窗口视图实体。
    pub(in crate::app) main_view: Entity<MainView>,
    /// 当前分析结果。
    pub(in crate::app) analysis: ThreadAnalysisData,
    /// 当前线程分析结果页签。
    ///
    /// 业务意图：
    /// - 频率分析和并发分析共享同一份解析数据，但展示模型不同；显式页签状态避免用滚动位置或图例状态推断当前视图。
    /// - 新分析结果打开时默认回到频率分析，保持旧用户进入窗口后看到的内容不变。
    pub(in crate::app) active_tab: ThreadAnalysisResultTab,
    /// 线程时间线虚拟列表滚动句柄。
    ///
    /// 业务意图：
    /// - Java thread dump 可能包含数千个线程，不能一次性把所有线程行都创建成 GPUI 元素。
    /// - 使用 `uniform_list` 只渲染可见行，并通过该句柄保存纵向和横向滚动位置。
    pub(in crate::app) scroll_handle: UniformListScrollHandle,
    /// 堆栈并发分析虚拟列表滚动句柄。
    ///
    /// 业务意图：
    /// - 并发页和频率页都可能有大量行，两者滚动位置互相独立，避免用户在一个页签滚动后切回另一个页签位置突变。
    pub(in crate::app) concurrency_scroll_handle: UniformListScrollHandle,
    /// 当前线程名列的布局宽度。
    ///
    /// 业务意图：
    /// - 线程名列宽度由可见线程名估算得到，并同时影响行布局与横向滚动条轨道起点。
    /// - 渲染时刷新该值，拖动横向滚动条时直接复用，避免鼠标移动每一帧都重新扫描所有线程状态。
    ///
    /// 边界条件：
    /// - 新窗口、替换分析结果或切换状态过滤后先回到默认最小宽度；下一次 render 会用最新可见线程名覆盖。
    pub(in crate::app) name_column_width: f32,
    /// 当前线程分析滚动条拖动状态。
    ///
    /// 业务意图：
    /// - 分析页面需要显式横向和纵向滚动条；拖动时保存方向和鼠标在滑块内的偏移，避免滑块跳动。
    pub(in crate::app) scrollbar_drag: Option<ThreadAnalysisScrollbarDrag>,
    /// 当前鼠标悬浮色块后展示的线程信息气泡。
    ///
    /// 业务意图：
    /// - 气泡跟随用户当前悬浮的色块展示线程详情；窗口重绘或滚动时不重新解析日志。
    /// - `None` 表示鼠标未停留在可见状态色块上，或分析数据已被替换。
    pub(in crate::app) cell_popup: Option<ThreadAnalysisCellPopup>,
    /// 最近一次点击打开堆栈详情的线程色块。
    ///
    /// 业务意图：
    /// - 用户打开堆栈详情后，需要快速确认当前详情窗口对应的是哪个快照中的哪个线程。
    /// - 使用矩阵内 `Arc` 的指针身份记录目标，不复制日志来源或线程名，避免同一线程在多个快照中出现时误高亮其它色块。
    /// - `None` 表示当前分析结果还没有执行过色块点击，或分析数据已被替换。
    pub(in crate::app) jumped_cell: Option<Arc<ThreadTimelineCell>>,
    /// 当前线程堆栈详情窗口句柄。
    ///
    /// 业务意图：
    /// - 点击时间线色块时需要额外弹出完整堆栈窗口；句柄保存在分析窗口中，便于后续点击复用同一个详情窗口。
    /// - 详情窗口关闭后会清空该字段；分析结果替换时会把详情窗口置为空状态，避免继续展示旧结果。
    pub(in crate::app) stack_window: Option<WindowHandle<ThreadStackWindowView>>,
    /// 当前线程分析图中允许显示的线程状态集合。
    ///
    /// 业务意图：
    /// - 右上角图例同时作为状态过滤器；用户可以按状态隐藏无关线程和色块，默认只关注 RUNNABLE 线程。
    /// - 集合为空时表示用户主动隐藏全部状态，时间线列表应展示为空，而不是自动回退为全部显示。
    pub(in crate::app) visible_state_kinds: HashSet<ThreadStateKind>,
    /// 主窗口状态变更订阅。
    pub(in crate::app) _main_view_subscription: gpui::Subscription,
}

/// 线程分析结果窗口的页签类型。
///
/// 业务意图：
/// - 页签名称进入用户界面和测试断言，集中定义可避免渲染处散落字符串。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum ThreadAnalysisResultTab {
    /// 当前已有的线程状态时间线分析。
    Frequency,
    /// 按规范化堆栈指纹聚合的并发统计分析。
    Concurrency,
}

impl ThreadAnalysisResultTab {
    /// 返回页签展示文案。
    pub(in crate::app) fn label(self) -> &'static str {
        match self {
            Self::Frequency => "线程频率分析",
            Self::Concurrency => "堆栈并发分析",
        }
    }
}

/// 线程分析滚动条拖动状态。
///
/// 业务意图：
/// - 线程分析窗口没有 tab 维度，只需要记录当前拖动轴向和鼠标按下时的滑块内偏移。
#[derive(Clone, Copy)]
pub(in crate::app) struct ThreadAnalysisScrollbarDrag {
    /// 当前拖动的滚动轴。
    pub(in crate::app) axis: LogScrollbarAxis,
    /// 鼠标按下点相对滑块起点的偏移。
    pub(in crate::app) cursor_offset: Pixels,
}

/// 线程分析色块悬浮气泡状态。
///
/// 业务意图：
/// - GPUI 渲染是声明式的，悬浮事件只记录展示所需的数据和窗口坐标，真正的气泡由下一次 render 输出。
#[derive(Clone)]
pub(in crate::app) struct ThreadAnalysisCellPopup {
    /// 被鼠标悬浮的时间线单元。
    pub(in crate::app) cell: Arc<ThreadTimelineCell>,
    /// 气泡左上角相对窗口的横向位置。
    pub(in crate::app) x: Pixels,
    /// 气泡左上角相对窗口的纵向位置。
    pub(in crate::app) y: Pixels,
}

impl ThreadAnalysisWindowView {
    /// 创建线程日志分析窗口根视图。
    pub(in crate::app) fn new(
        main_view: Entity<MainView>,
        analysis: ThreadAnalysisData,
        context: &mut Context<Self>,
    ) -> Self {
        let observed_main_view = main_view.clone();
        let main_view_subscription = context.observe(&observed_main_view, |_, _, context| {
            context.notify();
        });
        Self {
            main_view,
            analysis,
            active_tab: Self::default_result_tab(),
            scroll_handle: UniformListScrollHandle::new(),
            concurrency_scroll_handle: UniformListScrollHandle::new(),
            name_column_width: THREAD_ANALYSIS_NAME_COLUMN_WIDTH,
            scrollbar_drag: None,
            cell_popup: None,
            jumped_cell: None,
            stack_window: None,
            visible_state_kinds: Self::default_visible_state_kinds(),
            _main_view_subscription: main_view_subscription,
        }
    }

    /// 更新分析结果。
    ///
    /// 业务意图：
    /// - 用户重复对不同文件执行线程日志分析时复用已有窗口，直接替换数据并激活窗口。
    pub(in crate::app) fn set_analysis(
        &mut self,
        analysis: ThreadAnalysisData,
        context: &mut Context<Self>,
    ) {
        let replacing_progress_with_progress =
            self.analysis.progress.is_some() && analysis.progress.is_some();
        self.analysis = analysis;
        if !replacing_progress_with_progress {
            self.active_tab = Self::default_result_tab();
            self.scroll_handle = UniformListScrollHandle::new();
            self.concurrency_scroll_handle = UniformListScrollHandle::new();
            self.name_column_width = THREAD_ANALYSIS_NAME_COLUMN_WIDTH;
            self.scrollbar_drag = None;
            self.cell_popup = None;
            self.jumped_cell = None;
            if let Some(stack_window) = self.stack_window {
                if stack_window
                    .update(context, |stack_view, window, context| {
                        stack_view.update_stacks(Vec::new(), 0, context);
                        window
                            .set_window_title(&ThreadStackWindowView::window_title_for_cell(None));
                    })
                    .is_err()
                {
                    self.stack_window = None;
                }
            }
            self.visible_state_kinds = Self::default_visible_state_kinds();
        }
        context.notify();
    }

    /// 返回线程分析窗口默认页签。
    ///
    /// 业务意图：
    /// - 用户新增并发分析后，旧入口仍应首先看到原有结果，降低行为变更风险。
    pub(in crate::app) fn default_result_tab() -> ThreadAnalysisResultTab {
        ThreadAnalysisResultTab::Frequency
    }

    /// 切换线程分析结果页签。
    ///
    /// 业务意图：
    /// - 页签切换只影响展示形态，不重新解析日志；同时关闭悬浮气泡和滚动条拖动，避免频率页浮层残留到并发页。
    fn set_active_tab(&mut self, active_tab: ThreadAnalysisResultTab, context: &mut Context<Self>) {
        if self.active_tab == active_tab {
            return;
        }
        self.active_tab = active_tab;
        self.scrollbar_drag = None;
        self.cell_popup = None;
        context.notify();
    }

    /// 渲染线程日志解析进度条。
    ///
    /// 业务意图：
    /// - 解析过程运行在后台线程，窗口中央用短进度条展示当前文件、已完成文件数和已解析快照数。
    /// - 进度只在 `ThreadAnalysisData.progress` 存在时显示；最终结果不会占用额外布局空间。
    ///
    /// 布局约束：
    /// - 进度条宽度固定在较短的面板内，并限制最大相对宽度，避免大窗口下横向铺满影响阅读。
    /// - 使用绝对居中覆盖内容区，保证解析期间用户视线集中在进度反馈上，而不是顶部标题栏。
    fn render_progress_bar(
        &self,
        progress: &ThreadAnalysisProgress,
        palette: AppThemePalette,
    ) -> impl IntoElement {
        let ratio = progress.ratio();
        div()
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(360.0))
                    .max_w(relative(0.56))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.panel))
                    .shadow_lg()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette.muted_text))
                                    .truncate()
                                    .child(progress.message()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette.muted_text))
                                    .child(progress.label()),
                            ),
                    )
                    .child(
                        div()
                            .h(px(6.0))
                            .w_full()
                            .rounded(px(999.0))
                            .bg(rgb(palette.surface))
                            .overflow_hidden()
                            .child(div().h_full().w(relative(ratio)).bg(rgb(palette.accent))),
                    ),
            )
    }

    /// 渲染线程状态图例。
    fn render_legend(
        &self,
        palette: AppThemePalette,
        theme: EffectiveTheme,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_3()
            .children(Self::legend_state_kinds().into_iter().map(|state| {
                let visible = self.visible_state_kinds.contains(&state);
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .py_1()
                    .rounded(px(4.0))
                    .text_xs()
                    .text_color(rgb(if visible {
                        palette.text
                    } else {
                        palette.muted_text
                    }))
                    .bg(rgb(if visible {
                        palette.selected
                    } else {
                        palette.panel
                    }))
                    .opacity(if visible { 1.0 } else { 0.48 })
                    .cursor_pointer()
                    .hover(move |legend_item| legend_item.bg(rgb(palette.hover)))
                    .child(
                        div()
                            .w(px(10.0))
                            .h(px(10.0))
                            .rounded(px(2.0))
                            .bg(rgb(state.color(theme))),
                    )
                    .child(state.label())
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                            view.toggle_visible_state_kind(state, context);
                            context.stop_propagation();
                        }),
                    )
            }))
    }

    /// 渲染线程分析结果页签。
    ///
    /// 业务意图：
    /// - 频率分析保留原有时间线视图，并发分析提供按堆栈指纹聚合的新视图；页签让两类结果在同一分析窗口内切换。
    /// - 页签按钮需要消费鼠标事件，避免点击页签时触发窗口根节点关闭气泡之外的其它下层交互。
    fn render_result_tabs(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        div().flex().items_center().gap_1().children(
            [
                ThreadAnalysisResultTab::Frequency,
                ThreadAnalysisResultTab::Concurrency,
            ]
            .into_iter()
            .map(|tab| {
                let active = self.active_tab == tab;
                div()
                    .id(SharedString::from(format!(
                        "thread-analysis-tab-{}",
                        tab.label()
                    )))
                    .h(px(28.0))
                    .px_3()
                    .flex()
                    .items_center()
                    .rounded(px(6.0))
                    .text_xs()
                    .font_weight(if active {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(rgb(if active {
                        palette.text
                    } else {
                        palette.muted_text
                    }))
                    .bg(rgb(if active {
                        palette.selected
                    } else {
                        palette.panel
                    }))
                    .cursor_pointer()
                    .hover(move |tab_button| tab_button.bg(rgb(palette.hover)))
                    .child(tab.label())
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                            view.set_active_tab(tab, context);
                            context.stop_propagation();
                        }),
                    )
            }),
        )
    }

    /// 返回线程分析图例中展示的状态顺序。
    ///
    /// 业务意图：
    /// - 图例顺序需要稳定，避免用户每次打开分析窗口时过滤按钮位置变化。
    fn legend_state_kinds() -> [ThreadStateKind; 5] {
        [
            ThreadStateKind::Runnable,
            ThreadStateKind::Blocked,
            ThreadStateKind::Waiting,
            ThreadStateKind::TimedWaiting,
            ThreadStateKind::Other,
        ]
    }

    /// 返回线程分析窗口默认显示的状态集合。
    ///
    /// 业务意图：
    /// - 用户要求默认只显示 RUNNABLE 状态线程，便于优先定位正在运行或占用 CPU 的线程。
    pub(in crate::app) fn default_visible_state_kinds() -> HashSet<ThreadStateKind> {
        HashSet::from([ThreadStateKind::Runnable])
    }

    /// 切换某个线程状态是否显示。
    ///
    /// 业务意图：
    /// - 图例项既是说明也是过滤按钮；切换后重置滚动和气泡，避免旧滚动位置或旧详情指向已隐藏行。
    fn toggle_visible_state_kind(&mut self, state: ThreadStateKind, context: &mut Context<Self>) {
        if !self.visible_state_kinds.remove(&state) {
            self.visible_state_kinds.insert(state);
        }
        self.scroll_handle = UniformListScrollHandle::new();
        self.name_column_width = THREAD_ANALYSIS_NAME_COLUMN_WIDTH;
        self.scrollbar_drag = None;
        self.cell_popup = None;
        context.notify();
    }

    /// 渲染单个线程的时间线行。
    fn render_timeline_row(
        &self,
        thread_name: String,
        cells: &[Option<Arc<ThreadTimelineCell>>],
        visible_state_kinds: &HashSet<ThreadStateKind>,
        name_column_width: f32,
        palette: AppThemePalette,
        theme: EffectiveTheme,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .min_w_0()
            .h(px(30.0))
            .border_t_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .w(px(name_column_width))
                    .flex_none()
                    .pr_2()
                    .truncate()
                    .text_xs()
                    .text_color(rgb(palette.text))
                    .child(thread_name.clone()),
            )
            .children(cells.iter().enumerate().map(|(cell_index, cell)| {
                let block = if let Some(cell) = cell
                    .as_ref()
                    .filter(|cell| visible_state_kinds.contains(&cell.state))
                {
                    let color = Self::timeline_cell_fill_color(
                        cell.state,
                        self.is_jumped_timeline_cell(cell),
                        theme,
                    );
                    let cell_for_hover = Arc::clone(cell);
                    let cell_for_click = Arc::clone(cell);
                    div()
                        .id(SharedString::from(format!(
                            "thread-analysis-cell-{}-{}",
                            thread_name, cell_index
                        )))
                        .w(px(THREAD_ANALYSIS_STATE_BLOCK_SIZE))
                        .h(px(THREAD_ANALYSIS_STATE_BLOCK_SIZE))
                        .rounded(px(3.0))
                        .bg(rgb(color))
                        .cursor_pointer()
                        .hover(move |block| block.opacity(0.86))
                        .on_hover(context.listener(
                            move |view, is_hovered: &bool, window, context| {
                                view.handle_timeline_cell_hover(
                                    cell_for_hover.clone(),
                                    *is_hovered,
                                    window,
                                    context,
                                );
                            },
                        ))
                        .on_mouse_down(
                            MouseButton::Left,
                            context.listener(
                                move |view, event: &MouseDownEvent, window, context| {
                                    view.handle_timeline_cell_mouse_down(
                                        cell_for_click.clone(),
                                        event,
                                        window,
                                        context,
                                    );
                                },
                            ),
                        )
                } else {
                    div()
                        .id(SharedString::from(format!(
                            "thread-analysis-empty-cell-{}-{}",
                            thread_name, cell_index
                        )))
                        .w(px(THREAD_ANALYSIS_STATE_BLOCK_SIZE))
                        .h(px(THREAD_ANALYSIS_STATE_BLOCK_SIZE))
                        .rounded(px(3.0))
                        .bg(rgb(palette.surface))
                };
                div()
                    .w(px(THREAD_ANALYSIS_SNAPSHOT_COLUMN_WIDTH))
                    .flex_none()
                    .px_1()
                    .child(block)
            }))
    }

    /// 渲染堆栈并发分析表头。
    ///
    /// 业务意图：
    /// - 表头和虚拟列表行使用相同列宽，保证大量线程滚动时列对齐稳定。
    fn render_concurrency_table_header(&self, palette: AppThemePalette) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .min_w(px(Self::thread_concurrency_table_min_width()))
            .w_full()
            .h(px(THREAD_ANALYSIS_CONCURRENCY_ROW_HEIGHT))
            .px_4()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(palette.muted_text))
            .child(Self::render_concurrency_name_cell(
                "堆栈特征",
                palette,
                true,
            ))
            .child(Self::render_concurrency_count_cell(
                "样本数",
                THREAD_ANALYSIS_CONCURRENCY_TOTAL_COLUMN_WIDTH,
                palette,
                true,
            ))
            .child(Self::render_concurrency_count_cell(
                "最大并发",
                THREAD_ANALYSIS_CONCURRENCY_MAX_COLUMN_WIDTH,
                palette,
                true,
            ))
            .children(
                Self::thread_concurrency_state_columns()
                    .into_iter()
                    .map(|state| {
                        Self::render_concurrency_count_cell(
                            state.label(),
                            THREAD_ANALYSIS_CONCURRENCY_STATE_COLUMN_WIDTH,
                            palette,
                            true,
                        )
                    }),
            )
    }

    /// 渲染堆栈并发分析单行。
    ///
    /// 业务意图：
    /// - 行点击打开该堆栈指纹匹配到的全部原始堆栈样本详情；数字列只展示聚合结果，不改变过滤或排序语义。
    fn render_concurrency_row(
        &self,
        row: ThreadConcurrencyRow,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let stack_key_for_click = row.stack_key.clone();
        div()
            .id(SharedString::from(format!(
                "thread-analysis-concurrency-row-{}",
                row.stable_id()
            )))
            .flex()
            .items_center()
            .min_w(px(Self::thread_concurrency_table_min_width()))
            .w_full()
            .h(px(THREAD_ANALYSIS_CONCURRENCY_ROW_HEIGHT))
            .px_4()
            .border_b_1()
            .border_color(rgb(palette.border))
            .text_xs()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .child(Self::render_concurrency_name_cell(
                row.stack_title.clone(),
                palette,
                false,
            ))
            .child(Self::render_concurrency_count_cell(
                row.total_count.to_string(),
                THREAD_ANALYSIS_CONCURRENCY_TOTAL_COLUMN_WIDTH,
                palette,
                false,
            ))
            .child(Self::render_concurrency_count_cell(
                row.max_snapshot_concurrency.to_string(),
                THREAD_ANALYSIS_CONCURRENCY_MAX_COLUMN_WIDTH,
                palette,
                false,
            ))
            .children(
                Self::thread_concurrency_state_columns()
                    .into_iter()
                    .map(|state| {
                        Self::render_concurrency_count_cell(
                            row.count_for_state(state).to_string(),
                            THREAD_ANALYSIS_CONCURRENCY_STATE_COLUMN_WIDTH,
                            palette,
                            false,
                        )
                    }),
            )
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, window, context| {
                    view.handle_concurrency_row_mouse_down(
                        stack_key_for_click.clone(),
                        event,
                        window,
                        context,
                    );
                }),
            )
    }

    /// 渲染堆栈并发分析代表栈帧列。
    ///
    /// 边界条件：
    /// - 栈帧可能很长，只在单元格内截断，不允许挤压右侧统计列。
    fn render_concurrency_name_cell(
        text: impl Into<SharedString>,
        palette: AppThemePalette,
        is_header: bool,
    ) -> gpui::Div {
        div()
            .flex_1()
            .min_w(px(THREAD_ANALYSIS_CONCURRENCY_NAME_MIN_WIDTH))
            .pr_3()
            .truncate()
            .text_color(rgb(if is_header {
                palette.muted_text
            } else {
                palette.text
            }))
            .child(text.into())
    }

    /// 渲染堆栈并发分析数字列。
    ///
    /// 业务意图：
    /// - 数字右对齐便于比较同列大小，固定宽度保证虚拟列表滚动时表格不会横向抖动。
    fn render_concurrency_count_cell(
        text: impl Into<SharedString>,
        width: f32,
        palette: AppThemePalette,
        is_header: bool,
    ) -> gpui::Div {
        div()
            .w(px(width))
            .flex_none()
            .text_right()
            .font_family(LOG_VIEWER_FONT_FAMILY)
            .text_color(rgb(if is_header {
                palette.muted_text
            } else {
                palette.text
            }))
            .child(text.into())
    }

    /// 返回堆栈并发分析状态列顺序。
    ///
    /// 业务意图：
    /// - 并发页状态列需要和频率页图例保持主要状态顺序一致；OTHER 承接 NEW、TERMINATED 和未知状态。
    fn thread_concurrency_state_columns() -> [ThreadStateKind; 5] {
        [
            ThreadStateKind::Runnable,
            ThreadStateKind::Blocked,
            ThreadStateKind::Waiting,
            ThreadStateKind::TimedWaiting,
            ThreadStateKind::Other,
        ]
    }

    /// 返回堆栈并发分析表格最小宽度。
    ///
    /// 业务意图：
    /// - 表格宽度由固定状态列和堆栈摘要最小宽度组成，测试用该纯函数锁定表头和数据行的布局约束。
    pub(in crate::app) fn thread_concurrency_table_min_width() -> f32 {
        THREAD_ANALYSIS_CONCURRENCY_NAME_MIN_WIDTH
            + THREAD_ANALYSIS_CONCURRENCY_TOTAL_COLUMN_WIDTH
            + THREAD_ANALYSIS_CONCURRENCY_MAX_COLUMN_WIDTH
            + THREAD_ANALYSIS_CONCURRENCY_STATE_COLUMN_WIDTH
                * Self::thread_concurrency_state_columns().len() as f32
    }

    /// 返回线程时间线色块的填充色。
    ///
    /// 业务意图：
    /// - 普通色块使用线程状态色；最近一次点击打开详情的色块使用独立强调色，避免和状态语义混淆。
    /// - 该函数保持纯计算，便于单元测试锁定选中色不会和任一状态色冲突。
    pub(in crate::app) fn timeline_cell_fill_color(
        state: ThreadStateKind,
        is_jump_target: bool,
        theme: EffectiveTheme,
    ) -> u32 {
        if is_jump_target {
            THREAD_ANALYSIS_JUMPED_CELL_COLOR
        } else {
            state.color(theme)
        }
    }

    /// 判断指定色块是否是最近一次点击打开详情的目标。
    ///
    /// 业务意图：
    /// - 同一个线程可能跨多个快照重复出现，只比较线程名或行号容易误高亮；指针身份能精确定位矩阵中的单个色块。
    fn is_jumped_timeline_cell(&self, cell: &Arc<ThreadTimelineCell>) -> bool {
        self.jumped_cell
            .as_ref()
            .map(|jumped_cell| Arc::ptr_eq(jumped_cell, cell))
            .unwrap_or(false)
    }

    /// 处理线程分析色块悬浮状态变化。
    ///
    /// 业务意图：
    /// - 鼠标悬浮用于展示线程信息气泡，避免单击时用户还没决定是否打开详情窗口就打断阅读。
    /// - 离开色块时只关闭同一个色块打开的气泡，避免快速移动到相邻色块时旧的离开事件误关新气泡。
    fn handle_timeline_cell_hover(
        &mut self,
        cell: Arc<ThreadTimelineCell>,
        is_hovered: bool,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if is_hovered {
            let pointer = window.mouse_position();
            let (popup_x, popup_y) = Self::thread_analysis_popup_origin(
                f32::from(pointer.x),
                f32::from(pointer.y),
                f32::from(window.bounds().size.width),
                f32::from(window.bounds().size.height),
            );
            self.cell_popup = Some(ThreadAnalysisCellPopup {
                cell,
                x: popup_x,
                y: popup_y,
            });
            context.notify();
            return;
        }

        let should_close_popup = self
            .cell_popup
            .as_ref()
            .map(|popup| Arc::ptr_eq(&popup.cell, &cell))
            .unwrap_or(false);
        if should_close_popup {
            self.cell_popup = None;
            context.notify();
        }
    }

    /// 处理线程分析色块点击。
    ///
    /// 业务意图：
    /// - 单击只打开或更新堆栈详情窗口，不再自动驱动主日志窗口跳转，避免分析窗口和主窗口焦点被意外切换。
    /// - 鼠标多击会产生多次按下事件，继续按普通点击更新详情窗口，保证旧双击习惯不会失效。
    fn handle_timeline_cell_mouse_down(
        &mut self,
        cell: Arc<ThreadTimelineCell>,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if Self::timeline_cell_click_should_open_stack_window(event.click_count) {
            self.jumped_cell = Some(cell.clone());
            self.open_thread_stack_window(cell, window, context);
            context.notify();
        }
        context.stop_propagation();
    }

    /// 处理堆栈并发分析行点击。
    ///
    /// 业务意图：
    /// - 并发页按规范化后的堆栈指纹聚合，用户点击一行后应查看该堆栈匹配到的全部原始样本，并用详情窗口左右按钮切换。
    /// - 并发页不对应某一个时间线色块，因此点击后清空时间线高亮，避免把旧频率页色块误认为当前详情目标。
    fn handle_concurrency_row_mouse_down(
        &mut self,
        stack_key: String,
        event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if Self::timeline_cell_click_should_open_stack_window(event.click_count) {
            let stacks = Self::thread_stack_cells_for_stack_key(&self.analysis, &stack_key);
            if !stacks.is_empty() {
                self.jumped_cell = None;
                self.cell_popup = None;
                self.open_thread_stack_window_with_stacks(stacks, 0, context);
                context.notify();
            }
        }
        context.stop_propagation();
    }

    /// 打开或复用线程堆栈详情窗口。
    ///
    /// 业务意图：
    /// - 色块点击只弹出详情窗口展示当前线程完整堆栈，主日志打开由详情窗口右上角按钮显式触发。
    /// - 详情窗口复用同一线程在当前分析结果中的所有堆栈样本，左右按钮按时间线顺序切换。
    fn open_thread_stack_window(
        &mut self,
        cell: Arc<ThreadTimelineCell>,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let (stacks, active_index) =
            Self::thread_stack_cells_for_clicked_cell(&self.analysis, &cell);
        self.open_thread_stack_window_with_stacks(stacks, active_index, context);
    }

    /// 使用给定堆栈样本打开或复用线程堆栈详情窗口。
    ///
    /// 业务意图：
    /// - 频率页色块和并发页聚合行都需要打开同一个详情窗口；统一入口可以保证标题、窗口复用、焦点和前进后退能力一致。
    ///
    /// 边界条件：
    /// - 空样本表示没有可展示详情，直接忽略，避免创建只显示空态的新窗口误导用户。
    fn open_thread_stack_window_with_stacks(
        &mut self,
        stacks: Vec<Arc<ThreadTimelineCell>>,
        active_index: usize,
        context: &mut Context<Self>,
    ) {
        if stacks.is_empty() {
            return;
        }
        let active_index = ThreadStackWindowView::clamped_active_index(&stacks, active_index);
        let window_title = ThreadStackWindowView::window_title_for_cell(stacks.get(active_index));
        if let Some(stack_window) = self.stack_window {
            if stack_window
                .update(context, |stack_view, window, context| {
                    stack_view.update_stacks(stacks.clone(), active_index, context);
                    window.set_window_title(&window_title);
                    stack_view.focus_stack_body(window);
                    window.activate_window();
                })
                .is_ok()
            {
                return;
            }
            self.stack_window = None;
        }

        let main_view_for_window = self.main_view.clone();
        let owner_view = context.weak_entity();
        let window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(window_title.into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::centered(
                size(
                    px(THREAD_STACK_WINDOW_WIDTH),
                    px(THREAD_STACK_WINDOW_HEIGHT),
                ),
                context,
            )),
            is_resizable: true,
            is_minimizable: true,
            window_min_size: Some(size(
                px(THREAD_STACK_WINDOW_MIN_WIDTH),
                px(THREAD_STACK_WINDOW_MIN_HEIGHT),
            )),
            ..Default::default()
        };

        if let Ok(stack_window) = context.open_window(window_options, move |window, app| {
            window.on_window_should_close(app, move |_, app| {
                if let Some(owner_view) = owner_view.upgrade() {
                    owner_view.update(app, |view, context| {
                        view.stack_window = None;
                        context.notify();
                    });
                }
                true
            });
            app.new(|context| {
                ThreadStackWindowView::new(main_view_for_window, stacks, active_index, context)
            })
        }) {
            let _ = stack_window.update(context, |stack_view, window, _| {
                stack_view.focus_stack_body(window);
                window.activate_window();
            });
            self.stack_window = Some(stack_window);
        }
    }

    /// 收集点击色块所属线程的所有可切换堆栈样本。
    ///
    /// 业务意图：
    /// - 详情窗口左右按钮应在“当前线程”维度切换，而不是跨所有线程混排；这样用户从某个线程色块进入后上下文保持稳定。
    /// - 遍历矩阵时保持快照列顺序，和线程分析主窗口横轴一致。
    ///
    /// 边界条件：
    /// - 如果分析数据已经被替换或矩阵里找不到该线程，至少返回当前点击色块，保证详情窗口仍能打开。
    pub(in crate::app) fn thread_stack_cells_for_clicked_cell(
        analysis: &ThreadAnalysisData,
        clicked_cell: &Arc<ThreadTimelineCell>,
    ) -> (Vec<Arc<ThreadTimelineCell>>, usize) {
        let mut cells = Vec::new();
        if let Some(row_index) = analysis
            .thread_names
            .iter()
            .position(|thread_name| thread_name == &clicked_cell.thread_name)
            && let Some(row) = analysis.matrix.get(row_index)
        {
            cells.extend(row.iter().flatten().cloned());
        }
        if cells.is_empty() {
            cells.push(clicked_cell.clone());
        }
        let active_index = cells
            .iter()
            .position(|cell| Arc::ptr_eq(cell, clicked_cell))
            .unwrap_or_else(|| {
                cells
                    .iter()
                    .position(|cell| {
                        cell.thread_name == clicked_cell.thread_name
                            && cell.time_label == clicked_cell.time_label
                            && cell.source == clicked_cell.source
                            && cell.line_index == clicked_cell.line_index
                    })
                    .unwrap_or(0)
            });
        (cells, active_index)
    }

    /// 按堆栈指纹收集并发分析行对应的全部堆栈样本。
    ///
    /// 业务意图：
    /// - 并发分析统计包含用户过滤后全部线程样本，包括频率页默认隐藏的单次线程；详情入口也必须使用同一口径。
    /// - 遍历快照和线程样本的原始顺序，保证详情窗口前进/后退顺序和选中日志解析顺序一致。
    ///
    /// 边界条件：
    /// - 详情窗口必须保留原始线程名、行号和堆栈文本；这里只用规范化指纹做匹配，不替换用户最终看到的内容。
    pub(in crate::app) fn thread_stack_cells_for_stack_key(
        analysis: &ThreadAnalysisData,
        stack_key: &str,
    ) -> Vec<Arc<ThreadTimelineCell>> {
        analysis
            .snapshots
            .iter()
            .flat_map(|snapshot| {
                snapshot
                    .threads
                    .iter()
                    .filter(move |sample| {
                        thread_stack_fingerprint_key(&sample.stack_lines) == stack_key
                    })
                    .map(|sample| {
                        Arc::new(ThreadTimelineCell {
                            state: sample.state,
                            time_label: snapshot.label.clone(),
                            thread_name: sample.name.clone(),
                            thread_id: sample.thread_id.clone(),
                            source: snapshot.source.clone(),
                            line_index: sample.line_index,
                            preview_lines: sample.preview_lines.clone(),
                            stack_lines: sample.stack_lines.clone(),
                        })
                    })
            })
            .collect()
    }

    /// 判断线程分析色块的一次鼠标按下是否应打开堆栈详情窗口。
    ///
    /// 业务意图：
    /// - 新交互要求单击直接打开详情；GPUI 在双击时仍会继续递增点击次数，因此所有有效左键点击次数都按打开详情处理。
    /// - 点击次数为 0 只可能来自测试或异常平台事件，不能触发详情窗口，避免错误事件创建窗口。
    pub(in crate::app) fn timeline_cell_click_should_open_stack_window(click_count: usize) -> bool {
        click_count >= 1
    }

    /// 根据鼠标悬浮点和窗口尺寸计算线程信息气泡左上角。
    ///
    /// 业务意图：
    /// - 色块可能位于窗口右下角，如果始终向右下弹出会被窗口裁切；这里按剩余空间自动改为向左或向上弹出。
    /// - 计算只依赖逻辑像素，和 GPUI 布局使用的坐标体系一致，适配 macOS/Windows 缩放差异。
    ///
    /// 边界条件：
    /// - 窗口小于气泡宽高时退化为贴近边距展示，尽量保留气泡主体内容。
    pub(in crate::app) fn thread_analysis_popup_origin(
        pointer_x: f32,
        pointer_y: f32,
        window_width: f32,
        window_height: f32,
    ) -> (Pixels, Pixels) {
        let right_x = pointer_x + THREAD_ANALYSIS_POPUP_OFFSET;
        let left_x = pointer_x - THREAD_ANALYSIS_POPUP_WIDTH - THREAD_ANALYSIS_POPUP_OFFSET;
        let below_y = pointer_y + THREAD_ANALYSIS_POPUP_OFFSET;
        let above_y =
            pointer_y - THREAD_ANALYSIS_POPUP_ESTIMATED_HEIGHT - THREAD_ANALYSIS_POPUP_OFFSET;
        let max_x = (window_width - THREAD_ANALYSIS_POPUP_WIDTH - THREAD_ANALYSIS_POPUP_MARGIN)
            .max(THREAD_ANALYSIS_POPUP_MARGIN);
        let max_y =
            (window_height - THREAD_ANALYSIS_POPUP_ESTIMATED_HEIGHT - THREAD_ANALYSIS_POPUP_MARGIN)
                .max(THREAD_ANALYSIS_POPUP_MARGIN);
        let x = if right_x + THREAD_ANALYSIS_POPUP_WIDTH + THREAD_ANALYSIS_POPUP_MARGIN
            <= window_width
        {
            right_x
        } else {
            left_x
        }
        .clamp(THREAD_ANALYSIS_POPUP_MARGIN, max_x);
        let y = if below_y + THREAD_ANALYSIS_POPUP_ESTIMATED_HEIGHT + THREAD_ANALYSIS_POPUP_MARGIN
            <= window_height
        {
            below_y
        } else {
            above_y
        }
        .clamp(THREAD_ANALYSIS_POPUP_MARGIN, max_y);
        (px(x), px(y))
    }

    /// 关闭当前线程分析悬浮气泡。
    ///
    /// 业务意图：
    /// - 用户查看完某个色块后，点击分析窗口的其它位置应恢复干净时间线视图。
    /// - 正常离开色块时由悬浮事件关闭气泡；这里保留兜底，处理点击空白区域和滚动条拖动等场景。
    fn dismiss_cell_popup(&mut self, context: &mut Context<Self>) {
        if self.cell_popup.take().is_some() {
            context.notify();
        }
    }

    /// 渲染线程色块悬浮气泡。
    ///
    /// 业务意图：
    /// - 气泡展示用户悬浮色块对应的时间、完整线程名、线程 ID 和前 5 行原始日志，辅助快速确认线程上下文。
    /// - 预览文本可能很长，因此使用固定宽度和截断，避免覆盖整个分析窗口。
    fn render_cell_popup(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let popup = self.cell_popup.as_ref()?;
        let thread_id = popup
            .cell
            .thread_id
            .as_deref()
            .unwrap_or("未识别")
            .to_string();
        Some(
            div()
                .absolute()
                .left(popup.x)
                .top(popup.y)
                .w(px(THREAD_ANALYSIS_POPUP_WIDTH))
                .p_3()
                .rounded(px(6.0))
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.menu))
                .shadow_md()
                .flex()
                .flex_col()
                .gap_2()
                .text_xs()
                .text_color(rgb(palette.text))
                .on_mouse_down(
                    MouseButton::Left,
                    context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                        context.stop_propagation();
                    }),
                )
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .truncate()
                        .child(format!("时间：{}", popup.cell.time_label)),
                )
                .child(
                    div()
                        .truncate()
                        .child(format!("线程：{}", popup.cell.thread_name)),
                )
                .child(div().truncate().child(format!("线程 ID：{}", thread_id)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .pt_1()
                        .border_t_1()
                        .border_color(rgb(palette.border))
                        .children(popup.cell.preview_lines.iter().map(|line| {
                            div()
                                .truncate()
                                .font_family(LOG_VIEWER_FONT_FAMILY)
                                .text_color(rgb(palette.muted_text))
                                .child(line.clone())
                        })),
                ),
        )
    }

    /// 返回当前过滤条件下可见的线程行下标。
    ///
    /// 业务意图：
    /// - 状态过滤发生在线程行维度；只要某个线程在任一快照中出现了已勾选状态，该线程就保留在纵轴中。
    fn visible_thread_indexes(&self) -> Vec<usize> {
        Self::visible_thread_indexes_for_state_kinds(&self.analysis, &self.visible_state_kinds)
    }

    /// 根据状态集合计算可见线程行下标。
    ///
    /// 业务意图：
    /// - 拆成纯函数便于测试默认 RUNNABLE 过滤规则，避免 UI 事件和虚拟列表影响业务判断。
    pub(in crate::app) fn visible_thread_indexes_for_state_kinds(
        analysis: &ThreadAnalysisData,
        visible_state_kinds: &HashSet<ThreadStateKind>,
    ) -> Vec<usize> {
        visible_thread_indexes_for_state_kinds(analysis, visible_state_kinds)
    }

    /// 渲染线程分析纵向滚动条。
    ///
    /// 业务意图：
    /// - 虚拟列表虽然支持滚轮滚动，但线程很多时需要可见位置提示和拖动入口。
    fn render_vertical_scrollbar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) = MainView::log_vertical_scrollbar_metrics(&self.scroll_handle) else {
            return div()
                .id("thread-analysis-vertical-scrollbar-empty")
                .hidden();
        };

        div()
            .id("thread-analysis-vertical-scrollbar")
            .absolute()
            .top(metrics.thumb_start)
            .right(px(LOG_VIEWER_SCROLLBAR_PADDING))
            .w(px(LOG_VIEWER_SCROLLBAR_WIDTH))
            .h(metrics.thumb_length)
            .rounded(px(LOG_VIEWER_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(palette.scrollbar))
            .cursor_pointer()
            .hover(move |thumb| thumb.bg(rgb(palette.scrollbar_hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.start_scrollbar_drag(LogScrollbarAxis::Vertical, event, context);
                    // 线程分析滚动条覆盖在线程矩阵上，拖动时不能同时触发根节点关闭悬浮气泡。
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染线程分析横向滚动条。
    ///
    /// 业务意图：
    /// - 快照数量很多时需要横向拖动入口；轨道从线程名列右侧开始，对齐实际时间线区域。
    fn render_horizontal_scrollbar(
        &self,
        name_column_width: f32,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) =
            MainView::log_horizontal_scrollbar_metrics(&self.scroll_handle, name_column_width)
        else {
            return div()
                .id("thread-analysis-horizontal-scrollbar-empty")
                .hidden();
        };

        div()
            .id("thread-analysis-horizontal-scrollbar")
            .absolute()
            .left(metrics.thumb_start)
            .bottom(px(LOG_VIEWER_SCROLLBAR_PADDING))
            .w(metrics.thumb_length)
            .h(px(LOG_VIEWER_SCROLLBAR_WIDTH))
            .rounded(px(LOG_VIEWER_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(palette.scrollbar))
            .cursor_pointer()
            .hover(move |thumb| thumb.bg(rgb(palette.scrollbar_hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.start_scrollbar_drag(LogScrollbarAxis::Horizontal, event, context);
                    // 横向滚动条属于独立拖拽控件，按下事件需要在滑块层结束。
                    context.stop_propagation();
                }),
            )
    }

    /// 开始拖动线程分析滚动条。
    ///
    /// 边界条件：
    /// - 如果滚动条尚未完成测量或内容不足以滚动，则忽略本次拖动。
    fn start_scrollbar_drag(
        &mut self,
        axis: LogScrollbarAxis,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let Some(metrics) = self.scrollbar_metrics(axis) else {
            return;
        };
        if metrics.max_scroll <= px(0.0) {
            return;
        }
        let Some(viewport_origin) =
            MainView::uniform_list_viewport_axis_origin(&self.scroll_handle, axis)
        else {
            return;
        };
        let pointer_position = match axis {
            LogScrollbarAxis::Vertical => event.position.y,
            LogScrollbarAxis::Horizontal => event.position.x,
        };
        self.scrollbar_drag = Some(ThreadAnalysisScrollbarDrag {
            axis,
            cursor_offset: pointer_position - viewport_origin - metrics.thumb_start,
        });
        context.notify();
    }

    /// 根据鼠标移动更新线程分析滚动条拖动。
    fn update_scrollbar_drag(&mut self, event: &MouseMoveEvent, context: &mut Context<Self>) {
        let Some(drag) = self.scrollbar_drag else {
            return;
        };
        if !event.dragging() {
            self.scrollbar_drag = None;
            context.notify();
            return;
        }
        let Some(metrics) = self.scrollbar_metrics(drag.axis) else {
            self.scrollbar_drag = None;
            context.notify();
            return;
        };
        let Some(viewport_origin) =
            MainView::uniform_list_viewport_axis_origin(&self.scroll_handle, drag.axis)
        else {
            self.scrollbar_drag = None;
            context.notify();
            return;
        };
        let pointer_position = match drag.axis {
            LogScrollbarAxis::Vertical => event.position.y,
            LogScrollbarAxis::Horizontal => event.position.x,
        };
        let movable_length = (metrics.track_length - metrics.thumb_length).max(px(0.0));
        if metrics.max_scroll <= px(0.0) || movable_length <= px(0.0) {
            return;
        }

        let requested_thumb_start = pointer_position - viewport_origin - drag.cursor_offset;
        let thumb_start =
            requested_thumb_start.clamp(metrics.track_start, metrics.track_start + movable_length);
        let scroll_offset =
            metrics.max_scroll * ((thumb_start - metrics.track_start) / movable_length);
        let base_scroll_handle = { self.scroll_handle.0.borrow().base_handle.clone() };
        let current_offset = base_scroll_handle.offset();
        match drag.axis {
            LogScrollbarAxis::Vertical => {
                base_scroll_handle.set_offset(point(current_offset.x, -scroll_offset));
            }
            LogScrollbarAxis::Horizontal => {
                base_scroll_handle.set_offset(point(-scroll_offset, current_offset.y));
            }
        }
        context.notify();
    }

    /// 结束线程分析滚动条拖动。
    fn stop_scrollbar_drag(&mut self, context: &mut Context<Self>) {
        if self.scrollbar_drag.take().is_some() {
            context.notify();
        }
    }

    /// 返回指定方向的线程分析滚动条测量结果。
    fn scrollbar_metrics(&self, axis: LogScrollbarAxis) -> Option<LogScrollbarMetrics> {
        match axis {
            LogScrollbarAxis::Vertical => {
                MainView::log_vertical_scrollbar_metrics(&self.scroll_handle)
            }
            LogScrollbarAxis::Horizontal => MainView::log_horizontal_scrollbar_metrics(
                &self.scroll_handle,
                self.name_column_width,
            ),
        }
    }

    /// 渲染线程频率分析内容区。
    ///
    /// 业务意图：
    /// - 该页签完全承载旧版分析结果：按线程名和快照组成时间线矩阵，并继续使用虚拟列表与自绘滚动条处理大结果集。
    fn render_frequency_content(
        &self,
        visible_thread_indexes: Vec<usize>,
        visible_state_kinds: HashSet<ThreadStateKind>,
        name_column_width: f32,
        palette: AppThemePalette,
        theme: EffectiveTheme,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let row_count = visible_thread_indexes.len();
        let scroll_handle = self.scroll_handle.clone();
        div()
            .id("thread-analysis-frequency-scroll")
            .relative()
            .flex_1()
            .p_4()
            .child(
                uniform_list(
                    "thread-analysis-virtual-list",
                    row_count,
                    context.processor(
                        move |view, range: std::ops::Range<usize>, _window, _context| {
                            range
                                .map(|row_index| {
                                    let thread_name = view
                                        .analysis
                                        .thread_names
                                        .get(
                                            visible_thread_indexes
                                                .get(row_index)
                                                .copied()
                                                .unwrap_or(usize::MAX),
                                        )
                                        .cloned()
                                        .unwrap_or_default();
                                    let cells = view
                                        .analysis
                                        .matrix
                                        .get(
                                            visible_thread_indexes
                                                .get(row_index)
                                                .copied()
                                                .unwrap_or(usize::MAX),
                                        )
                                        .map(Vec::as_slice)
                                        .unwrap_or(&[]);
                                    view.render_timeline_row(
                                        thread_name,
                                        cells,
                                        &visible_state_kinds,
                                        name_column_width,
                                        palette,
                                        theme,
                                        _context,
                                    )
                                })
                                .collect::<Vec<_>>()
                        },
                    ),
                )
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .size_full()
                .track_scroll(scroll_handle),
            )
            .child(self.render_vertical_scrollbar(palette, context))
            .child(self.render_horizontal_scrollbar(name_column_width, palette, context))
    }

    /// 渲染堆栈并发分析内容区。
    ///
    /// 业务意图：
    /// - 并发页按堆栈指纹聚合展示，不受频率页状态图例影响；行数可能很大，因此正文仍使用 `uniform_list` 虚拟渲染。
    fn render_concurrency_content(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let row_count = self.analysis.concurrency_rows.len();
        if row_count == 0 {
            return div()
                .id("thread-analysis-concurrency-empty")
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .text_sm()
                .text_color(rgb(palette.muted_text))
                .child("没有可展示的堆栈并发数据");
        }

        let scroll_handle = self.concurrency_scroll_handle.clone();
        div()
            .id("thread-analysis-concurrency-table")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(self.render_concurrency_table_header(palette))
            .child(
                div().relative().flex_1().min_h_0().child(
                    uniform_list(
                        "thread-analysis-concurrency-list",
                        row_count,
                        context.processor(
                            move |view, range: std::ops::Range<usize>, _window, context| {
                                range
                                    .filter_map(|row_index| {
                                        view.analysis.concurrency_rows.get(row_index).cloned().map(
                                            |row| {
                                                view.render_concurrency_row(row, palette, context)
                                            },
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            },
                        ),
                    )
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .size_full()
                    .track_scroll(scroll_handle),
                ),
            )
    }

    /// 返回当前窗口中可见线程名列应使用的宽度。
    ///
    /// 业务意图：
    /// - 线程 dump 的线程名经常包含线程池编号、HTTP 连接地址和业务资源名，固定 260px 会在结果首帧截断关键信息。
    /// - 列宽只根据当前过滤后仍可见的最长线程名估算，不再把右侧空白全部吞掉；这样既能显示完整线程名，也不会人为制造大段空白。
    ///
    /// 边界条件：
    /// - 首帧不依赖滚动容器测量，因此窗口刚打开时就能得到和后续交互一致的宽度。
    /// - 如果最长线程名超过当前视口，仍优先给出完整线程名所需宽度，时间线区域通过已有横向滚动查看。
    fn thread_analysis_name_column_width_for_visible_threads(
        &self,
        visible_thread_indexes: &[usize],
    ) -> f32 {
        let required_width = visible_thread_indexes
            .iter()
            .filter_map(|thread_index| self.analysis.thread_names.get(*thread_index))
            .map(|thread_name| Self::thread_analysis_name_column_required_width(thread_name))
            .fold(THREAD_ANALYSIS_NAME_COLUMN_WIDTH, f32::max);

        Self::thread_analysis_name_column_width_for_required_width(required_width)
    }

    /// 根据已估算出的线程名内容宽度计算最终列宽。
    ///
    /// 业务意图：
    /// - 单元测试只需要验证“至少保留默认宽度”和“长线程名只扩到内容所需宽度”两条规则，不依赖 GPUI 运行时测量。
    pub(in crate::app) fn thread_analysis_name_column_width_for_required_width(
        required_width: f32,
    ) -> f32 {
        required_width.max(THREAD_ANALYSIS_NAME_COLUMN_WIDTH)
    }

    /// 估算单个线程名完整展示所需的列宽。
    ///
    /// 业务意图：
    /// - 线程名列的宽度必须在虚拟列表生成行之前确定，不能等到每一行真实绘制后再反馈布局，否则会出现首帧截断、点击后才扩展的问题。
    /// - 使用显示列数估算可以覆盖 ASCII、制表符和常见中文线程名，且计算成本只与可见线程数和线程名长度相关。
    ///
    /// 边界条件：
    /// - 空线程名仍返回右侧安全余量，最终列宽会由默认最小宽度兜底。
    /// - 制表符按 4 列处理，避免日志中异常线程名包含制表符时估算过窄。
    pub(in crate::app) fn thread_analysis_name_column_required_width(thread_name: &str) -> f32 {
        THREAD_ANALYSIS_NAME_COLUMN_TEXT_PADDING
            + Self::thread_analysis_thread_name_display_columns(thread_name) as f32
                * THREAD_ANALYSIS_NAME_COLUMN_ESTIMATED_CHAR_WIDTH
    }

    /// 返回线程名用于布局估算的显示列数。
    ///
    /// 业务意图：
    /// - 线程名通常是 ASCII，但日志来源不可控；中文或全角字符如果仍按 1 列估算，在 Windows 和 macOS 字体下都容易继续被截断。
    /// - 该函数只用于 UI 宽度估算，不改变线程名原文和过滤语义。
    fn thread_analysis_thread_name_display_columns(thread_name: &str) -> usize {
        thread_name
            .chars()
            .map(|character| {
                if character == '\t' {
                    4
                } else if character.is_ascii() {
                    1
                } else {
                    2
                }
            })
            .sum()
    }
}

impl Render for ThreadAnalysisWindowView {
    /// 渲染线程日志分析窗口。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let (palette, theme) = {
            let main_view = self.main_view.read(context);
            (main_view.palette(), main_view.effective_theme())
        };
        let visible_thread_indexes = self.visible_thread_indexes();
        let visible_state_kinds = self.visible_state_kinds.clone();
        let name_column_width =
            self.thread_analysis_name_column_width_for_visible_threads(&visible_thread_indexes);
        if self.active_tab == ThreadAnalysisResultTab::Frequency {
            self.name_column_width = name_column_width;
        }

        div()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.dismiss_cell_popup(context);
                }),
            )
            .on_mouse_move(
                context.listener(|view, event: &MouseMoveEvent, _window, context| {
                    view.update_scrollbar_drag(event, context);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    view.stop_scrollbar_drag(context);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    view.stop_scrollbar_drag(context);
                }),
            )
            .flex()
            .flex_col()
            .relative()
            .size_full()
            .bg(rgb(palette.background))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .min_h(px(54.0))
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.panel))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_4()
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
                                            .child(self.analysis.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(palette.muted_text))
                                            .truncate()
                                            .child(self.analysis.summary.clone()),
                                    ),
                            )
                            .when(
                                self.active_tab == ThreadAnalysisResultTab::Frequency,
                                |row| row.child(self.render_legend(palette, theme, context)),
                            ),
                    )
                    .child(self.render_result_tabs(palette, context)),
            )
            .child(
                match self.active_tab {
                    ThreadAnalysisResultTab::Frequency => self.render_frequency_content(
                        visible_thread_indexes,
                        visible_state_kinds,
                        name_column_width,
                        palette,
                        theme,
                        context,
                    ),
                    ThreadAnalysisResultTab::Concurrency => {
                        self.render_concurrency_content(palette, context)
                    }
                }
                .when_some(self.analysis.progress.as_ref(), |content, progress| {
                    content.child(self.render_progress_bar(progress, palette))
                }),
            )
            .when(self.cell_popup.is_some(), |root| {
                root.child(self.render_cell_popup(palette, context).unwrap_or_else(div))
            })
    }
}
