// 独立本地终端页渲染。
//
// 业务意图：
// - 本文件把 `TerminalWorkspaceState` 映射为独立导航页：顶部 tab 栏、正文终端、右键菜单和空态。
// - 终端行渲染复用连接页的 alacritty 渲染快照，保证本地终端和 SSH 终端的字号、颜色和选区行为一致。

use super::*;

impl MainView {
    /// 渲染独立本地终端页。
    pub(in crate::app) fn render_terminal_page(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = self.palette();
        let terminal_colors = connection_terminal_ui_colors(self.effective_theme(), palette);
        div()
            .id("terminal-page")
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(terminal_colors.background))
            .on_mouse_move(context.listener(Self::handle_terminal_page_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(Self::handle_terminal_page_mouse_up),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(Self::handle_terminal_page_mouse_up),
            )
            .child(self.render_terminal_tab_bar(palette, context))
            .child(self.render_terminal_area(terminal_colors, context))
            .child(self.render_terminal_workspace_menu_dismiss_overlay(context))
            .child(self.render_terminal_tab_context_menu(palette, context))
            .child(self.render_terminal_context_menu(palette, context))
    }

    /// 渲染本地终端 tab 栏。
    fn render_terminal_tab_bar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let tabs = self
            .terminal
            .tabs
            .iter()
            .map(|tab| self.render_terminal_tab(tab, palette, context))
            .collect::<Vec<_>>();

        div()
            .id("terminal-tab-bar")
            .relative()
            .flex()
            .items_center()
            .h(px(CONNECTIONS_TAB_BAR_HEIGHT))
            .flex_none()
            .overflow_hidden()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .child(self.render_terminal_new_tab_button(palette, context))
            .child(self.render_terminal_tab_scroll_button(
                Icon::ChevronLeft,
                -LOG_TAB_SCROLL_STEP,
                palette,
                context,
            ))
            .child(
                div()
                    .id("terminal-tab-scroll-viewport")
                    .flex()
                    .items_end()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_scroll()
                    .scrollbar_width(px(0.0))
                    .track_scroll(&self.terminal.tab_bar_scroll_handle)
                    .children(tabs),
            )
            .child(self.render_terminal_tab_scroll_button(
                Icon::ChevronRight,
                LOG_TAB_SCROLL_STEP,
                palette,
                context,
            ))
    }

    /// 渲染新建本地终端 tab 按钮。
    fn render_terminal_new_tab_button(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("terminal-new-tab-button")
            .flex()
            .items_center()
            .justify_center()
            .h_full()
            .w(px(LOG_TAB_SCROLL_BUTTON_WIDTH))
            .flex_none()
            .border_r_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .cursor_pointer()
            .hover(move |button| {
                button
                    .bg(rgb(palette.surface))
                    .text_color(rgb(palette.accent))
            })
            .child(Self::render_lucide_icon(
                Some(Icon::Plus),
                16.0,
                16.0,
                palette.muted_text,
            ))
            .on_click(
                context.listener(|view, _event: &ClickEvent, window, context| {
                    view.open_terminal_tab(window, context);
                }),
            )
    }

    /// 渲染终端 tab 栏横向滚动按钮。
    fn render_terminal_tab_scroll_button(
        &self,
        icon: Icon,
        delta: f32,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("terminal-tab-scroll-{delta}")))
            .flex()
            .items_center()
            .justify_center()
            .h_full()
            .w(px(LOG_TAB_SCROLL_BUTTON_WIDTH))
            .flex_none()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .cursor_pointer()
            .hover(move |button| {
                button
                    .bg(rgb(palette.surface))
                    .text_color(rgb(palette.accent))
            })
            .when(delta < 0.0, |button| button.border_r_1())
            .when(delta > 0.0, |button| button.border_l_1())
            .child(Self::render_lucide_icon(
                Some(icon),
                14.0,
                14.0,
                palette.muted_text,
            ))
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.scroll_terminal_tab_bar(delta, context);
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染单个本地终端 tab。
    fn render_terminal_tab(
        &self,
        tab: &ConnectionTerminalTab,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tab_id = tab.id;
        let active = self.terminal.active_tab_id == Some(tab.id);
        let terminal_colors = connection_terminal_ui_colors(self.effective_theme(), palette);
        div()
            .id(SharedString::from(format!("terminal-tab-{tab_id}")))
            .flex()
            .items_center()
            .gap_2()
            .h_full()
            .max_w(px(220.0))
            .px_3()
            .border_r_1()
            .border_color(rgb(palette.border))
            .bg(rgb(if active {
                terminal_colors.active_tab_background
            } else {
                terminal_colors.inactive_tab_background
            }))
            .text_color(rgb(if active {
                terminal_colors.active_tab_text
            } else {
                terminal_colors.inactive_tab_text
            }))
            .cursor_pointer()
            .on_click(
                context.listener(move |view, _event: &ClickEvent, window, context| {
                    view.activate_terminal_tab(tab_id, window, context);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.open_terminal_tab_context_menu(
                        tab_id,
                        f32::from(event.position.x),
                        f32::from(event.position.y),
                        context,
                    );
                    context.stop_propagation();
                }),
            )
            .child(Self::render_lucide_icon(
                Some(Icon::Terminal),
                16.0,
                14.0,
                if active {
                    terminal_colors.muted
                } else {
                    palette.muted_text
                },
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(12.0))
                    .line_height(px(16.0))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(tab.title.clone()),
            )
            .child(
                div()
                    .id(SharedString::from(format!("terminal-tab-close-{tab_id}")))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(18.0))
                    .rounded(px(4.0))
                    .hover(move |button| button.bg(rgb(terminal_colors.close_hover)))
                    .child(Self::render_lucide_icon(
                        Some(Icon::X),
                        12.0,
                        12.0,
                        if active {
                            terminal_colors.foreground
                        } else {
                            palette.muted_text
                        },
                    ))
                    .on_click(context.listener(
                        move |view, _event: &ClickEvent, _window, context| {
                            view.close_terminal_tab(tab_id, context);
                            context.stop_propagation();
                        },
                    )),
            )
            .into_any_element()
    }

    /// 渲染终端工作区菜单关闭遮罩。
    fn render_terminal_workspace_menu_dismiss_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if self.terminal.tab_context_menu.is_none() && self.terminal.terminal_context_menu.is_none()
        {
            return div().id("terminal-workspace-menu-overlay-empty").hidden();
        }
        div()
            .id("terminal-workspace-menu-overlay")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.terminal.tab_context_menu = None;
                    view.terminal.terminal_context_menu = None;
                    context.stop_propagation();
                    context.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.terminal.tab_context_menu = None;
                    view.terminal.terminal_context_menu = None;
                    context.stop_propagation();
                    context.notify();
                }),
            )
    }

    /// 渲染本地终端 tab 右键菜单。
    fn render_terminal_tab_context_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = self.terminal.tab_context_menu.as_ref() else {
            return div().id("terminal-tab-context-menu-empty").hidden();
        };
        let tab_id = menu.tab_id;
        div()
            .id("terminal-tab-context-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(TAB_CONTEXT_MENU_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(self.render_terminal_tab_context_menu_item(
                tab_id,
                TerminalTabContextMenuAction::Duplicate,
                "复制终端",
                palette,
                context,
            ))
            .child(self.render_terminal_tab_context_menu_item(
                tab_id,
                TerminalTabContextMenuAction::Current,
                "关闭当前",
                palette,
                context,
            ))
            .child(self.render_terminal_tab_context_menu_item(
                tab_id,
                TerminalTabContextMenuAction::OtherTabs,
                "关闭其他",
                palette,
                context,
            ))
            .child(self.render_terminal_tab_context_menu_item(
                tab_id,
                TerminalTabContextMenuAction::AllTabs,
                "关闭所有",
                palette,
                context,
            ))
    }

    /// 渲染本地终端 tab 菜单单项。
    fn render_terminal_tab_context_menu_item(
        &self,
        tab_id: usize,
        action: TerminalTabContextMenuAction,
        label: &'static str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!(
                "terminal-tab-menu-{}-{}",
                tab_id, label
            )))
            .flex()
            .items_center()
            .h(px(TAB_CONTEXT_MENU_ITEM_HEIGHT))
            .px_3()
            .text_sm()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |item| item.bg(rgb(palette.hover)))
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, window, context| {
                    view.handle_terminal_tab_context_menu_action(tab_id, action, window, context);
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染终端正文右键菜单。
    fn render_terminal_context_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = self.terminal.terminal_context_menu.as_ref() else {
            return div().id("terminal-context-menu-empty").hidden();
        };
        let tab_id = menu.tab_id;
        let copy_enabled = self
            .terminal
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| tab.emulator.selected_text())
            .is_some_and(|text| !text.is_empty());

        div()
            .id("terminal-context-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(TAB_CONTEXT_MENU_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(self.render_terminal_context_menu_item(
                tab_id,
                TerminalContextMenuAction::FileManager,
                "文件管理",
                true,
                palette,
                context,
            ))
            .child(self.render_terminal_context_menu_item(
                tab_id,
                TerminalContextMenuAction::Copy,
                "复制",
                copy_enabled,
                palette,
                context,
            ))
            .child(self.render_terminal_context_menu_item(
                tab_id,
                TerminalContextMenuAction::Paste,
                "粘贴",
                true,
                palette,
                context,
            ))
    }

    /// 渲染终端正文菜单单项。
    fn render_terminal_context_menu_item(
        &self,
        tab_id: usize,
        action: TerminalContextMenuAction,
        label: &'static str,
        enabled: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!(
                "terminal-menu-{}-{}",
                tab_id, label
            )))
            .flex()
            .items_center()
            .h(px(TAB_CONTEXT_MENU_ITEM_HEIGHT))
            .px_3()
            .text_sm()
            .text_color(rgb(if enabled {
                palette.text
            } else {
                palette.muted_text
            }))
            .when(enabled, |item| {
                item.cursor_pointer()
                    .hover(move |item| item.bg(rgb(palette.hover)))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(move |view, _event: &MouseDownEvent, window, context| {
                            view.handle_terminal_context_menu_action(
                                tab_id, action, window, context,
                            );
                        }),
                    )
            })
            .child(label)
    }

    /// 渲染本地终端正文区域。
    fn render_terminal_area(
        &self,
        terminal_colors: ConnectionTerminalUiColors,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(tab) = self.terminal.active_tab() else {
            return div()
                .id("terminal-empty")
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .flex_1()
                .text_color(rgb(terminal_colors.muted))
                .bg(rgb(terminal_colors.background))
                .child(Self::render_lucide_icon(
                    Some(Icon::SquareTerminal),
                    34.0,
                    32.0,
                    terminal_colors.muted,
                ))
                .child(div().text_sm().child("点击 + 新建本地终端"))
                .into_any_element();
        };

        let lines = tab.emulator.render_lines(self.effective_theme());
        div()
            .id("terminal-area")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .track_focus(&tab.focus)
            .key_context("local-terminal")
            .on_key_down(context.listener(Self::handle_terminal_key_down))
            .bg(rgb(terminal_colors.background))
            .child(
                div()
                    .id("terminal-lines")
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .p(px(CONNECTION_TERMINAL_PADDING))
                    .font_family(LOG_VIEWER_FONT_FAMILY)
                    .text_size(px(CONNECTION_TERMINAL_FONT_SIZE))
                    .line_height(px(CONNECTION_TERMINAL_ROW_HEIGHT))
                    .text_color(rgb(terminal_colors.foreground))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, event: &MouseDownEvent, window, context| {
                            if let Some(tab) = view.terminal.active_tab() {
                                window.focus(&tab.focus);
                            }
                            view.begin_terminal_selection(event, context);
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        context.listener(|view, event: &MouseDownEvent, window, context| {
                            let Some(tab_id) = view.terminal.active_tab_id else {
                                return;
                            };
                            view.open_terminal_context_menu(
                                tab_id,
                                f32::from(event.position.x),
                                f32::from(event.position.y),
                                window,
                                context,
                            );
                            context.stop_propagation();
                        }),
                    )
                    .children(
                        lines
                            .into_iter()
                            .map(|line| {
                                div()
                                    .h(px(CONNECTION_TERMINAL_ROW_HEIGHT))
                                    .flex_none()
                                    .whitespace_nowrap()
                                    .child(
                                        StyledText::new(if line.text.is_empty() {
                                            " ".to_string()
                                        } else {
                                            line.text
                                        })
                                        .with_highlights(line.highlights),
                                    )
                            })
                            .collect::<Vec<_>>(),
                    )
                    .when(!tab.ime.text.is_empty(), |terminal| {
                        terminal.child(self.render_terminal_ime_preedit(tab, terminal_colors))
                    })
                    .child(
                        div()
                            .id("terminal-resize-observer")
                            .absolute()
                            .left(px(CONNECTION_TERMINAL_PADDING))
                            .right(px(CONNECTION_TERMINAL_PADDING))
                            .top(px(CONNECTION_TERMINAL_PADDING))
                            .bottom(px(CONNECTION_TERMINAL_PADDING))
                            .child(TerminalResizeObserverElement {
                                view: context.entity(),
                                tab_id: tab.id,
                                focus: tab.focus.clone(),
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 渲染本地终端 IME 组合文本预览。
    fn render_terminal_ime_preedit(
        &self,
        tab: &ConnectionTerminalTab,
        terminal_colors: ConnectionTerminalUiColors,
    ) -> impl IntoElement {
        let cursor = tab.emulator.cursor_point();
        let left = CONNECTION_TERMINAL_PADDING + cursor.column.0 as f32 * tab.cell_width;
        let top = CONNECTION_TERMINAL_PADDING
            + cursor.line.0.max(0) as f32 * CONNECTION_TERMINAL_ROW_HEIGHT;

        div()
            .id("terminal-ime-preedit")
            .absolute()
            .left(px(left))
            .top(px(top))
            .h(px(CONNECTION_TERMINAL_ROW_HEIGHT))
            .px_0()
            .font_family(LOG_VIEWER_FONT_FAMILY)
            .text_size(px(CONNECTION_TERMINAL_FONT_SIZE))
            .line_height(px(CONNECTION_TERMINAL_ROW_HEIGHT))
            .text_color(rgb(terminal_colors.foreground))
            .bg(rgb(terminal_colors.background))
            .border_b_1()
            .border_color(rgb(terminal_colors.cursor_background))
            .child(tab.ime.text.clone())
    }
}

/// 本地终端内容区尺寸观察元素。
///
/// 业务意图：
/// - GPUI 普通 `div` 没有布局完成回调；本地 PTY resize 和 IME 候选窗口都需要真实内容 bounds。
/// - 该元素不绘制内容，只在 paint 阶段回写尺寸并注册平台输入处理器。
struct TerminalResizeObserverElement {
    /// 主视图实体，用于在 paint 阶段回写当前 tab 的终端尺寸。
    view: Entity<MainView>,
    /// 需要同步尺寸的终端 tab ID。
    tab_id: usize,
    /// 当前 tab 的终端焦点句柄，用于注册平台输入处理器。
    focus: FocusHandle,
}

impl IntoElement for TerminalResizeObserverElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalResizeObserverElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

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
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], context), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _context: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        context: &mut App,
    ) {
        let cell_width = measure_terminal_cell_width(window);
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            context,
        );
        self.view.update(context, |view, context| {
            view.sync_terminal_size_from_bounds(self.tab_id, bounds, cell_width, context);
        });
    }
}

/// 测量本地终端字体的真实单元格宽度。
///
/// 实现原因：
/// - 终端鼠标选区、PTY resize 和 IME 候选位置都以等宽单元格为单位。
/// - 不同平台字体回退可能让经验值产生累计偏差，因此渲染阶段要用同一字体真实测量。
fn measure_terminal_cell_width(window: &mut Window) -> f32 {
    let mut style = window.text_style();
    style.font_family = LOG_VIEWER_FONT_FAMILY.into();
    style.font_size = px(CONNECTION_TERMINAL_FONT_SIZE).into();
    let run = TextRun {
        len: 1,
        font: style.font(),
        color: style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(
        SharedString::from("M"),
        px(CONNECTION_TERMINAL_FONT_SIZE),
        &[run],
        None,
    );
    let measured = f32::from(line.x_for_index(1));
    if measured.is_finite() && measured >= 1.0 {
        measured
    } else {
        CONNECTION_TERMINAL_CELL_WIDTH
    }
}
