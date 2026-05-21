// 连接页 GPUI 渲染。
//
// 业务意图：
// - 本文件只把 `ConnectionsWorkspaceState` 映射成左侧连接管理、右侧终端 tab 和弹窗 UI。
// - 保存、删除、连接、主机指纹确认等副作用全部委托 actions.rs，避免渲染路径持有业务规则。

use super::*;

impl MainView {
    /// 渲染连接页。
    pub(in crate::app) fn render_connections_page(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = self.palette();
        let terminal_colors = connection_terminal_ui_colors(self.effective_theme(), palette);
        div()
            .id("connections-page")
            .relative()
            .flex()
            .size_full()
            .bg(rgb(palette.background))
            .on_mouse_move(context.listener(Self::handle_connections_page_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(Self::handle_connections_page_mouse_up),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(Self::handle_connections_page_mouse_up),
            )
            .child(self.render_connections_tree_panel(palette, context))
            .child(self.render_connections_workspace(palette, terminal_colors, context))
            .when(self.connections.tree_resize_drag.is_some(), |page| {
                page.child(
                    div()
                        .id("connections-resize-cursor-overlay")
                        .absolute()
                        .left(px(0.0))
                        .right(px(0.0))
                        .top(px(0.0))
                        .bottom(px(0.0))
                        .cursor_col_resize()
                        .on_mouse_move(context.listener(Self::handle_connections_page_mouse_move))
                        .on_mouse_up(
                            MouseButton::Left,
                            context.listener(Self::handle_connections_page_mouse_up),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            context.listener(Self::handle_connections_page_mouse_up),
                        ),
                )
            })
            .child(self.render_connection_dialog(palette, context))
            .child(self.render_connection_delete_dialog(palette, context))
            .child(self.render_connection_host_key_dialog(palette, context))
    }

    /// 渲染左侧连接管理栏。
    fn render_connections_tree_panel(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("connections-tree-panel")
            .relative()
            .flex()
            .flex_col()
            .w(px(self.connections.tree_width))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .child(self.render_connections_tree_toolbar(palette, context))
            .child(self.render_connections_tree_body(palette, context))
            .child(self.render_connections_tree_resize_handle(context))
            .child(self.render_connections_tree_menu_dismiss_overlay(context))
            .child(self.render_connections_create_menu(palette, context))
            .child(self.render_connection_profile_context_menu(palette, context))
    }

    /// 渲染连接左侧栏工具栏。
    fn render_connections_tree_toolbar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("connections-tree-toolbar")
            .flex()
            .items_center()
            .justify_between()
            .h(px(CONNECTIONS_TOOLBAR_HEIGHT))
            .px_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(Self::render_lucide_icon(
                        Some(Icon::Cable),
                        20.0,
                        17.0,
                        palette.muted_text,
                    ))
                    .child("连接"),
            )
            .child(
                div()
                    .id("connections-create-button")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(26.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
                    .child(Self::render_lucide_icon(
                        Some(Icon::Plus),
                        18.0,
                        16.0,
                        palette.text,
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(Self::toggle_connection_create_menu_from_mouse_down),
                    ),
            )
    }

    /// 渲染连接左侧栏浮层菜单的透明关闭遮罩。
    ///
    /// 业务意图：
    /// - 新建菜单和连接行右键菜单都只属于左侧连接栏，点击菜单外的左侧区域应关闭菜单并消费事件，避免误选连接行。
    /// - 遮罩限制在左侧栏内部，不影响主导航和右侧终端区域交互。
    fn render_connections_tree_menu_dismiss_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if !self.connections.create_menu_open && self.connections.profile_context_menu.is_none() {
            return div().id("connections-tree-menu-overlay-empty").hidden();
        }

        div()
            .id("connections-tree-menu-overlay")
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.close_connection_tree_menus(context);
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.close_connection_tree_menus(context);
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染新建连接类型菜单。
    ///
    /// 业务意图：
    /// - 新增按钮先展开连接类型列表；SSH 进入配置表单，本地终端直接打开 tab。
    /// - 菜单项顺序由状态层常量维护，避免视图和测试分别写死连接类型。
    fn render_connections_create_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if !self.connections.create_menu_open {
            return div().id("connections-create-menu-empty").hidden();
        }

        div()
            .id("connections-create-menu")
            .absolute()
            .right(px(12.0))
            .top(px(CONNECTIONS_TOOLBAR_HEIGHT - 2.0))
            .w(px(168.0))
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
            .children(
                CONNECTION_CREATE_KINDS
                    .iter()
                    .copied()
                    .map(|kind| self.render_connections_create_menu_item(kind, palette, context))
                    .collect::<Vec<_>>(),
            )
    }

    /// 渲染新建连接类型菜单项。
    fn render_connections_create_menu_item(
        &self,
        kind: ConnectionCreateKind,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "connections-create-kind-{}",
                kind.label()
            )))
            .flex()
            .items_center()
            .gap_2()
            .h(px(32.0))
            .px_3()
            .text_sm()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |item| item.bg(rgb(palette.hover)))
            .child(Self::render_lucide_icon(
                Some(kind.icon()),
                17.0,
                16.0,
                palette.muted_text,
            ))
            .text_size(px(14.0))
            .line_height(px(20.0))
            .child(kind.label())
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, window, context| {
                    view.handle_connection_create_menu_action(kind, window, context);
                }),
            )
    }

    /// 渲染左侧连接列表。
    fn render_connections_tree_body(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        if let Some(error) = self.connections.database_error.as_ref() {
            return div()
                .id("connections-database-error")
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .text_sm()
                .text_color(rgb(0xb91c1c))
                .child("连接数据库不可用")
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette.muted_text))
                        .child(error.clone()),
                )
                .into_any_element();
        }

        if self.connections.profiles.is_empty() {
            return div()
                .id("connections-empty-list")
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .flex_1()
                .px_4()
                .text_center()
                .text_sm()
                .text_color(rgb(palette.muted_text))
                .child(Self::render_lucide_icon(
                    Some(Icon::Server),
                    30.0,
                    28.0,
                    palette.muted_text,
                ))
                .child("暂无连接")
                .into_any_element();
        }

        let mut rows = Vec::new();
        for profile in &self.connections.profiles {
            rows.push(self.render_connection_profile_row(profile, palette, context));
        }

        div()
            .id("connections-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_2()
            .children(rows)
            .into_any_element()
    }

    /// 渲染单条连接。
    fn render_connection_profile_row(
        &self,
        profile: &ConnectionProfile,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let click_profile_id = profile.id.clone();
        let context_profile_id = profile.id.clone();
        let active = self.connections.selected_profile_id.as_deref() == Some(profile.id.as_str());
        let background = if active { palette.hover } else { palette.panel };
        div()
            .id(SharedString::from(format!("connection-row-{}", profile.id)))
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .mb_1()
            .rounded(px(6.0))
            .bg(rgb(background))
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_click(
                context.listener(move |view, _event: &ClickEvent, window, context| {
                    // 连接行主动作是直接打开一个新的 SSH tab；编辑和删除统一放在右键菜单，避免卡片内按钮分散点击目标。
                    view.open_connection_terminal(&click_profile_id, window, context);
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(move |view, event: &MouseDownEvent, window, context| {
                    view.open_connection_profile_context_menu(
                        &context_profile_id,
                        event,
                        window,
                        context,
                    );
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Self::render_lucide_icon(
                        Some(Icon::Terminal),
                        18.0,
                        15.0,
                        palette.muted_text,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(profile.name.clone()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted_text))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(format!(
                        "{}@{}:{}",
                        profile.username, profile.host, profile.port
                    )),
            )
            .into_any_element()
    }

    /// 渲染连接行右键菜单。
    fn render_connection_profile_context_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = self.connections.profile_context_menu.as_ref() else {
            return div().id("connection-profile-context-menu-empty").hidden();
        };

        div()
            .id("connection-profile-context-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(CONNECTIONS_CONTEXT_MENU_WIDTH))
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
            .child(self.render_connection_profile_context_menu_item(
                ConnectionProfileContextMenuAction::Connect,
                "连接",
                Icon::Play,
                palette,
                context,
            ))
            .child(self.render_connection_profile_context_menu_item(
                ConnectionProfileContextMenuAction::Edit,
                "编辑",
                Icon::Pencil,
                palette,
                context,
            ))
            .child(self.render_connection_profile_context_menu_item(
                ConnectionProfileContextMenuAction::Delete,
                "删除",
                Icon::Trash2,
                palette,
                context,
            ))
    }

    /// 渲染连接行右键菜单单项。
    fn render_connection_profile_context_menu_item(
        &self,
        action: ConnectionProfileContextMenuAction,
        label: &'static str,
        icon: Icon,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "connection-profile-menu-{label}"
            )))
            .flex()
            .items_center()
            .gap_2()
            .h(px(CONNECTIONS_CONTEXT_MENU_ITEM_HEIGHT))
            .px_3()
            .text_sm()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |item| item.bg(rgb(palette.hover)))
            .child(Self::render_lucide_icon(
                Some(icon),
                16.0,
                15.0,
                palette.muted_text,
            ))
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, window, context| {
                    view.handle_connection_profile_context_menu_action(action, window, context);
                }),
            )
    }

    /// 渲染左侧栏拖拽命中区。
    fn render_connections_tree_resize_handle(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("connections-tree-resize-handle")
            .absolute()
            .right(px(-(SPLITTER_HIT_WIDTH / 2.0)))
            .top(px(0.0))
            .bottom(px(0.0))
            .w(px(SPLITTER_HIT_WIDTH))
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(Self::start_connections_tree_resize),
            )
    }

    /// 渲染右侧终端工作区。
    fn render_connections_workspace(
        &self,
        palette: AppThemePalette,
        terminal_colors: ConnectionTerminalUiColors,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("connections-workspace")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(rgb(terminal_colors.background))
            .child(self.render_connections_tab_bar(palette, context))
            .child(self.render_connection_terminal_area(palette, terminal_colors, context))
    }

    /// 渲染终端 tab 栏。
    fn render_connections_tab_bar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.connections.tabs.is_empty() {
            return div()
                .id("connections-empty-tab-bar")
                .h(px(CONNECTIONS_TAB_BAR_HEIGHT))
                .flex_none()
                .border_b_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.panel))
                .into_any_element();
        }
        let mut tabs = Vec::new();
        for tab in &self.connections.tabs {
            tabs.push(self.render_connection_terminal_tab(tab, palette, context));
        }
        div()
            .id("connections-tab-bar")
            .flex()
            .items_center()
            .h(px(CONNECTIONS_TAB_BAR_HEIGHT))
            .flex_none()
            .overflow_x_scroll()
            .scrollbar_width(px(0.0))
            .track_scroll(&self.connections.tab_bar_scroll_handle)
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .children(tabs)
            .into_any_element()
    }

    /// 渲染单个终端 tab。
    fn render_connection_terminal_tab(
        &self,
        tab: &ConnectionTerminalTab,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let tab_id = tab.id;
        let close_tab_id = tab.id;
        let active = self.connections.active_tab_id == Some(tab.id);
        let terminal_colors = connection_terminal_ui_colors(self.effective_theme(), palette);
        div()
            .id(SharedString::from(format!(
                "connection-terminal-tab-{tab_id}"
            )))
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
                    view.activate_connection_terminal_tab(tab_id, window, context);
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
                    .id(SharedString::from(format!(
                        "connection-terminal-tab-close-{tab_id}"
                    )))
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
                            view.close_connection_terminal_tab(close_tab_id, context);
                            context.stop_propagation();
                        },
                    )),
            )
            .into_any_element()
    }

    /// 渲染终端区域。
    fn render_connection_terminal_area(
        &self,
        _palette: AppThemePalette,
        terminal_colors: ConnectionTerminalUiColors,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(tab) = self.connections.active_tab() else {
            return div()
                .id("connections-terminal-empty")
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .flex_1()
                .text_color(rgb(terminal_colors.muted))
                .bg(rgb(terminal_colors.background))
                .child(Self::render_lucide_icon(
                    Some(Icon::Cable),
                    34.0,
                    32.0,
                    terminal_colors.muted,
                ))
                .child(div().text_sm().child("点击左侧连接打开终端"))
                .into_any_element();
        };

        let lines = tab.emulator.render_lines(self.effective_theme());
        div()
            .id("connections-terminal-area")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .track_focus(&tab.focus)
            .key_context("connection-terminal")
            .on_key_down(context.listener(Self::handle_connection_terminal_key_down))
            .bg(rgb(terminal_colors.background))
            .child(
                div()
                    .id("connections-terminal-lines")
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
                            // 只在终端正文区域启动选区或 xterm 鼠标上报；终端外壳没有状态栏后，点击 tab 栏或侧栏不会被换算成终端第 0 行。
                            if let Some(tab) = view.connections.active_tab() {
                                window.focus(&tab.focus);
                            }
                            view.begin_connection_terminal_selection(event, context);
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
                    .child(
                        div()
                            .id("connections-terminal-resize-observer")
                            .absolute()
                            .left(px(CONNECTION_TERMINAL_PADDING))
                            .right(px(CONNECTION_TERMINAL_PADDING))
                            .top(px(CONNECTION_TERMINAL_PADDING))
                            .bottom(px(CONNECTION_TERMINAL_PADDING))
                            .child(ConnectionTerminalResizeObserverElement {
                                view: context.entity(),
                                tab_id: tab.id,
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 渲染新增/编辑连接弹窗。
    fn render_connection_dialog(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(dialog) = self.connections.dialog.as_ref() else {
            return div().hidden().into_any_element();
        };
        let is_edit = matches!(dialog.mode, ConnectionDialogMode::Edit { .. });
        let title = if is_edit {
            "编辑 SSH 连接"
        } else {
            "新增 SSH 连接"
        };
        self.render_connections_modal_shell("connection-dialog-overlay", palette, context)
            .child(
                div()
                    .id("connection-dialog")
                    .flex()
                    .flex_col()
                    .w(px(440.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.background))
                    .shadow_lg()
                    .child(self.render_connection_dialog_header(title, palette, context))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_4()
                            .child(self.render_connection_form_field(
                                "名称",
                                ConnectionFormField::Name,
                                None,
                                palette,
                                context,
                            ))
                            .child(self.render_connection_form_field(
                                "主机",
                                ConnectionFormField::Host,
                                None,
                                palette,
                                context,
                            ))
                            .child(self.render_connection_form_field(
                                "端口",
                                ConnectionFormField::Port,
                                None,
                                palette,
                                context,
                            ))
                            .child(self.render_connection_form_field(
                                "用户名",
                                ConnectionFormField::Username,
                                None,
                                palette,
                                context,
                            ))
                            .child(self.render_connection_form_field(
                                "密码",
                                ConnectionFormField::Password,
                                is_edit.then_some("留空表示不修改旧密码"),
                                palette,
                                context,
                            ))
                            .when_some(dialog.error.as_ref(), |body, error| {
                                body.child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(0xb91c1c))
                                        .child(error.clone()),
                                )
                            }),
                    )
                    .child(self.render_connection_dialog_footer(is_edit, palette, context)),
            )
            .into_any_element()
    }

    /// 渲染连接弹窗标题栏。
    fn render_connection_dialog_header(
        &self,
        title: &'static str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(44.0))
            .px_4()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(title),
            )
            .child(
                div()
                    .id("connection-dialog-close-button")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(26.0))
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
                    .child(Self::render_lucide_icon(
                        Some(Icon::X),
                        16.0,
                        15.0,
                        palette.muted_text,
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(Self::close_connection_dialog_from_mouse_down),
                    ),
            )
    }

    /// 渲染连接表单字段。
    fn render_connection_form_field(
        &self,
        label: &'static str,
        field: ConnectionFormField,
        placeholder: Option<&'static str>,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let focus_handle = self
            .connections
            .dialog
            .as_ref()
            .map(|dialog| dialog.field(field).focus.clone())
            .unwrap_or_else(|| context.focus_handle());
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted_text))
                    .child(label),
            )
            .child(
                div()
                    .h(px(32.0))
                    .flex()
                    .items_center()
                    .px_2()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.panel))
                    .text_sm()
                    .text_color(rgb(palette.text))
                    .track_focus(&focus_handle)
                    .key_context("connection-form-input")
                    .on_key_down(context.listener(Self::handle_connection_form_key_down))
                    .child(TextInputElement {
                        view: context.entity(),
                        binding: TextInputBinding::ConnectionForm(field),
                        focus_handle,
                        placeholder: placeholder.unwrap_or(""),
                        palette,
                    }),
            )
    }

    /// 渲染连接弹窗底部操作。
    fn render_connection_dialog_footer(
        &self,
        is_edit: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_4()
            .pb_4()
            .child(div().when(is_edit, |left| {
                left.child(self.render_modal_secondary_button(
                    "重置信任指纹",
                    palette,
                    context.listener(Self::reset_connection_host_key_from_dialog_from_mouse_down),
                ))
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.render_modal_secondary_button(
                        "取消",
                        palette,
                        context.listener(Self::close_connection_dialog_from_mouse_down),
                    ))
                    .child(self.render_modal_primary_button(
                        "保存",
                        palette,
                        context.listener(Self::save_connection_dialog_from_mouse_down),
                    )),
            )
    }

    /// 渲染删除确认弹窗。
    fn render_connection_delete_dialog(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(dialog) = self.connections.delete_confirm_dialog.as_ref() else {
            return div().hidden().into_any_element();
        };
        self.render_connections_modal_shell("connection-delete-dialog-overlay", palette, context)
            .child(
                div()
                    .id("connection-delete-dialog")
                    .flex()
                    .flex_col()
                    .gap_4()
                    .w(px(380.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.background))
                    .shadow_lg()
                    .p_4()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child("删除连接"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_text))
                            .child(format!(
                                "删除 {} 后，该连接已打开的终端 tab 会同时关闭。",
                                dialog.profile_name
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                self.render_modal_secondary_button(
                                    "取消",
                                    palette,
                                    context.listener(
                                        Self::close_delete_connection_dialog_from_mouse_down,
                                    ),
                                ),
                            )
                            .child(self.render_modal_danger_button(
                                "删除",
                                palette,
                                context.listener(Self::confirm_delete_connection_from_mouse_down),
                            )),
                    ),
            )
            .into_any_element()
    }

    /// 渲染 SSH 主机指纹弹窗。
    fn render_connection_host_key_dialog(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(dialog) = self.connections.host_key_dialog.as_ref() else {
            return div().hidden().into_any_element();
        };
        let mismatch = dialog.expected.is_some();
        self.render_connections_modal_shell("connection-host-key-dialog-overlay", palette, context)
            .child(
                div()
                    .id("connection-host-key-dialog")
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w(px(520.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.background))
                    .shadow_lg()
                    .p_4()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(if mismatch {
                                "SSH 主机指纹不匹配"
                            } else {
                                "确认 SSH 主机指纹"
                            }),
                    )
                    .when_some(dialog.expected.as_ref(), |body, expected| {
                        body.child(
                            div()
                                .text_xs()
                                .text_color(rgb(0xb91c1c))
                                .child(format!("已信任：{expected}")),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .font_family(LOG_VIEWER_FONT_FAMILY)
                            .text_color(rgb(palette.text))
                            .bg(rgb(palette.panel))
                            .rounded(px(6.0))
                            .p_2()
                            .child(dialog.fingerprint.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_text))
                            .child(if mismatch {
                                "服务器返回的指纹与本地保存值不一致，连接已阻止。确认服务器可信后，可在编辑连接中重置信任指纹。"
                            } else {
                                "首次连接该主机需要确认指纹。确认后会保存到连接配置，后续不匹配时会阻止连接。"
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(self.render_modal_secondary_button(
                                "关闭",
                                palette,
                                context.listener(
                                    Self::close_connection_host_key_dialog_from_mouse_down,
                                ),
                            ))
                            .when(!mismatch, |actions| {
                                actions.child(self.render_modal_primary_button(
                                    "确认并连接",
                                    palette,
                                    context.listener(
                                        Self::confirm_connection_host_key_from_mouse_down,
                                    ),
                                ))
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 渲染弹窗外壳。
    fn render_connections_modal_shell(
        &self,
        id: &'static str,
        _palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .flex()
            .items_center()
            .justify_center()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 透明模态层不绘制黑色遮罩，但仍必须阻断连接列表和终端区域的底层点击。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 右键不能穿透到终端或连接行，避免弹窗打开时触发底层上下文动作。
                    context.stop_propagation();
                }),
            )
            .on_scroll_wheel(context.listener(
                |_view, _event: &ScrollWheelEvent, _window, context| {
                    // 弹窗背景区域的滚轮不应滚动背后的终端或连接列表。
                    context.stop_propagation();
                },
            ))
    }

    /// 渲染弹窗主按钮。
    ///
    /// 业务意图：
    /// - 连接页弹窗位于终端、拖拽分栏和菜单等复杂鼠标区域之上，按钮统一使用 `mouse_down`
    ///   触发动作，避免 `click` 合成被底层焦点或拖拽状态打断。
    fn render_modal_primary_button(
        &self,
        label: &'static str,
        palette: AppThemePalette,
        listener: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!(
                "connection-modal-primary-{label}"
            )))
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .rounded(px(6.0))
            .text_sm()
            .text_color(rgb(0xffffff))
            .bg(rgb(palette.accent))
            .cursor_pointer()
            .hover(move |button| button.opacity(0.9))
            .child(label)
            .on_mouse_down(MouseButton::Left, listener)
    }

    /// 渲染弹窗次按钮。
    ///
    /// 业务意图：
    /// - 取消、关闭、重置等非主动作和主按钮保持同一事件模型，保证弹窗内所有操作的消费时机一致。
    fn render_modal_secondary_button(
        &self,
        label: &'static str,
        palette: AppThemePalette,
        listener: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!(
                "connection-modal-secondary-{label}"
            )))
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .rounded(px(6.0))
            .text_sm()
            .text_color(rgb(palette.text))
            .bg(rgb(palette.panel))
            .border_1()
            .border_color(rgb(palette.border))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(label)
            .on_mouse_down(MouseButton::Left, listener)
    }

    /// 渲染弹窗危险按钮。
    ///
    /// 业务意图：
    /// - 删除连接会级联关闭终端 tab，必须在按钮按下时明确消费事件，防止误传到底层连接列表或终端。
    fn render_modal_danger_button(
        &self,
        label: &'static str,
        _palette: AppThemePalette,
        listener: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!(
                "connection-modal-danger-{label}"
            )))
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .rounded(px(6.0))
            .text_sm()
            .text_color(rgb(0xffffff))
            .bg(rgb(0xdc2626))
            .cursor_pointer()
            .hover(move |button| button.opacity(0.9))
            .child(label)
            .on_mouse_down(MouseButton::Left, listener)
    }
}

/// 终端内容区尺寸观察元素。
///
/// 业务意图：
/// - GPUI 普通 `div` 渲染阶段不会把最终 bounds 回调给业务状态；终端 resize 又必须依赖真实内容区尺寸。
/// - 使用一个不绘制内容、不注册输入的轻量元素放在终端内容区绝对定位层中，只负责把 bounds 回写到当前 tab。
struct ConnectionTerminalResizeObserverElement {
    /// 主视图实体，用于在 paint 阶段回写当前 tab 的终端尺寸。
    view: Entity<MainView>,
    /// 需要同步尺寸的终端 tab ID。
    tab_id: usize,
}

impl IntoElement for ConnectionTerminalResizeObserverElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ConnectionTerminalResizeObserverElement {
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
        _window: &mut Window,
        context: &mut App,
    ) {
        self.view.update(context, |view, context| {
            view.sync_connection_terminal_size_from_bounds(self.tab_id, bounds, context);
        });
    }
}
