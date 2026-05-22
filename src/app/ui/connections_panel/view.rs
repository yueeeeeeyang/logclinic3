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
            .child(self.render_smb_connection_dialog(palette, context))
            .child(self.render_connection_category_dialog(palette, context))
            .child(self.render_connection_delete_dialog(palette, context))
            .child(self.render_connection_category_delete_dialog(palette, context))
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
            .child(self.render_connection_profile_hover_tooltip(palette))
            .child(self.render_connection_profile_context_menu(palette, context))
            .child(self.render_connection_category_context_menu(palette, context))
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
        if !self.connections.create_menu_open
            && self.connections.profile_context_menu.is_none()
            && self.connections.category_context_menu.is_none()
        {
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

        div()
            .id("connections-tree-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.render_connections_tree_search_bar(palette, context))
            .child(self.render_connections_tree_rows(palette, context))
            .into_any_element()
    }

    /// 渲染连接树搜索框。
    fn render_connections_tree_search_bar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let focus_handle = self.connections.tree_search.focus.clone();
        div()
            .id("connections-tree-search-bar")
            .flex()
            .items_center()
            .gap_2()
            .mx_2()
            .my_2()
            .h(px(CONNECTIONS_TREE_SEARCH_HEIGHT))
            .px_2()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.input))
            .text_sm()
            .text_color(rgb(palette.text))
            .track_focus(&focus_handle)
            .key_context("connection-tree-search-input")
            .on_key_down(context.listener(Self::handle_connection_tree_search_key_down))
            .child(Self::render_lucide_icon(
                Some(Icon::Search),
                15.0,
                14.0,
                palette.muted_text,
            ))
            .child(TextInputElement {
                view: context.entity(),
                binding: TextInputBinding::ConnectionTreeSearch,
                focus_handle,
                placeholder: "搜索连接名称",
                palette,
            })
            .when(!self.connections.tree_search.input.text.is_empty(), |bar| {
                bar.child(
                    div()
                        .id("connections-tree-search-clear")
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(18.0))
                        .rounded(px(4.0))
                        .cursor_pointer()
                        .hover(move |button| button.bg(rgb(palette.hover)))
                        .child(Self::render_lucide_icon(
                            Some(Icon::X),
                            12.0,
                            12.0,
                            palette.muted_text,
                        ))
                        .on_mouse_down(
                            MouseButton::Left,
                            context.listener(|view, _event: &MouseDownEvent, _window, context| {
                                view.connections.tree_search.input =
                                    SingleLineTextInputState::empty();
                                view.connections.tree_search.clear_layout();
                                view.connections.profile_context_menu = None;
                                view.connections.category_context_menu = None;
                                context.stop_propagation();
                                context.notify();
                            }),
                        ),
                )
            })
    }

    /// 渲染连接树分类和连接行。
    fn render_connections_tree_rows(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let search_query = self.connections.tree_search.query();
        if self.connections.profiles.is_empty()
            && self.connections.smb_profiles.is_empty()
            && self.connections.categories.is_empty()
        {
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

        let rows = build_connection_tree_rows(
            &self.connections.categories,
            &self.connections.profiles,
            &self.connections.smb_profiles,
            &self.connections.expanded_category_ids,
            &search_query,
        );
        if rows.is_empty() {
            return div()
                .id("connections-search-empty")
                .flex()
                .items_center()
                .justify_center()
                .flex_1()
                .px_4()
                .text_sm()
                .text_color(rgb(palette.muted_text))
                .child("没有匹配的连接")
                .into_any_element();
        }

        div()
            .id("connections-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_2()
            .children(
                rows.into_iter()
                    .map(|row| match row {
                        ConnectionTreeRow::Category {
                            category,
                            depth,
                            expanded,
                        } => self.render_connection_category_row(
                            &category, depth, expanded, palette, context,
                        ),
                        ConnectionTreeRow::Profile { profile, depth } => {
                            self.render_connection_profile_row(profile, depth, palette, context)
                        }
                    })
                    .collect::<Vec<_>>(),
            )
            .into_any_element()
    }

    /// 渲染单个分类行。
    fn render_connection_category_row(
        &self,
        category: &ConnectionCategory,
        depth: usize,
        expanded: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let click_category_id = category.id.clone();
        let context_category_id = category.id.clone();
        div()
            .id(SharedString::from(format!(
                "connection-category-{}",
                category.id
            )))
            .flex()
            .items_center()
            .gap_2()
            .h(px(30.0))
            .mb_1()
            .pl(px(8.0 + depth as f32 * CONNECTIONS_TREE_DEPTH_INDENT))
            .pr_2()
            .rounded(px(6.0))
            .bg(rgb(palette.panel))
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.toggle_connection_category(&click_category_id, context);
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(move |view, event: &MouseDownEvent, window, context| {
                    view.open_connection_category_context_menu(
                        &context_category_id,
                        event,
                        window,
                        context,
                    );
                }),
            )
            .child(Self::render_lucide_icon(
                Some(if expanded {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                }),
                14.0,
                13.0,
                palette.muted_text,
            ))
            .child(Self::render_lucide_icon(
                Some(if expanded {
                    Icon::FolderOpen
                } else {
                    Icon::Folder
                }),
                17.0,
                15.0,
                palette.muted_text,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(CONNECTIONS_TREE_NODE_TEXT_SIZE))
                    .line_height(px(CONNECTIONS_TREE_NODE_LINE_HEIGHT))
                    .font_weight(FontWeight::NORMAL)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(category.name.clone()),
            )
            .into_any_element()
    }

    /// 渲染单条连接。
    fn render_connection_profile_row(
        &self,
        profile: ConnectionTreeProfile,
        depth: usize,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let profile_key = profile.key();
        let click_profile_id = profile_key.clone();
        let context_profile_id = profile_key.clone();
        let hover_profile_id = profile_key.clone();
        let row_id = profile.id().to_string();
        let name = profile.name().to_string();
        let icon = profile.icon();
        let active = self.connections.selected_profile_id.as_deref() == Some(profile_key.as_str());
        let background = if active { palette.hover } else { palette.panel };
        div()
            .id(SharedString::from(format!("connection-row-{}", row_id)))
            .flex()
            .items_center()
            .gap_2()
            .h(px(30.0))
            .pl(px(8.0 + depth as f32 * CONNECTIONS_TREE_DEPTH_INDENT))
            .pr_2()
            .mb_1()
            .rounded(px(6.0))
            .bg(rgb(background))
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_click(
                context.listener(move |view, _event: &ClickEvent, window, context| {
                    // 连接行主动作由协议类型决定：SSH 打开终端，SMB 打开文件管理；编辑和删除统一放在右键菜单。
                    view.open_connection_entry(&click_profile_id, window, context);
                    context.stop_propagation();
                }),
            )
            .on_hover(
                context.listener(move |view, is_hovered: &bool, window, context| {
                    view.update_connection_profile_hover_tooltip(
                        &hover_profile_id,
                        *is_hovered,
                        window,
                        context,
                    );
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
            .child(Self::render_lucide_icon(
                Some(icon),
                18.0,
                15.0,
                palette.muted_text,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(CONNECTIONS_TREE_NODE_TEXT_SIZE))
                    .line_height(px(CONNECTIONS_TREE_NODE_LINE_HEIGHT))
                    .font_weight(FontWeight::NORMAL)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(name),
            )
            .into_any_element()
    }

    /// 渲染连接详情悬浮气泡。
    ///
    /// 业务意图：
    /// - 左侧连接卡片只显示名称，用户名和主机地址等次级信息在 hover 时展示，降低列表视觉噪音。
    /// - 右键菜单、分类菜单和新增菜单打开时隐藏该气泡，避免多个浮层同时抢占鼠标目标。
    fn render_connection_profile_hover_tooltip(
        &self,
        palette: AppThemePalette,
    ) -> gpui::Stateful<gpui::Div> {
        if self.connections.create_menu_open
            || self.connections.profile_context_menu.is_some()
            || self.connections.category_context_menu.is_some()
        {
            return div().id("connection-profile-tooltip-empty").hidden();
        }
        let Some(tooltip) = self.connections.profile_hover_tooltip.as_ref() else {
            return div().id("connection-profile-tooltip-empty").hidden();
        };
        let Some((title_id, user_label, address_label)) =
            connection_profile_tooltip_labels(&self.connections, &tooltip.profile_id)
        else {
            return div().id("connection-profile-tooltip-empty").hidden();
        };

        div()
            .id(SharedString::from(format!(
                "connection-profile-tooltip-{}",
                title_id
            )))
            .absolute()
            .left(px(tooltip.x))
            .top(px(tooltip.y))
            .w(px(CONNECTIONS_PROFILE_TOOLTIP_WIDTH))
            .py_2()
            .px_3()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .text_xs()
            .text_color(rgb(palette.text))
            .child(
                div()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(user_label),
            )
            .child(
                div()
                    .mt_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_color(rgb(palette.muted_text))
                    .child(address_label),
            )
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

    /// 渲染分类行右键菜单。
    fn render_connection_category_context_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = self.connections.category_context_menu.as_ref() else {
            return div().id("connection-category-context-menu-empty").hidden();
        };

        div()
            .id("connection-category-context-menu")
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
            .child(self.render_connection_category_context_menu_item(
                ConnectionCategoryContextMenuAction::CreateChild,
                "新建子分类",
                Icon::FolderPlus,
                palette,
                context,
            ))
            .child(self.render_connection_category_context_menu_item(
                ConnectionCategoryContextMenuAction::Edit,
                "编辑",
                Icon::Pencil,
                palette,
                context,
            ))
            .child(self.render_connection_category_context_menu_item(
                ConnectionCategoryContextMenuAction::Delete,
                "删除",
                Icon::Trash2,
                palette,
                context,
            ))
    }

    /// 渲染分类行右键菜单单项。
    fn render_connection_category_context_menu_item(
        &self,
        action: ConnectionCategoryContextMenuAction,
        label: &'static str,
        icon: Icon,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "connection-category-menu-{label}"
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
                    view.handle_connection_category_context_menu_action(action, window, context);
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
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(rgb(terminal_colors.background))
            .child(self.render_connections_tab_bar(palette, context))
            .child(self.render_connection_terminal_area(palette, terminal_colors, context))
            .child(self.render_connection_workspace_menu_dismiss_overlay(context))
            .child(self.render_connection_tab_context_menu(palette, context))
            .child(self.render_connection_terminal_context_menu(palette, context))
    }

    /// 渲染右侧终端工作区菜单关闭遮罩。
    ///
    /// 业务意图：
    /// - 连接 tab 右键菜单打开后，点击终端空白或其它 tab 区域应先关闭菜单，不能把同一次点击继续发给底层终端。
    /// - 遮罩只在菜单打开时出现，并且绘制在菜单下方，保证菜单项自身仍可点击。
    fn render_connection_workspace_menu_dismiss_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if self.connections.tab_context_menu.is_none()
            && self.connections.terminal_context_menu.is_none()
        {
            return div().id("connection-workspace-menu-overlay-empty").hidden();
        }

        div()
            .id("connection-workspace-menu-overlay")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.connections.tab_context_menu = None;
                    view.connections.terminal_context_menu = None;
                    context.stop_propagation();
                    context.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.connections.tab_context_menu = None;
                    view.connections.terminal_context_menu = None;
                    context.stop_propagation();
                    context.notify();
                }),
            )
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
            .relative()
            .flex()
            .items_center()
            .h(px(CONNECTIONS_TAB_BAR_HEIGHT))
            .flex_none()
            .overflow_hidden()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .child(self.render_connection_tab_scroll_button(
                Icon::ChevronLeft,
                -LOG_TAB_SCROLL_STEP,
                palette,
                context,
            ))
            .child(
                div()
                    .id("connections-tab-scroll-viewport")
                    .flex()
                    .items_end()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_scroll()
                    .scrollbar_width(px(0.0))
                    .track_scroll(&self.connections.tab_bar_scroll_handle)
                    .children(tabs),
            )
            .child(self.render_connection_tab_scroll_button(
                Icon::ChevronRight,
                LOG_TAB_SCROLL_STEP,
                palette,
                context,
            ))
            .into_any_element()
    }

    /// 渲染连接终端 tab 栏横向滚动按钮。
    fn render_connection_tab_scroll_button(
        &self,
        icon: Icon,
        delta: f32,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "connection-tab-scroll-{}",
                delta
            )))
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
                    view.scroll_connection_tab_bar(delta, context);
                    context.stop_propagation();
                }),
            )
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
            .on_mouse_down(
                MouseButton::Right,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.open_connection_tab_context_menu(
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

    /// 渲染连接终端 tab 右键菜单。
    fn render_connection_tab_context_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = self.connections.tab_context_menu.as_ref() else {
            return div().id("connection-tab-context-menu-empty").hidden();
        };
        let tab_id = menu.tab_id;
        div()
            .id("connection-tab-context-menu")
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
            .child(self.render_connection_tab_context_menu_item(
                tab_id,
                ConnectionTabContextMenuAction::Current,
                "关闭当前",
                palette,
                context,
            ))
            .child(self.render_connection_tab_context_menu_item(
                tab_id,
                ConnectionTabContextMenuAction::OtherTabs,
                "关闭其他",
                palette,
                context,
            ))
            .child(self.render_connection_tab_context_menu_item(
                tab_id,
                ConnectionTabContextMenuAction::AllTabs,
                "关闭所有",
                palette,
                context,
            ))
    }

    /// 渲染连接终端 tab 右键菜单单项。
    fn render_connection_tab_context_menu_item(
        &self,
        tab_id: usize,
        action: ConnectionTabContextMenuAction,
        label: &'static str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!(
                "connection-tab-menu-{}-{}",
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
                context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                    view.handle_connection_tab_context_menu_action(tab_id, action, context);
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染终端正文右键菜单。
    fn render_connection_terminal_context_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = self.connections.terminal_context_menu.as_ref() else {
            return div().id("connection-terminal-context-menu-empty").hidden();
        };
        let tab_id = menu.tab_id;
        let copy_enabled = self
            .connections
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| tab.emulator.selected_text())
            .is_some_and(|text| !text.is_empty());

        div()
            .id("connection-terminal-context-menu")
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
            .child(self.render_connection_terminal_context_menu_item(
                tab_id,
                ConnectionTerminalContextMenuAction::FileManager,
                "文件管理",
                true,
                palette,
                context,
            ))
            .child(self.render_connection_terminal_context_menu_item(
                tab_id,
                ConnectionTerminalContextMenuAction::Copy,
                "复制",
                copy_enabled,
                palette,
                context,
            ))
            .child(self.render_connection_terminal_context_menu_item(
                tab_id,
                ConnectionTerminalContextMenuAction::Paste,
                "粘贴",
                true,
                palette,
                context,
            ))
    }

    /// 渲染终端正文右键菜单单项。
    fn render_connection_terminal_context_menu_item(
        &self,
        tab_id: usize,
        action: ConnectionTerminalContextMenuAction,
        label: &'static str,
        enabled: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!(
                "connection-terminal-menu-{}-{}",
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
                            view.handle_connection_terminal_context_menu_action(
                                tab_id, action, window, context,
                            );
                        }),
                    )
            })
            .child(label)
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
                    .on_mouse_down(
                        MouseButton::Right,
                        context.listener(|view, event: &MouseDownEvent, window, context| {
                            let Some(tab_id) = view.connections.active_tab_id else {
                                return;
                            };
                            view.open_connection_terminal_context_menu(
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
                        terminal.child(
                            self.render_connection_terminal_ime_preedit(tab, terminal_colors),
                        )
                    })
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
                                focus: tab.focus.clone(),
                            }),
                    ),
            )
            .into_any_element()
    }

    /// 渲染终端 IME 组合文本预览。
    ///
    /// 业务意图：
    /// - 中文输入法组合阶段不能把拼音直接写入 PTY/SSH 后端，但用户仍需要看到当前组合文本的位置。
    /// - 预览层放在 alacritty 光标所在格点上方，最终提交后由 shell 回显真实内容。
    fn render_connection_terminal_ime_preedit(
        &self,
        tab: &ConnectionTerminalTab,
        terminal_colors: ConnectionTerminalUiColors,
    ) -> impl IntoElement {
        let cursor = tab.emulator.cursor_point();
        let left = CONNECTION_TERMINAL_PADDING + cursor.column.0 as f32 * tab.cell_width;
        let top = CONNECTION_TERMINAL_PADDING
            + cursor.line.0.max(0) as f32 * CONNECTION_TERMINAL_ROW_HEIGHT;

        div()
            .id("connections-terminal-ime-preedit")
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
            .when(dialog.category_select_open, |shell| {
                shell.child(self.render_connection_dialog_select_background_overlay(context))
            })
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
                            .relative()
                            .gap_3()
                            .p_4()
                            .when(dialog.category_select_open, |body| {
                                body.child(
                                    self.render_connection_category_select_dismiss_overlay(context),
                                )
                            })
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
                            .child(self.render_connection_category_select_field(palette, context))
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

    /// 渲染新增/编辑 SMB 连接弹窗。
    fn render_smb_connection_dialog(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(dialog) = self.connections.smb_dialog.as_ref() else {
            return div().hidden().into_any_element();
        };
        let is_edit = matches!(dialog.mode, ConnectionDialogMode::Edit { .. });
        let title = if is_edit {
            "编辑 SMB 连接"
        } else {
            "新增 SMB 连接"
        };
        self.render_connections_modal_shell("smb-connection-dialog-overlay", palette, context)
            .when(dialog.category_select_open, |shell| {
                shell.child(self.render_smb_connection_dialog_select_background_overlay(context))
            })
            .child(
                div()
                    .id("smb-connection-dialog")
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
                            .relative()
                            .gap_3()
                            .p_4()
                            .when(dialog.category_select_open, |body| {
                                body.child(
                                    self.render_smb_connection_category_select_dismiss_overlay(
                                        context,
                                    ),
                                )
                            })
                            .child(self.render_smb_connection_form_field(
                                "名称",
                                SmbConnectionFormField::Name,
                                None,
                                palette,
                                context,
                            ))
                            .child(self.render_smb_connection_form_field(
                                "地址",
                                SmbConnectionFormField::Address,
                                Some("server/share/path"),
                                palette,
                                context,
                            ))
                            .child(self.render_smb_connection_form_field(
                                "用户名",
                                SmbConnectionFormField::Username,
                                None,
                                palette,
                                context,
                            ))
                            .child(
                                self.render_smb_connection_category_select_field(palette, context),
                            )
                            .child(self.render_smb_connection_form_field(
                                "密码",
                                SmbConnectionFormField::Password,
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
                    .child(self.render_smb_connection_dialog_footer(palette, context)),
            )
            .into_any_element()
    }

    /// 渲染 SMB 连接弹窗背景层上的分类 Select 关闭区域。
    fn render_smb_connection_dialog_select_background_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("smb-connection-dialog-select-background-overlay")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(Self::close_smb_connection_dialog_category_select_from_mouse_down),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(Self::close_smb_connection_dialog_category_select_from_mouse_down),
            )
    }

    /// 渲染连接弹窗背景层上的分类 Select 关闭区域。
    ///
    /// 业务意图：
    /// - 用户点击弹窗卡片外部时不关闭连接弹窗，但应收起卡片内部打开的 Select，符合普通表单失焦体验。
    /// - 该层插入在弹窗卡片之前，卡片本身仍位于上方，避免影响触发器、输入框和按钮的点击命中。
    fn render_connection_dialog_select_background_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("connection-dialog-select-background-overlay")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(Self::close_connection_dialog_category_select_from_mouse_down),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(Self::close_connection_dialog_category_select_from_mouse_down),
            )
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

    /// 渲染 SMB 连接表单字段。
    fn render_smb_connection_form_field(
        &self,
        label: &'static str,
        field: SmbConnectionFormField,
        placeholder: Option<&'static str>,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let focus_handle = self
            .connections
            .smb_dialog
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
                    .key_context("smb-connection-form-input")
                    .on_key_down(context.listener(Self::handle_smb_connection_form_key_down))
                    .child(TextInputElement {
                        view: context.entity(),
                        binding: TextInputBinding::SmbConnectionForm(field),
                        focus_handle,
                        placeholder: placeholder.unwrap_or(""),
                        palette,
                    }),
            )
    }

    /// 渲染连接表单分类选择器。
    ///
    /// 业务意图：
    /// - 连接可以挂在任意分类；新增和编辑都通过同一个受控 Select 修改 `category_id`，保存时再写入 SQLite。
    /// - “无分类”对应根层，兼容升级前没有分类的旧连接。
    fn render_connection_category_select_field(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let (selected_category_id, open) = self
            .connections
            .dialog
            .as_ref()
            .map(|dialog| (dialog.category_id.clone(), dialog.category_select_open))
            .unwrap_or((None, false));
        let options = self.connection_category_select_options();
        let selected_label = options
            .iter()
            .find(|option| option.value == selected_category_id)
            .map(|option| option.label.clone())
            .unwrap_or_else(|| "无分类".to_string());
        let metrics = SelectMetrics::new(408.0, 32.0, 408.0, 30.0, 180.0, 2.0);
        let select = Select::new(
            "connection-category-select",
            selected_label,
            selected_category_id,
            options,
            metrics,
            palette,
        )
        .open_anchor(open.then_some(SelectAnchor::new(
            0.0,
            metrics.button_height + metrics.menu_gap,
        )));

        div()
            .id("connection-category-select-field")
            .relative()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted_text))
                    .child("分类"),
            )
            .child(
                select.render_trigger(
                    context.listener(Self::toggle_connection_dialog_category_select),
                ),
            )
            .child({
                // Select 菜单必须延迟到表单字段之后绘制，否则后续“密码”字段会盖住菜单，
                // 造成看起来半透明且无法稳定点击的浮层问题。
                gpui::deferred(select.render_menu(|category_id| {
                    Box::new(context.listener(
                        move |view, event: &MouseDownEvent, window, context| {
                            view.select_connection_dialog_category(
                                category_id.clone(),
                                event,
                                window,
                                context,
                            );
                        },
                    ))
                }))
                .with_priority(32)
            })
    }

    /// 渲染 SMB 连接表单分类选择器。
    fn render_smb_connection_category_select_field(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let (selected_category_id, open) = self
            .connections
            .smb_dialog
            .as_ref()
            .map(|dialog| (dialog.category_id.clone(), dialog.category_select_open))
            .unwrap_or((None, false));
        let options = self.connection_category_select_options();
        let selected_label = options
            .iter()
            .find(|option| option.value == selected_category_id)
            .map(|option| option.label.clone())
            .unwrap_or_else(|| "无分类".to_string());
        let metrics = SelectMetrics::new(408.0, 32.0, 408.0, 30.0, 180.0, 2.0);
        let select = Select::new(
            "smb-connection-category-select",
            selected_label,
            selected_category_id,
            options,
            metrics,
            palette,
        )
        .open_anchor(open.then_some(SelectAnchor::new(
            0.0,
            metrics.button_height + metrics.menu_gap,
        )));

        div()
            .id("smb-connection-category-select-field")
            .relative()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted_text))
                    .child("分类"),
            )
            .child(select.render_trigger(
                context.listener(Self::toggle_smb_connection_dialog_category_select),
            ))
            .child({
                gpui::deferred(select.render_menu(|category_id| {
                    Box::new(context.listener(
                        move |view, event: &MouseDownEvent, window, context| {
                            view.select_smb_connection_dialog_category(
                                category_id.clone(),
                                event,
                                window,
                                context,
                            );
                        },
                    ))
                }))
                .with_priority(32)
            })
    }

    /// 渲染连接表单分类 Select 的弹窗内关闭层。
    ///
    /// 业务意图：
    /// - 分类菜单打开后，用户点击同一弹窗内的空白区域应只收起菜单，不应关闭整个连接弹窗。
    /// - 该层先于表单字段插入，后续字段和菜单仍位于其上方；因此输入框、按钮和菜单项可以继续接收自己的鼠标事件。
    /// - 左右键都需要消费，避免右键点击空白时穿透到底层连接树或终端。
    fn render_connection_category_select_dismiss_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("connection-category-select-dismiss-overlay")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(Self::close_connection_dialog_category_select_from_mouse_down),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(Self::close_connection_dialog_category_select_from_mouse_down),
            )
    }

    /// 渲染 SMB 表单分类 Select 的弹窗内关闭层。
    fn render_smb_connection_category_select_dismiss_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("smb-connection-category-select-dismiss-overlay")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(Self::close_smb_connection_dialog_category_select_from_mouse_down),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(Self::close_smb_connection_dialog_category_select_from_mouse_down),
            )
    }

    /// 生成连接表单分类 Select 选项。
    fn connection_category_select_options(&self) -> Vec<SelectOption<Option<String>>> {
        let mut options = vec![SelectOption::new("root", "无分类", None)];
        for category in &self.connections.categories {
            let label = self.connection_category_path_label(category);
            options.push(SelectOption::new(
                format!("category-{}", category.id),
                label,
                Some(category.id.clone()),
            ));
        }
        options
    }

    /// 返回分类路径文案。
    fn connection_category_path_label(&self, category: &ConnectionCategory) -> String {
        let mut names = vec![category.name.clone()];
        let mut parent_id = category.parent_id.as_deref();
        let mut visited = std::collections::HashSet::new();
        while let Some(id) = parent_id {
            if !visited.insert(id.to_string()) {
                break;
            }
            let Some(parent) = self.connections.category_by_id(id) else {
                break;
            };
            names.push(parent.name.clone());
            parent_id = parent.parent_id.as_deref();
        }
        names.reverse();
        names.join(" / ")
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

    /// 渲染 SMB 连接弹窗底部操作。
    fn render_smb_connection_dialog_footer(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_end()
            .px_4()
            .pb_4()
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
                        context.listener(Self::save_smb_connection_dialog_from_mouse_down),
                    )),
            )
    }

    /// 渲染新增/编辑分类弹窗。
    fn render_connection_category_dialog(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(dialog) = self.connections.category_dialog.as_ref() else {
            return div().hidden().into_any_element();
        };
        let is_edit = matches!(dialog.mode, ConnectionCategoryDialogMode::Edit { .. });
        let title = if is_edit {
            "编辑分类"
        } else {
            "新建分类"
        };
        let focus_handle = dialog.name.focus.clone();
        self.render_connections_modal_shell("connection-category-dialog-overlay", palette, context)
            .child(
                div()
                    .id("connection-category-dialog")
                    .flex()
                    .flex_col()
                    .w(px(380.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.background))
                    .shadow_lg()
                    .child(self.render_connection_category_dialog_header(title, palette, context))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_4()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(palette.muted_text))
                                            .child("名称"),
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
                                            .key_context("connection-category-name-input")
                                            .on_key_down(context.listener(
                                                Self::handle_connection_category_name_key_down,
                                            ))
                                            .child(TextInputElement {
                                                view: context.entity(),
                                                binding: TextInputBinding::ConnectionCategoryName,
                                                focus_handle,
                                                placeholder: "",
                                                palette,
                                            }),
                                    ),
                            )
                            .when_some(dialog.error.as_ref(), |body, error| {
                                body.child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(0xb91c1c))
                                        .child(error.clone()),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .px_4()
                            .pb_4()
                            .child(self.render_modal_secondary_button(
                                "取消",
                                palette,
                                context.listener(
                                    Self::close_connection_category_dialog_from_mouse_down,
                                ),
                            ))
                            .child(self.render_modal_primary_button(
                                "保存",
                                palette,
                                context.listener(
                                    Self::save_connection_category_dialog_from_mouse_down,
                                ),
                            )),
                    ),
            )
            .into_any_element()
    }

    /// 渲染分类弹窗标题栏。
    fn render_connection_category_dialog_header(
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
                    .id("connection-category-dialog-close-button")
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
                        context.listener(Self::close_connection_category_dialog_from_mouse_down),
                    ),
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

    /// 渲染删除分类确认弹窗。
    fn render_connection_category_delete_dialog(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(dialog) = self.connections.category_delete_confirm_dialog.as_ref() else {
            return div().hidden().into_any_element();
        };
        self.render_connections_modal_shell(
            "connection-category-delete-dialog-overlay",
            palette,
            context,
        )
        .child(
            div()
                .id("connection-category-delete-dialog")
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
                        .child("删除分类"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(palette.muted_text))
                        .child(format!(
                            "仅空分类可以删除。确认删除 {}？",
                            dialog.category_name
                        )),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(self.render_modal_secondary_button(
                            "取消",
                            palette,
                            context.listener(
                                Self::close_delete_connection_category_dialog_from_mouse_down,
                            ),
                        ))
                        .child(
                            self.render_modal_danger_button(
                                "删除",
                                palette,
                                context.listener(
                                    Self::confirm_delete_connection_category_from_mouse_down,
                                ),
                            ),
                        ),
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
/// - 使用一个不绘制内容的轻量元素放在终端内容区绝对定位层中，同时回写 bounds 并注册平台输入处理器。
/// - 中文 IME 提交依赖 `ElementInputHandler`，因此终端必须像普通文本框一样把当前内容区 bounds 暴露给平台。
struct ConnectionTerminalResizeObserverElement {
    /// 主视图实体，用于在 paint 阶段回写当前 tab 的终端尺寸。
    view: Entity<MainView>,
    /// 需要同步尺寸的终端 tab ID。
    tab_id: usize,
    /// 当前 tab 的终端焦点句柄，用于注册平台输入处理器并接收中文 IME 提交。
    focus: FocusHandle,
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
        window: &mut Window,
        context: &mut App,
    ) {
        let cell_width = measure_connection_terminal_cell_width(window);
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            context,
        );
        self.view.update(context, |view, context| {
            view.sync_connection_terminal_size_from_bounds(
                self.tab_id,
                bounds,
                cell_width,
                context,
            );
        });
    }
}

/// 测量当前终端字体的真实单元格宽度。
///
/// 业务意图：
/// - 鼠标选区和 remote pty 行列都以“终端列”为单位，必须和 GPUI 实际绘制的等宽字符宽度一致。
/// - macOS/Windows 字体回退、缩放和 JetBrains Mono 版本差异都会让经验常量产生累积误差。
///
/// 边界条件：
/// - 测量失败、返回非有限值或异常小时回退到保守默认值，保证 resize 和鼠标命中不会出现非法除法。
fn measure_connection_terminal_cell_width(window: &mut Window) -> f32 {
    let mut style = window.text_style();
    // 这里必须和终端行的 `.font_family(LOG_VIEWER_FONT_FAMILY)` 保持一致。
    // 如果用系统默认 UI 字体测量，列宽会和实际等宽字体不一致，鼠标选区和 PTY resize 都会出现横向偏移。
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

/// 生成连接 hover 气泡需要的稳定文案。
///
/// 业务意图：
/// - 左侧连接树混合展示 SSH 和 SMB，但气泡布局保持一致；这里按连接 key 分发，避免渲染层再手写协议判断。
fn connection_profile_tooltip_labels(
    state: &ConnectionsWorkspaceState,
    key: &str,
) -> Option<(String, String, String)> {
    match ConnectionProfileKey::parse(key) {
        Some((ConnectionProfileKind::Ssh, profile_id)) => {
            let profile = state.profile_by_id(profile_id)?;
            Some((
                profile.id.clone(),
                ConnectionsWorkspaceState::profile_tooltip_user_label(profile),
                ConnectionsWorkspaceState::profile_tooltip_address_label(profile),
            ))
        }
        Some((ConnectionProfileKind::Smb, profile_id)) => {
            let profile = state.smb_profile_by_id(profile_id)?;
            Some((
                profile.id.clone(),
                ConnectionsWorkspaceState::smb_profile_tooltip_user_label(profile),
                ConnectionsWorkspaceState::smb_profile_tooltip_address_label(profile),
            ))
        }
        None => None,
    }
}
