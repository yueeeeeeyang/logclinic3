// 搜索独立窗口和自绘搜索输入框实现。
//
// 业务意图：
// - 该文件集中维护搜索窗口、关键字/目录输入框、计数按钮、搜索按钮和 GPUI 文本命中绘制逻辑。
// - 当前作为 `app` 的子模块运行，通过显式 `pub(super)` 接口访问主窗口搜索状态。
//
// 边界条件：
// - 搜索关键字历史只保存在当前会话内；该窗口只提供下拉选择入口，不把关键字持久化到配置目录。

use super::*;

pub(super) struct SearchDialogWindowView {
    /// 主窗口视图实体。
    ///
    /// 实现原因：
    /// - 搜索窗口需要读取和修改主窗口中的搜索状态，同时保持结果面板仍由主窗口渲染。
    main_view: Entity<MainView>,
    /// 主窗口状态变更订阅。
    ///
    /// 业务意图：
    /// - 后台目录搜索会持续更新进度；搜索窗口必须跟随 `MainView::notify` 重绘，否则进度文案会停留在旧值。
    /// - 订阅句柄必须保存在视图中，避免创建后立即释放导致观察失效。
    _main_view_subscription: gpui::Subscription,
}

impl SearchDialogWindowView {
    /// 创建搜索对话框窗口根视图。
    pub(super) fn new(main_view: Entity<MainView>, context: &mut Context<Self>) -> Self {
        let observed_main_view = main_view.clone();
        let main_view_subscription = context.observe(&observed_main_view, |_, _, context| {
            context.notify();
        });

        Self {
            main_view,
            _main_view_subscription: main_view_subscription,
        }
    }

    /// 关闭搜索对话框窗口。
    ///
    /// 业务意图：
    /// - 关闭按钮和 `Esc` 都应复用主视图的搜索取消逻辑，保证后台任务、历史记录和窗口句柄状态一致。
    pub(super) fn close_dialog(&mut self, window: &mut Window, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            view.close_search_dialog(window, context);
        });
        context.notify();
    }

    /// 开始拖动搜索窗口。
    ///
    /// 业务意图：
    /// - 搜索对话框现在是独立窗口，拖动应交给平台窗口系统处理，而不是在主窗口里维护浮层坐标。
    /// - 这样鼠标移动不会再触发主窗口日志行 hover 或选择状态。
    /// - Windows 隐藏标题栏窗口依赖 `WindowControlArea::Drag` 参与原生命中测试；这里保留显式调用作为其它平台的兜底路径。
    pub(super) fn start_window_drag(&mut self, window: &mut Window, context: &mut Context<Self>) {
        window.start_window_move();
        self.main_view.update(context, |view, context| {
            view.stop_log_text_selection(context);
            view.tab_context_menu = None;
            view.encoding_dropdown_menu = None;
            view.search_results_context_menu = None;
            view.log_viewer_context_menu = None;
            context.notify();
        });
    }

    /// 把搜索输入框按键转发给主视图状态。
    pub(super) fn handle_search_input_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.handle_search_text_key_down(SearchTextInputKind::Query, event, context);
        });
        context.notify();
    }

    /// 把目录目标输入框按键转发给主视图状态。
    pub(super) fn handle_search_directory_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.handle_search_text_key_down(SearchTextInputKind::DirectoryTarget, event, context);
        });
        context.notify();
    }

    /// 切换搜索范围。
    pub(super) fn select_search_scope(
        &mut self,
        scope: SearchScope,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let focus_handle = self.main_view.update(context, |view, context| {
            let default_directory_target = (scope == SearchScope::CurrentDirectory)
                .then(|| view.active_search_directory_label())
                .flatten();
            if let Some(dialog) = view.search_dialog.as_mut() {
                dialog.scope = scope;
                dialog.query_history_menu_open = false;
                if let Some(target) = default_directory_target
                    && dialog.directory_target.trim().is_empty()
                {
                    dialog.directory_target = target;
                }
                if scope == SearchScope::CurrentDirectory {
                    let cursor = dialog.directory_target.len();
                    dialog.directory_selection_range = cursor..cursor;
                    dialog.directory_marked_range = None;
                }
                dialog.message = "输入关键字后按 Enter 或点击搜索".to_string();
            }
            context.notify();
            if scope == SearchScope::CurrentDirectory {
                view.search_directory_focus.clone()
            } else {
                view.search_input_focus.clone()
            }
        });
        window.focus(&focus_handle);
        context.notify();
    }

    /// 切换大小写匹配选项。
    pub(super) fn toggle_case_sensitive(&mut self, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            if let Some(dialog) = view.search_dialog.as_mut() {
                if dialog.match_mode == SearchMatchMode::Regex {
                    dialog.query_history_menu_open = false;
                    context.notify();
                    return;
                }
                dialog.case_sensitive = !dialog.case_sensitive;
                dialog.query_history_menu_open = false;
                dialog.current_file_match_count = None;
            }
            context.notify();
        });
        context.notify();
    }

    /// 切换普通搜索匹配模式。
    ///
    /// 业务意图：
    /// - 正则开关只影响普通搜索和当前文件计数，不影响快搜；状态仍保存在主窗口搜索对话框里。
    pub(super) fn toggle_regex_mode(&mut self, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            if let Some(dialog) = view.search_dialog.as_mut() {
                MainView::set_search_dialog_match_mode(dialog, dialog.match_mode.toggled());
            }
            context.notify();
        });
        context.notify();
    }

    /// 统计当前关键字在当前文件中的出现次数。
    pub(super) fn count_current_file_matches(&mut self, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            if let Some(dialog) = view.search_dialog.as_mut() {
                dialog.query_history_menu_open = false;
            }
            view.count_search_query_in_current_file(context);
        });
        context.notify();
    }

    /// 启动搜索任务。
    pub(super) fn start_search(&mut self, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            view.start_search(context);
        });
        context.notify();
    }

    /// 启动快搜任务。
    pub(super) fn start_quick_search(&mut self, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            view.start_quick_search(context);
        });
        context.notify();
    }

    /// 停止当前搜索任务。
    ///
    /// 业务意图：
    /// - 搜索按钮在任务运行中会切换成停止按钮；点击后只中断后台搜索，保留搜索窗口和用户输入。
    pub(super) fn stop_search(&mut self, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            view.stop_current_search(context);
        });
        context.notify();
    }

    /// 处理搜索关键字输入框鼠标按下。
    ///
    /// 业务意图：
    /// - GPUI 当前版本的文本输入能力通过自定义元素注册到平台输入协议；鼠标事件仍需要回写到业务状态。
    /// - 单击定位光标，双击选中当前词，三连击选中整段输入，符合常见系统文本框习惯。
    pub(super) fn handle_query_input_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let focus_handle = self.main_view.update(context, |view, context| {
            view.start_search_text_mouse_selection(SearchTextInputKind::Query, event, context);
            view.search_input_focus.clone()
        });
        window.focus(&focus_handle);
        context.notify();
    }

    /// 处理目录目标输入框鼠标按下。
    pub(super) fn handle_directory_input_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let focus_handle = self.main_view.update(context, |view, context| {
            view.start_search_text_mouse_selection(
                SearchTextInputKind::DirectoryTarget,
                event,
                context,
            );
            view.search_directory_focus.clone()
        });
        window.focus(&focus_handle);
        context.notify();
    }

    /// 拖动扩展当前搜索文本输入框的选择范围。
    pub(super) fn handle_search_text_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.update_search_text_mouse_selection(event.position, context);
        });
        context.notify();
    }

    /// 结束搜索文本输入框的鼠标选择。
    pub(super) fn handle_search_text_mouse_up(&mut self, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            view.finish_search_text_mouse_selection(context);
        });
        context.notify();
    }

    /// 切换搜索关键字历史下拉菜单。
    ///
    /// 业务意图：
    /// - 用户可以从最近 10 次搜索关键字中快速恢复查询条件；历史为空时按钮不展开菜单。
    /// - 展开后焦点仍回到关键字输入框，方便用户选择历史项后继续键盘编辑。
    pub(super) fn toggle_search_history_menu(
        &mut self,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let focus_handle = self.main_view.update(context, |view, context| {
            let has_history = !view.search_query_history.is_empty();
            if let Some(dialog) = view.search_dialog.as_mut() {
                dialog.query_history_menu_open = has_history && !dialog.query_history_menu_open;
            }
            context.notify();
            view.search_input_focus.clone()
        });
        window.focus(&focus_handle);
        context.notify();
    }

    /// 关闭搜索关键字历史下拉菜单。
    ///
    /// 业务意图：
    /// - 点击搜索窗口中除历史按钮和历史项外的区域时，应收起菜单，避免它持续遮挡范围和选项控件。
    pub(super) fn dismiss_search_history_menu(&mut self, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            if let Some(dialog) = view.search_dialog.as_mut()
                && dialog.query_history_menu_open
            {
                dialog.query_history_menu_open = false;
                context.notify();
            }
        });
        context.notify();
    }

    /// 选择一个历史搜索关键字。
    ///
    /// 业务意图：
    /// - 选择历史项只替换关键字，不立即搜索，让用户仍可调整搜索范围、大小写和目录目标。
    pub(super) fn select_search_history_query(
        &mut self,
        item: SearchQueryHistoryItem,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let focus_handle = self.main_view.update(context, |view, context| {
            if let Some(dialog) = view.search_dialog.as_mut() {
                MainView::apply_search_history_query(dialog, &item);
            }
            context.notify();
            view.search_input_focus.clone()
        });
        window.focus(&focus_handle);
        context.notify();
    }

    /// 渲染搜索窗口标题栏。
    fn render_header(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("search-dialog-window-header")
            .flex()
            .items_center()
            .justify_between()
            .h(px(34.0))
            .px_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .h_full()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .cursor_move()
                    .window_control_area(gpui::WindowControlArea::Drag)
                    // 只把标题文本到关闭按钮左侧的区域作为窗口拖拽区，避免 Windows 原生命中测试把关闭按钮误判成标题栏。
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseDownEvent, window, context| {
                            view.start_window_drag(window, context);
                        }),
                    )
                    .child(MainView::render_lucide_icon(
                        Some(Icon::Search),
                        15.0,
                        15.0,
                        palette.muted_text,
                    ))
                    .child("搜索"),
            )
            .child(
                div()
                    .id("search-dialog-window-close")
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(22.0))
                    .h(px(22.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
                    .child(MainView::render_lucide_icon(
                        Some(Icon::X),
                        13.0,
                        13.0,
                        palette.muted_text,
                    ))
                    .on_click(
                        context.listener(|view, _event: &ClickEvent, window, context| {
                            view.close_dialog(window, context);
                        }),
                    ),
            )
    }

    /// 渲染搜索关键字输入框。
    fn render_search_input(
        &self,
        _dialog: &SearchDialogState,
        search_query_history: &[SearchQueryHistoryItem],
        focus_handle: gpui::FocusHandle,
        _window: &Window,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let has_history = !search_query_history.is_empty();
        div()
            .id("search-dialog-window-input")
            .relative()
            .flex()
            .items_center()
            .h(px(SEARCH_INPUT_HEIGHT))
            .w_full()
            .px_2()
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.input))
            .track_focus(&focus_handle)
            .key_context("search-input")
            .on_key_down(context.listener(Self::handle_search_input_key_down))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, window, context| {
                    view.handle_query_input_mouse_down(event, window, context);
                }),
            )
            .on_mouse_move(
                context.listener(|view, event: &MouseMoveEvent, _window, context| {
                    view.handle_search_text_mouse_move(event, context);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    view.handle_search_text_mouse_up(context);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    view.handle_search_text_mouse_up(context);
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .h_full()
                    .w_full()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .line_height(px(20.0))
                            .text_size(px(14.0))
                            .text_color(rgb(palette.text))
                            .child(SearchTextInputElement {
                                view: self.main_view.clone(),
                                input_kind: SearchTextInputKind::Query,
                                focus_handle,
                                placeholder: "输入搜索关键字",
                                palette,
                            }),
                    )
                    .child(self.render_search_history_button(has_history, palette, context)),
            )
    }

    /// 渲染搜索关键字历史下拉按钮。
    ///
    /// 业务意图：
    /// - 按钮使用下拉箭头而不是文字，减少搜索框内占用空间，并符合“选择历史项”的控件语义。
    fn render_search_history_button(
        &self,
        enabled: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("search-dialog-window-history-button")
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .w(px(SEARCH_HISTORY_DROPDOWN_BUTTON_WIDTH))
            .h(px(24.0))
            .rounded(px(4.0))
            .text_color(rgb(if enabled {
                palette.text
            } else {
                palette.muted_text
            }))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
            })
            .when(!enabled, |button| button.opacity(0.45))
            .child(MainView::render_lucide_icon(
                Some(Icon::ChevronDown),
                13.0,
                13.0,
                if enabled {
                    palette.muted_text
                } else {
                    palette.border
                },
            ))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, window, context| {
                    if enabled {
                        view.toggle_search_history_menu(window, context);
                    }
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染搜索关键字历史下拉菜单。
    ///
    /// 业务意图：
    /// - 菜单贴在关键字输入框下方，按最近使用顺序展示历史关键字，点击任一项后填入输入框。
    /// - 历史最多 10 条；菜单设置最大高度和纵向滚动，避免窗口高度受历史数量影响。
    fn render_search_history_dropdown(
        &self,
        search_query_history: &[SearchQueryHistoryItem],
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let menu_height = (search_query_history.len() as f32 * SEARCH_HISTORY_DROPDOWN_ITEM_HEIGHT
            + 8.0)
            .min(SEARCH_HISTORY_DROPDOWN_MAX_HEIGHT);

        div()
            .id("search-dialog-window-history-menu")
            .absolute()
            .left(px(SEARCH_DIALOG_CONTENT_PADDING))
            .right(px(SEARCH_DIALOG_CONTENT_PADDING))
            .top(px(SEARCH_DIALOG_CONTENT_PADDING
                + SEARCH_INPUT_HEIGHT
                + SEARCH_HISTORY_DROPDOWN_GAP))
            .h(px(menu_height))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 历史菜单是搜索窗口内的浮层，点到滚动区域或内边距时不能冒泡到窗口根节点导致误关闭。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 右键同样需要停留在历史菜单层，避免后续新增父级右键行为时形成透传。
                    context.stop_propagation();
                }),
            )
            .children(
                search_query_history
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        self.render_search_history_item(index, item, palette, context)
                    })
                    .collect::<Vec<_>>(),
            )
    }

    /// 渲染单个搜索关键字历史项。
    fn render_search_history_item(
        &self,
        index: usize,
        item: &SearchQueryHistoryItem,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let item = item.clone();
        let query = item.query.clone();
        let is_regex = item.match_mode == SearchMatchMode::Regex;
        div()
            .id(SharedString::from(format!(
                "search-dialog-window-history-item-{index}"
            )))
            .flex()
            .items_center()
            .h(px(SEARCH_HISTORY_DROPDOWN_ITEM_HEIGHT))
            .px_2()
            .text_xs()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |item| item.bg(rgb(palette.hover)).text_color(rgb(palette.accent)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(LOG_VIEWER_FONT_FAMILY)
                    .child(query.clone()),
            )
            .when(is_regex, |row| {
                row.child(
                    div()
                        .flex_none()
                        .ml_2()
                        .px_1()
                        .rounded(px(3.0))
                        .bg(rgb(palette.selected))
                        .text_color(rgb(palette.accent))
                        .child("正则"),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, window, context| {
                    view.select_search_history_query(item.clone(), window, context);
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染搜索范围切换控件。
    fn render_scope_controls(
        &self,
        selected_scope: SearchScope,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.render_scope_button(
                SearchScope::CurrentFile,
                selected_scope,
                palette,
                context,
            ))
            .child(self.render_scope_button(
                SearchScope::CurrentDirectory,
                selected_scope,
                palette,
                context,
            ))
    }

    /// 渲染单个搜索范围按钮。
    fn render_scope_button(
        &self,
        scope: SearchScope,
        selected_scope: SearchScope,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = scope == selected_scope;
        div()
            .id(SharedString::from(format!(
                "search-dialog-window-scope-{}",
                scope.label()
            )))
            .flex()
            .items_center()
            .justify_center()
            .h(px(26.0))
            .px_2()
            .rounded(px(4.0))
            .text_xs()
            .text_color(rgb(if selected {
                palette.accent
            } else {
                palette.muted_text
            }))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.panel
            }))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(scope.label())
            .on_click(
                context.listener(move |view, _event: &ClickEvent, window, context| {
                    view.select_search_scope(scope, window, context);
                }),
            )
    }

    /// 渲染当前目录搜索目标输入区域。
    fn render_directory_target(
        &self,
        _dialog: &SearchDialogState,
        focus_handle: gpui::FocusHandle,
        _window: &Window,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("search-dialog-window-directory")
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted_text))
                    .child("搜索目录"),
            )
            .child(
                div()
                    .id("search-dialog-window-directory-input")
                    .relative()
                    .flex()
                    .items_center()
                    .h(px(SEARCH_INPUT_HEIGHT))
                    .w_full()
                    .px_2()
                    .rounded(px(5.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.input))
                    .track_focus(&focus_handle)
                    .key_context("search-directory-input")
                    .on_key_down(context.listener(Self::handle_search_directory_key_down))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, event: &MouseDownEvent, window, context| {
                            view.handle_directory_input_mouse_down(event, window, context);
                        }),
                    )
                    .on_mouse_move(context.listener(
                        |view, event: &MouseMoveEvent, _window, context| {
                            view.handle_search_text_mouse_move(event, context);
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.handle_search_text_mouse_up(context);
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.handle_search_text_mouse_up(context);
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .h_full()
                            .w_full()
                            .min_w_0()
                            .overflow_hidden()
                            .line_height(px(18.0))
                            .text_size(px(12.0))
                            .text_color(rgb(palette.text))
                            .child(SearchTextInputElement {
                                view: self.main_view.clone(),
                                input_kind: SearchTextInputKind::DirectoryTarget,
                                focus_handle,
                                placeholder: "输入目录路径或子目录关键字",
                                palette,
                            }),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted_text))
                    .child("仅在已加载目录树内过滤，不会额外扫描磁盘"),
            )
    }

    /// 渲染匹配选项。
    fn render_options_row(
        &self,
        case_sensitive: bool,
        match_mode: SearchMatchMode,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        let regex_enabled = match_mode == SearchMatchMode::Regex;
        let case_toggle_enabled = !regex_enabled;
        div()
            .flex()
            .items_center()
            .justify_start()
            .gap_4()
            .child(
                div()
                    .id("search-dialog-window-case-sensitive-toggle")
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(if case_toggle_enabled {
                        palette.muted_text
                    } else {
                        palette.border
                    }))
                    .when(case_toggle_enabled, |toggle| toggle.cursor_pointer())
                    .when(!case_toggle_enabled, |toggle| toggle.opacity(0.6))
                    .child(MainView::render_checkbox(case_sensitive, palette))
                    .child("区分大小写")
                    .on_click(
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.toggle_case_sensitive(context);
                        }),
                    ),
            )
            .child(
                div()
                    .id("search-dialog-window-regex-toggle")
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(if regex_enabled {
                        palette.accent
                    } else {
                        palette.muted_text
                    }))
                    .cursor_pointer()
                    .child(MainView::render_checkbox(regex_enabled, palette))
                    .child("正则")
                    .on_click(
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.toggle_regex_mode(context);
                        }),
                    ),
            )
    }

    /// 渲染搜索窗口右下角操作按钮。
    ///
    /// 业务意图：
    /// - 计数和搜索都是执行类动作，放到窗口右下角更符合常见对话框布局。
    /// - 计数按钮文案固定为“计数”，计数结果只写入状态提示，避免按钮宽度随结果变化导致布局跳动。
    fn render_action_buttons(
        &self,
        can_search: bool,
        can_quick_search: bool,
        can_count: bool,
        is_searching: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        let submit_enabled = is_searching || can_search;
        let submit_label = if is_searching { "停止" } else { "搜索" };
        let submit_icon = if is_searching { Icon::X } else { Icon::Search };
        let submit_background = if submit_enabled {
            if is_searching {
                palette.error
            } else {
                palette.accent
            }
        } else {
            palette.muted_text
        };
        let submit_hover_background = if is_searching {
            palette.error
        } else {
            palette.accent_hover
        };

        div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .child(
                div()
                    .id("search-dialog-window-count-current-file")
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .h(px(28.0))
                    .px_3()
                    .rounded(px(5.0))
                    .text_xs()
                    .text_color(rgb(if can_count {
                        palette.accent
                    } else {
                        palette.muted_text
                    }))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.panel))
                    .when(can_count, |button| {
                        button
                            .cursor_pointer()
                            .hover(move |button| button.bg(rgb(palette.hover)))
                    })
                    .when(!can_count, |button| button.opacity(0.72))
                    .child(MainView::render_lucide_icon(
                        Some(Icon::Search),
                        12.0,
                        12.0,
                        if can_count {
                            palette.accent
                        } else {
                            palette.muted_text
                        },
                    ))
                    .child("计数")
                    .on_click(context.listener(
                        move |view, _event: &ClickEvent, _window, context| {
                            if can_count {
                                view.count_current_file_matches(context);
                            }
                            context.stop_propagation();
                        },
                    )),
            )
            .child(
                div()
                    .id("search-dialog-window-quick-search")
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .h(px(28.0))
                    .px_3()
                    .rounded(px(5.0))
                    .text_xs()
                    .text_color(rgb(if can_quick_search {
                        palette.accent
                    } else {
                        palette.muted_text
                    }))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.panel))
                    .when(can_quick_search, |button| {
                        button
                            .cursor_pointer()
                            .hover(move |button| button.bg(rgb(palette.hover)))
                    })
                    .when(!can_quick_search, |button| button.opacity(0.72))
                    .child(MainView::render_lucide_icon(
                        Some(Icon::Zap),
                        12.0,
                        12.0,
                        if can_quick_search {
                            palette.accent
                        } else {
                            palette.muted_text
                        },
                    ))
                    .child("快搜")
                    .on_click(context.listener(
                        move |view, _event: &ClickEvent, _window, context| {
                            if can_quick_search {
                                view.start_quick_search(context);
                            }
                            context.stop_propagation();
                        },
                    )),
            )
            .child(
                div()
                    .id("search-dialog-window-submit")
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .h(px(28.0))
                    .px_3()
                    .rounded(px(5.0))
                    .text_xs()
                    .text_color(rgb(palette.on_accent))
                    .bg(rgb(submit_background))
                    .when(submit_enabled, |button| {
                        button
                            .cursor_pointer()
                            .hover(move |button| button.bg(rgb(submit_hover_background)))
                    })
                    .when(!submit_enabled, |button| button.opacity(0.72))
                    .child(MainView::render_lucide_icon(
                        Some(submit_icon),
                        12.0,
                        12.0,
                        palette.on_accent,
                    ))
                    .child(submit_label)
                    .on_click(context.listener(
                        move |view, _event: &ClickEvent, _window, context| {
                            if is_searching {
                                view.stop_search(context);
                            } else if can_search {
                                view.start_search(context);
                            }
                            context.stop_propagation();
                        },
                    )),
            )
    }
}

impl Render for SearchDialogWindowView {
    /// 渲染独立搜索窗口。
    ///
    /// 业务意图：
    /// - 窗口只承载搜索条件、进度和关闭动作；结果仍显示在主窗口底部面板。
    /// - 根节点填满独立窗口，避免在无系统标题栏场景下出现透明或不可点击区域。
    fn render(&mut self, window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let (
            dialog,
            can_search,
            can_quick_search,
            can_count,
            search_query_history,
            search_focus,
            directory_focus,
            palette,
        ) = {
            let main_view = self.main_view.read(context);
            let Some(dialog) = main_view.search_dialog.clone() else {
                let palette = main_view.palette();
                return div()
                    .id("search-dialog-window-empty")
                    .size_full()
                    .bg(rgb(palette.background));
            };
            (
                dialog.clone(),
                main_view.search_can_start(&dialog),
                main_view.quick_search_can_start(&dialog),
                main_view.search_can_count_current_file(&dialog) && !dialog.is_searching,
                main_view.search_query_history.clone(),
                main_view.search_input_focus.clone(),
                main_view.search_directory_focus.clone(),
                main_view.palette(),
            )
        };
        let history_menu_open = dialog.query_history_menu_open && !search_query_history.is_empty();

        div()
            .id("search-dialog-window")
            .flex()
            .flex_col()
            .size_full()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.background))
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.dismiss_search_history_menu(context);
                }),
            )
            .on_mouse_move(
                context.listener(|view, event: &MouseMoveEvent, _window, context| {
                    // 输入框拖拽选择一旦从输入框内开始，后续鼠标可能移到标题栏、范围按钮或空白区域。
                    // 根节点继续接收窗口内移动事件，可让选区稳定扩展到开头或末尾，而不是离开输入框后停住。
                    view.handle_search_text_mouse_move(event, context);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    view.handle_search_text_mouse_up(context);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    view.handle_search_text_mouse_up(context);
                }),
            )
            .child(self.render_header(palette, context))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_4()
                    .pt_4()
                    .pb_2()
                    .child(self.render_search_input(
                        &dialog,
                        &search_query_history,
                        search_focus,
                        window,
                        palette,
                        context,
                    ))
                    .child(self.render_directory_target(
                        &dialog,
                        directory_focus,
                        window,
                        palette,
                        context,
                    ))
                    .child(self.render_scope_controls(dialog.scope, palette, context))
                    .child(self.render_options_row(
                        dialog.case_sensitive,
                        dialog.match_mode,
                        palette,
                        context,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(if dialog.is_searching {
                                palette.accent
                            } else {
                                palette.muted_text
                            }))
                            .child(dialog.message.clone()),
                    )
                    .when(history_menu_open, |content| {
                        content.child(self.render_search_history_dropdown(
                            &search_query_history,
                            palette,
                            context,
                        ))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .justify_end()
                    // 操作按钮位于搜索窗口右下角，底部留白需要大于普通内容间距，避免按钮贴近无标题窗口下边框。
                    // 这里只调整外层安全边距，不改变按钮尺寸和点击热区，保证已有肌肉记忆不受影响。
                    .px_4()
                    .pb_5()
                    .pt_2()
                    .child(self.render_action_buttons(
                        can_search,
                        can_quick_search,
                        can_count,
                        dialog.is_searching,
                        palette,
                        context,
                    )),
            )
    }
}

/// 搜索输入框文本元素的预绘制结果。
///
/// 业务意图：
/// - GPUI 的官方输入示例会在 `prepaint` 阶段完成文本排版、选区矩形和光标矩形计算。
/// - 搜索关键字和目录目标输入框沿用这一路径，避免自绘文本再用估算字符宽度处理鼠标命中。
struct SearchTextInputPrepaint {
    /// 当前帧的单行字形布局，用于绘制文本并回写给 `MainView` 供鼠标命中测试。
    line: ShapedLine,
    /// 当前选择范围对应的高亮矩形；无选择时为空。
    selection: Option<PaintQuad>,
    /// 当前光标矩形；有非空选择时为空。
    cursor: Option<PaintQuad>,
}

/// 搜索输入框的 GPUI 文本输入元素。
///
/// 业务意图：
/// - GPUI 0.2.2 没有公开导出的现成 `TextInput` 控件，但官方示例提供的做法是自定义 `Element`，
///   在绘制阶段调用 `Window::handle_input`，并使用 GPUI 文本系统 `shape_line` 管理光标和选区。
/// - 该元素把搜索关键字和目录目标输入框改为同一套 GPUI 文本输入实现，支持中文 IME、全选、双击选词和三连击全选。
///
/// 边界条件：
/// - 输入框仍只支持单行文本；平台提交的换行会在 `EntityInputHandler` 中清理。
/// - 该元素只负责文本绘制和平台输入注册，搜索业务状态仍保存在 `MainView.search_dialog` 中。
struct SearchTextInputElement {
    /// 主视图实体，用于读取和写回搜索输入状态。
    view: Entity<MainView>,
    /// 当前元素对应的输入槽位。
    input_kind: SearchTextInputKind,
    /// 该输入框的焦点句柄。
    focus_handle: gpui::FocusHandle,
    /// 输入为空时显示的占位文案。
    placeholder: &'static str,
    /// 当前主题调色板，用于绘制文本、占位、选区和光标。
    palette: AppThemePalette,
}

impl IntoElement for SearchTextInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SearchTextInputElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<SearchTextInputPrepaint>;

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
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        // GPUI 文本元素需要在布局阶段拿到明确行高；如果使用相对高度，父级 flex 布局在某些窗口
        // 尺寸计算路径下会给出 0 高度，导致文字、光标和选区都完成状态更新但没有可见绘制区域。
        style.size.height = window.line_height().into();
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
        let Some((text, selection_range, marked_range, cursor_visible_by_activity)) = ({
            let view = self.view.read(context);
            view.search_text_snapshot(self.input_kind).map(
                |(text, selection_range, marked_range)| {
                    (
                        text,
                        selection_range,
                        marked_range,
                        view.search_text_cursor_visible(),
                    )
                },
            )
        }) else {
            return None;
        };
        let style = window.text_style();
        let display_text = if text.is_empty() {
            SharedString::from(self.placeholder)
        } else {
            SharedString::from(text.clone())
        };
        let text_color = if text.is_empty() {
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
        let runs = if !text.is_empty() {
            if let Some(marked_range) = marked_range {
                vec![
                    TextRun {
                        len: marked_range.start,
                        ..base_run.clone()
                    },
                    TextRun {
                        len: marked_range.end.saturating_sub(marked_range.start),
                        underline: Some(UnderlineStyle {
                            color: Some(base_run.color),
                            thickness: px(1.0),
                            wavy: false,
                        }),
                        ..base_run.clone()
                    },
                    TextRun {
                        len: display_text.len().saturating_sub(marked_range.end),
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

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);
        let focused = self.focus_handle.is_focused(window);
        let selection_range = MainView::clamp_search_text_range(&text, selection_range);
        let has_selection =
            focused && !text.is_empty() && selection_range.start < selection_range.end;
        let cursor_index = selection_range.end;
        let selection = has_selection.then(|| {
            let mut selection_color = rgb(self.palette.accent);
            selection_color.a = 0.32;
            fill(
                Bounds::from_corners(
                    point(
                        bounds.left() + line.x_for_index(selection_range.start),
                        bounds.top(),
                    ),
                    point(
                        bounds.left() + line.x_for_index(selection_range.end),
                        bounds.bottom(),
                    ),
                ),
                selection_color,
            )
        });
        let cursor_visible = focused && !has_selection && cursor_visible_by_activity;
        let cursor = cursor_visible.then(|| {
            fill(
                Bounds::new(
                    point(bounds.left() + line.x_for_index(cursor_index), bounds.top()),
                    size(px(1.5), bounds.bottom() - bounds.top()),
                ),
                rgb(self.palette.accent),
            )
        });

        Some(SearchTextInputPrepaint {
            line,
            selection,
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
        if let Some(selection) = prepaint.selection {
            window.paint_quad(selection);
        }
        prepaint
            .line
            .paint(bounds.origin, window.line_height(), window, context)
            .ok();
        if let Some(cursor) = prepaint.cursor {
            window.paint_quad(cursor);
        }
        if self.focus_handle.is_focused(window) {
            // 光标闪烁不依赖业务状态变化；只要输入框仍聚焦，就请求下一帧重绘，由时间片决定当前帧是否显示光标。
            window.request_animation_frame();
        }
        self.view.update(context, |view, _context| {
            view.store_search_text_layout(self.input_kind, prepaint.line, bounds);
        });
    }
}
