// 线程日志分析独立窗口与数据模型。
//
// 业务意图：
// - 该文件从历史 `app.rs` 中拆出，集中维护 Java thread dump 时间线窗口、状态过滤、滚动条和悬浮气泡。
// - 当前作为 `app/ui` 的窗口视图模块运行，通过显式 `pub(in crate::app)` 接口和主视图协作。
//
// 边界条件：
// - 本阶段必须保持行为不变；纯解析逻辑继续留在顶层 `thread_analysis` 业务域，窗口渲染留在本 UI 模块。

use super::*;

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
    /// 线程时间线虚拟列表滚动句柄。
    ///
    /// 业务意图：
    /// - Java thread dump 可能包含数千个线程，不能一次性把所有线程行都创建成 GPUI 元素。
    /// - 使用 `uniform_list` 只渲染可见行，并通过该句柄保存纵向和横向滚动位置。
    pub(in crate::app) scroll_handle: UniformListScrollHandle,
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
            scroll_handle: UniformListScrollHandle::new(),
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
            self.scroll_handle = UniformListScrollHandle::new();
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
                    .w(px(THREAD_ANALYSIS_NAME_COLUMN_WIDTH))
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
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) = MainView::log_horizontal_scrollbar_metrics(
            &self.scroll_handle,
            THREAD_ANALYSIS_NAME_COLUMN_WIDTH,
        ) else {
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
                THREAD_ANALYSIS_NAME_COLUMN_WIDTH,
            ),
        }
    }
}

impl Render for ThreadAnalysisWindowView {
    /// 渲染线程日志分析窗口。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let (palette, theme) = {
            let main_view = self.main_view.read(context);
            (main_view.palette(), main_view.effective_theme())
        };
        let _snapshot_count = self.analysis.snapshots.len();
        let visible_thread_indexes = self.visible_thread_indexes();
        let row_count = visible_thread_indexes.len();
        let visible_state_kinds = self.visible_state_kinds.clone();
        let scroll_handle = self.scroll_handle.clone();

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
                            .child(self.render_legend(palette, theme, context)),
                    ),
            )
            .child(
                div()
                    .id("thread-analysis-scroll")
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
                                                palette,
                                                theme,
                                                _context,
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                },
                            ),
                        )
                        .with_horizontal_sizing_behavior(
                            ListHorizontalSizingBehavior::Unconstrained,
                        )
                        .size_full()
                        .track_scroll(scroll_handle),
                    )
                    .child(self.render_vertical_scrollbar(palette, context))
                    .child(self.render_horizontal_scrollbar(palette, context))
                    .when_some(self.analysis.progress.as_ref(), |content, progress| {
                        content.child(self.render_progress_bar(progress, palette))
                    }),
            )
            .when(self.cell_popup.is_some(), |root| {
                root.child(self.render_cell_popup(palette, context).unwrap_or_else(div))
            })
    }
}
