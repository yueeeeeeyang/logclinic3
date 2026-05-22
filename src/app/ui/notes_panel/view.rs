// 笔记页面渲染方法。
//
// 业务意图：
// - 该文件只描述笔记树、阅读器、编辑器和确认弹窗的 GPUI 结构。
// - 状态流转、物理文件操作和输入编辑逻辑集中在 actions.rs，避免渲染路径混入持久化副作用。

use super::*;

/// 笔记树插件右键菜单渲染快照。
///
/// 业务意图：
/// - 笔记树菜单同样从运行时 manifest 读取贡献点；渲染前整理成独立结构，避免监听闭包持有 manifest 引用。
struct NotesPluginMenuRenderItem {
    /// 插件 ID。
    plugin_id: String,
    /// 菜单贡献点 ID。
    menu_id: String,
    /// 命令 ID。
    command_id: String,
    /// 菜单标题。
    title: String,
    /// 菜单图标。
    icon: Icon,
}

impl MainView {
    /// 渲染笔记页。
    pub(in crate::app) fn render_notes_page(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = self.palette();
        div()
            .id("notes-page")
            .relative()
            .flex()
            .size_full()
            .bg(rgb(palette.background))
            .on_mouse_move(context.listener(Self::handle_notes_page_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(Self::handle_notes_page_mouse_up),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(Self::handle_notes_page_mouse_up),
            )
            .child(self.render_notes_tree_panel(palette, context))
            .child(self.render_notes_workspace(palette, context))
            .when(self.notes.tree_resize_drag.is_some(), |page| {
                page.child(
                    div()
                        .id("notes-tree-resize-cursor-overlay")
                        .absolute()
                        .left(px(0.0))
                        .right(px(0.0))
                        .top(px(0.0))
                        .bottom(px(0.0))
                        .cursor_col_resize()
                        .on_mouse_move(context.listener(Self::handle_notes_page_mouse_move))
                        .on_mouse_up(
                            MouseButton::Left,
                            context.listener(Self::handle_notes_page_mouse_up),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            context.listener(Self::handle_notes_page_mouse_up),
                        ),
                )
            })
            .child(self.render_notes_unsaved_dialog(palette, context))
            .child(self.render_notes_rename_dialog(palette, context))
            .child(self.render_notes_delete_confirm_dialog(palette, context))
    }

    /// 渲染左侧笔记树面板。
    fn render_notes_tree_panel(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let border_color = if self.notes.tree_resize_drag.is_some() {
            0x94a3b8
        } else {
            palette.border
        };
        div()
            .id("notes-tree-panel")
            .relative()
            .flex()
            .flex_col()
            .w(px(self.notes.tree_width))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(rgb(border_color))
            .bg(rgb(palette.panel))
            .child(self.render_notes_tree_toolbar(palette, context))
            .child(self.render_notes_tree_body(palette, context))
            .child(self.render_notes_tree_resize_handle(context))
            .child(self.render_notes_tree_context_menu_dismiss_overlay(context))
            .child(self.render_notes_tree_create_menu(palette, context))
            .child(self.render_notes_tree_context_menu(palette, context))
    }

    /// 渲染笔记树右侧宽度拖拽命中区。
    ///
    /// UI 约束：
    /// - 可见边界仍复用树面板右边框，命中区透明覆盖在边界附近，避免新增粗分隔条挤占右侧 A4 页面空间。
    /// - 命中区从面板右侧略微外扩，保证用户在边线附近移动鼠标时更容易抓住。
    fn render_notes_tree_resize_handle(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("notes-tree-resize-handle")
            .absolute()
            .right(px(-(SPLITTER_HIT_WIDTH / 2.0)))
            .top(px(0.0))
            .bottom(px(0.0))
            .w(px(SPLITTER_HIT_WIDTH))
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.start_notes_tree_resize(event, context);
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染笔记树工具栏。
    fn render_notes_tree_toolbar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("notes-tree-toolbar")
            .flex()
            .items_center()
            .justify_between()
            .h(px(LOG_TREE_HEADER_HEIGHT))
            .px(px(LOG_TREE_ROW_HORIZONTAL_PADDING))
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(14.0))
                    .text_color(rgb(palette.text))
                    .child(Self::render_lucide_icon(
                        Some(Icon::NotebookText),
                        LOG_TREE_ITEM_ICON_WIDTH,
                        LOG_TREE_ITEM_ICON_SIZE,
                        palette.muted_text,
                    ))
                    .child("笔记"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(self.render_notes_toolbar_button(
                        "notes-refresh",
                        Icon::RefreshCw,
                        palette,
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.request_refresh_notes_tree(context);
                            context.stop_propagation();
                        }),
                    ))
                    .child(self.render_notes_toolbar_button(
                        "notes-create-menu",
                        Icon::Plus,
                        palette,
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.toggle_notes_tree_create_menu(context);
                            context.stop_propagation();
                        }),
                    )),
            )
    }

    /// 渲染笔记工具栏图标按钮。
    fn render_notes_toolbar_button(
        &self,
        id: &'static str,
        icon: Icon,
        palette: AppThemePalette,
        listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .w(px(28.0))
            .h(px(28.0))
            .rounded(px(5.0))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(Self::render_lucide_icon(
                Some(icon),
                14.0,
                14.0,
                palette.text,
            ))
            .on_click(listener)
    }

    /// 渲染笔记树主体区域。
    fn render_notes_tree_body(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("notes-tree-body")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(self.render_notes_tree_search_bar(palette, context))
            .child(self.render_notes_tree_rows(palette, context))
    }

    /// 渲染笔记树搜索框。
    ///
    /// 业务意图：
    /// - 搜索框只按左侧笔记名称过滤，不读取笔记正文，也不按目录名称命中；用户输入过程必须保持轻量。
    /// - 复用通用 `TextInputElement`，让笔记和连接的搜索框在 IME、复制粘贴、拖拽选区和长文本滚动上保持一致。
    fn render_notes_tree_search_bar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let focus_handle = self.notes.tree_search.focus.clone();
        div()
            .id("notes-tree-search-bar")
            .flex()
            .items_center()
            .gap_2()
            .mx_2()
            .my_2()
            .h(px(NOTES_TREE_SEARCH_HEIGHT))
            .px_2()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.input))
            .text_sm()
            .text_color(rgb(palette.text))
            .track_focus(&focus_handle)
            .key_context("notes-tree-search-input")
            .on_key_down(context.listener(Self::handle_notes_tree_search_key_down))
            .child(Self::render_lucide_icon(
                Some(Icon::Search),
                15.0,
                14.0,
                palette.muted_text,
            ))
            .child(TextInputElement {
                view: context.entity(),
                binding: TextInputBinding::NotesTreeSearch,
                focus_handle,
                placeholder: "搜索笔记",
                palette,
            })
            .when(!self.notes.tree_search.input.text.is_empty(), |bar| {
                bar.child(
                    div()
                        .id("notes-tree-search-clear")
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
                                view.clear_notes_tree_search(context);
                                context.stop_propagation();
                            }),
                        ),
                )
            })
    }

    /// 渲染笔记树虚拟列表。
    fn render_notes_tree_rows(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let visible_row_count = self.notes.visible_rows.len();
        div()
            .id("notes-tree-list")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            // 和连接树保持一致的列表内边距，确保 hover/选中背景不会贴住左侧面板边缘。
            .p_2()
            .child(
                uniform_list(
                    "notes-tree-virtual-list",
                    visible_row_count,
                    context.processor(
                        move |view, range: std::ops::Range<usize>, _window, context| {
                            let range_start = range.start;
                            let rows: Vec<NoteTreeRow> = range
                                .filter_map(|index| view.notes.visible_rows.get(index).cloned())
                                .collect();
                            rows.into_iter()
                                .enumerate()
                                .map(|(offset, row)| {
                                    view.render_note_tree_row_at_index(
                                        range_start + offset,
                                        row,
                                        palette,
                                        context,
                                    )
                                })
                                .collect::<Vec<_>>()
                        },
                    ),
                )
                .size_full()
                .track_scroll(self.notes.tree_scroll_handle.clone()),
            )
    }

    /// 按索引渲染笔记树行。
    fn render_note_tree_row_at_index(
        &self,
        index: usize,
        row: NoteTreeRow,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        self.render_note_tree_row(index, row, palette, context)
            .into_any_element()
    }

    /// 渲染单个笔记树行。
    fn render_note_tree_row(
        &self,
        visible_index: usize,
        row: NoteTreeRow,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = self
            .notes
            .selected
            .as_ref()
            .is_some_and(|selection| selection.id == row.id && selection.kind == row.kind);
        let can_toggle = row.kind == NoteTreeRowKind::Directory && row.has_children;
        let directory_expanded = note_tree_directory_expanded_for_view(
            &row,
            &self.notes.expanded_directory_ids,
            &self.notes.tree_search.input.text,
        );
        let expand_icon = if can_toggle {
            Some(if directory_expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            })
        } else {
            None
        };
        let item_icon = match row.kind {
            NoteTreeRowKind::Directory if directory_expanded => Icon::FolderOpen,
            NoteTreeRowKind::Directory => Icon::Folder,
            NoteTreeRowKind::Note => Icon::FileText,
        };
        let selection = NotesTreeSelection {
            id: row.id.clone(),
            kind: row.kind,
        };
        let selection_for_left_click = selection.clone();
        let selection_for_right_click = selection.clone();
        let tree_hover = palette.resource_tree_hover();
        let background = if selected { tree_hover } else { palette.panel };
        div()
            .id(SharedString::from(format!(
                "notes-tree-row-{}-{}-{visible_index}",
                match row.kind {
                    NoteTreeRowKind::Directory => "dir",
                    NoteTreeRowKind::Note => "note",
                },
                row.id
            )))
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(30.0))
            .pl(px(8.0 + row.depth as f32 * NOTES_TREE_DEPTH_INDENT))
            .pr_2()
            .mb_1()
            .rounded(px(6.0))
            .bg(rgb(background))
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |item| item.bg(rgb(tree_hover)))
            .child(Self::render_lucide_icon(
                expand_icon,
                14.0,
                13.0,
                palette.muted_text,
            ))
            .child(Self::render_lucide_icon(
                Some(item_icon),
                17.0,
                15.0,
                palette.muted_text,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(NOTES_TREE_FONT_SIZE))
                    .line_height(px(NOTES_TREE_NODE_LINE_HEIGHT))
                    .font_weight(FontWeight::NORMAL)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(row.title),
            )
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.handle_note_tree_left_mouse_down(
                        selection_for_left_click.clone(),
                        event,
                        context,
                    );
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.open_notes_tree_context_menu(
                        selection_for_right_click.clone(),
                        event,
                        context,
                    );
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染笔记树右键菜单的透明关闭遮罩。
    fn render_notes_tree_context_menu_dismiss_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if self.notes.tree_context_menu.is_none() && !self.notes.tree_create_menu_open {
            return div()
                .id("notes-tree-context-menu-dismiss-overlay-empty")
                .hidden();
        }

        div()
            .id("notes-tree-context-menu-dismiss-overlay")
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.close_notes_tree_menus(context);
                    context.stop_propagation();
                }),
            )
            .on_click(
                context.listener(|view, _event: &ClickEvent, _window, context| {
                    view.close_notes_tree_menus(context);
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染笔记树工具栏新增下拉菜单。
    ///
    /// 业务意图：
    /// - 顶部只保留一个新增图标，具体创建目录或笔记交给菜单选择，避免两个相近图标造成误点。
    /// - 菜单作为笔记树面板内的浮层绘制，配合透明遮罩消费外部点击，避免点击穿透到树行。
    fn render_notes_tree_create_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if !self.notes.tree_create_menu_open {
            return div().id("notes-tree-create-menu-empty").hidden();
        }
        div()
            .id("notes-tree-create-menu")
            .absolute()
            .right(px(LOG_TREE_ROW_HORIZONTAL_PADDING))
            .top(px(LOG_TREE_HEADER_HEIGHT - 2.0))
            .w(px(LOG_TREE_CONTEXT_MENU_WIDTH))
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
            .child(self.render_notes_tree_create_menu_item(
                NotesTreeContextMenuAction::NewDirectory,
                "新增目录",
                Icon::FolderPlus,
                palette,
                context,
            ))
            .child(self.render_notes_tree_create_menu_item(
                NotesTreeContextMenuAction::NewNote,
                "新增文件",
                Icon::FilePlus,
                palette,
                context,
            ))
    }

    /// 渲染笔记树新增菜单单项。
    fn render_notes_tree_create_menu_item(
        &self,
        action: NotesTreeContextMenuAction,
        label: &'static str,
        icon: Icon,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("notes-tree-create-{label}")))
            .flex()
            .items_center()
            .gap_2()
            .h(px(LOG_TREE_CONTEXT_MENU_ITEM_HEIGHT))
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
                    view.handle_notes_tree_create_menu_action(action.clone(), window, context);
                }),
            )
    }

    /// 渲染笔记树右键菜单。
    fn render_notes_tree_context_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = &self.notes.tree_context_menu else {
            return div().id("notes-tree-context-menu-empty").hidden();
        };
        let target_kind = menu.target.kind;
        let plugin_menu_items = self.notes_tree_plugin_menu_items();
        div()
            .id("notes-tree-context-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(LOG_TREE_CONTEXT_MENU_WIDTH))
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
            .when(target_kind == NoteTreeRowKind::Directory, |menu| {
                menu.child(self.render_notes_tree_context_menu_item(
                    NotesTreeContextMenuAction::NewDirectory,
                    "新建目录".to_string(),
                    Icon::FolderPlus,
                    palette,
                    context,
                ))
                .child(self.render_notes_tree_context_menu_item(
                    NotesTreeContextMenuAction::NewNote,
                    "新建笔记".to_string(),
                    Icon::FilePlus,
                    palette,
                    context,
                ))
            })
            .child(self.render_notes_tree_context_menu_item(
                NotesTreeContextMenuAction::Rename,
                "重命名".to_string(),
                Icon::Pencil,
                palette,
                context,
            ))
            .child(self.render_notes_tree_context_menu_item(
                NotesTreeContextMenuAction::Delete,
                "删除".to_string(),
                Icon::Trash2,
                palette,
                context,
            ))
            .children(plugin_menu_items.into_iter().map(|item| {
                self.render_notes_tree_context_menu_item(
                    NotesTreeContextMenuAction::Plugin {
                        plugin_id: item.plugin_id,
                        menu_id: item.menu_id,
                        command_id: item.command_id,
                    },
                    item.title,
                    item.icon,
                    palette,
                    context,
                )
            }))
    }

    /// 返回当前已启用插件贡献的笔记树右键菜单项。
    ///
    /// 业务意图：
    /// - 插件可以向笔记左栏追加右键菜单，但第一版只传递节点元数据，避免插件直接读取或修改笔记正文。
    /// - 加载失败和禁用插件不参与渲染，用户可在设置页查看具体原因。
    fn notes_tree_plugin_menu_items(&self) -> Vec<NotesPluginMenuRenderItem> {
        self.plugins
            .definitions
            .iter()
            .filter(|plugin| plugin.active())
            .filter_map(|plugin| {
                plugin
                    .manifest
                    .as_ref()
                    .map(|manifest| (plugin.id.clone(), manifest))
            })
            .flat_map(|(plugin_id, manifest)| {
                manifest
                    .contributes
                    .notes_tree_context_menu
                    .iter()
                    .map(move |menu| NotesPluginMenuRenderItem {
                        plugin_id: plugin_id.clone(),
                        menu_id: menu.id.clone(),
                        command_id: menu.command_id().to_string(),
                        title: menu.title.clone(),
                        icon: Self::plugin_menu_icon(menu.icon.as_deref()),
                    })
            })
            .collect()
    }

    /// 渲染笔记树右键菜单单项。
    fn render_notes_tree_context_menu_item(
        &self,
        action: NotesTreeContextMenuAction,
        label: String,
        icon: Icon,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("notes-tree-menu-{label}")))
            .flex()
            .items_center()
            .gap_2()
            .h(px(LOG_TREE_CONTEXT_MENU_ITEM_HEIGHT))
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
                    view.handle_notes_tree_context_menu_action(action.clone(), window, context);
                }),
            )
    }

    /// 渲染右侧笔记工作区。
    fn render_notes_workspace(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("notes-workspace")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(rgb(palette.background))
            .child(self.render_notes_header(palette, context))
            .child(self.render_notes_body(palette, context))
    }

    /// 渲染笔记工作区顶部栏。
    fn render_notes_header(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let title = self
            .notes
            .active_note
            .as_ref()
            .map(|note| note.title.clone())
            .unwrap_or_else(|| "选择或创建笔记".to_string());
        div()
            .id("notes-header")
            .flex()
            .items_center()
            .justify_between()
            .h(px(LOG_TREE_HEADER_HEIGHT))
            .px_4()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.background))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(title),
                    )
                    .children(self.notes.active_note.as_ref().map(|_| {
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child("文本")
                    })),
            )
            .child(self.render_notes_header_actions(palette, context))
    }

    /// 渲染笔记顶部操作区。
    fn render_notes_header_actions(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if self.notes.active_note.is_none() {
            return div().id("notes-header-actions-empty").hidden();
        }
        div()
            .id("notes-header-actions")
            .flex()
            .items_center()
            .gap_2()
            .when(!self.notes.is_editing, |actions| {
                actions.child(self.render_notes_text_button(
                    "编辑",
                    Icon::Pencil,
                    palette,
                    context.listener(|view, _event: &ClickEvent, window, context| {
                        view.start_note_editing(window, context);
                        context.stop_propagation();
                    }),
                ))
            })
            .when(self.notes.is_editing, |actions| {
                actions
                    .child(self.render_notes_text_button(
                        "取消",
                        Icon::X,
                        palette,
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.cancel_note_editing(context);
                            context.stop_propagation();
                        }),
                    ))
                    .child(self.render_notes_text_button(
                        "保存",
                        Icon::Save,
                        palette,
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.save_active_note(context);
                            context.stop_propagation();
                        }),
                    ))
            })
    }

    /// 渲染笔记文字按钮。
    ///
    /// 业务意图：
    /// - 右侧内容 header 的操作是当前笔记的轻量命令，使用和日志/HProf 工具栏一致的图标加文字按钮样式。
    /// - 不使用边框背景，避免按钮在 34px header 内显得过重；hover 时只强调文字颜色。
    fn render_notes_text_button(
        &self,
        label: &'static str,
        icon: Icon,
        palette: AppThemePalette,
        listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("notes-action-{label}")))
            .flex()
            .items_center()
            .gap_1()
            .flex_none()
            .px(px(TOOLBAR_BUTTON_HORIZONTAL_PADDING))
            .py(px(TOOLBAR_BUTTON_VERTICAL_PADDING))
            .rounded(px(6.0))
            .text_sm()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |button| button.text_color(rgb(palette.accent)))
            .active(|button| button.opacity(0.82))
            .child(Self::render_lucide_icon(
                Some(icon),
                TOOLBAR_BUTTON_ICON_WIDTH,
                TOOLBAR_BUTTON_ICON_SIZE,
                palette.muted_text,
            ))
            .child(label)
            .on_click(listener)
    }

    /// 渲染笔记主体。
    fn render_notes_body(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let body = div()
            .id("notes-body")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(palette.background));
        if let Some(error) = &self.notes.database_error {
            return body.child(self.render_notes_empty_state(error, palette));
        }
        if self.notes.active_note.is_none() {
            return body.child(self.render_notes_empty_state("创建或选择一篇笔记", palette));
        }
        if self.notes.is_editing {
            body.child(self.render_note_editor(palette, context))
        } else {
            body.child(self.render_note_reader(palette, context))
        }
    }

    /// 渲染笔记空态。
    fn render_notes_empty_state(
        &self,
        message: &str,
        palette: AppThemePalette,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("notes-empty-state")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .size_full()
            .text_color(rgb(palette.muted_text))
            .child(Self::render_lucide_icon(
                Some(Icon::NotebookText),
                34.0,
                30.0,
                palette.muted_text,
            ))
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child("笔记"),
            )
            .child(div().text_sm().child(message.to_string()))
    }

    /// 渲染笔记只读阅读器。
    fn render_note_reader(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("note-rich-reader")
            .size_full()
            .overflow_scroll()
            .scrollbar_width(px(6.0))
            .bg(rgb(palette.background))
            .track_focus(&self.notes.rich_editor.focus)
            .key_context("note-rich-reader")
            .on_key_down(
                context.listener(|view, event: &KeyDownEvent, _window, context| {
                    view.handle_note_reader_key_down(event, context);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, window, context| {
                    view.start_note_rich_text_selection(event, window, context);
                    context.stop_propagation();
                }),
            )
            .on_mouse_move(
                context.listener(|view, event: &MouseMoveEvent, _window, context| {
                    view.update_note_rich_text_selection(event, context);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    view.finish_note_rich_text_selection(context);
                }),
            )
            .on_scroll_wheel(context.listener(
                |view, event: &ScrollWheelEvent, _window, context| {
                    if view.handle_note_rich_text_scroll_wheel(event, context) {
                        context.stop_propagation();
                    }
                },
            ))
            .child(self.render_note_a4_page("note-reader", false, palette, context))
    }

    /// 渲染笔记编辑器。
    fn render_note_editor(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("note-editor")
            .flex()
            .gap_3()
            .size_full()
            .p(px(NOTES_EDITOR_PADDING))
            .child(
                // UI 约束：
                // - AI 侧边栏打开时，正文区域必须允许在横向 flex 中收缩，否则固定宽度侧栏会挤压到窗口外。
                // - 这里只约束布局收缩，不改变富文本编辑器自身的滚动和选择行为。
                div()
                    .id("note-editor-content")
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.input))
                    .track_focus(&self.notes.rich_editor.focus)
                    .key_context("note-rich-editor")
                    .on_key_down(context.listener(
                        |view, event: &KeyDownEvent, _window, context| {
                            view.handle_note_editor_container_key_down(event, context);
                        },
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, event: &MouseDownEvent, window, context| {
                            view.start_note_rich_text_selection(event, window, context);
                            context.stop_propagation();
                        }),
                    )
                    .on_mouse_move(context.listener(
                        |view, event: &MouseMoveEvent, _window, context| {
                            view.update_note_rich_text_selection(event, context);
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.finish_note_rich_text_selection(context);
                        }),
                    )
                    .child(self.render_note_rich_text_toolbar(palette, context))
                    .child(
                        div()
                            .id("note-editor-scroll")
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .overflow_scroll()
                            .scrollbar_width(px(6.0))
                            .text_color(rgb(palette.text))
                            .on_scroll_wheel(context.listener(
                                |view, event: &ScrollWheelEvent, _window, context| {
                                    if view.handle_note_rich_text_scroll_wheel(event, context) {
                                        context.stop_propagation();
                                    }
                                },
                            ))
                            .child(self.render_note_a4_page("note-editor", true, palette, context)),
                    )
                    .child(self.render_note_rich_toolbar_menus(palette, context)),
            )
            .when(self.notes.ai.is_open, |editor| {
                editor.child(self.render_notes_ai_assistant_panel(palette, context))
            })
    }

    /// 渲染笔记 AI 侧边栏。
    ///
    /// 业务意图：
    /// - 侧边栏绑定当前编辑中的笔记，每次发送前读取当前草稿 Markdown；回复只在用户点击插入后才进入编辑器。
    /// - 面板所有鼠标事件都消费，避免点击模型、消息或输入区时透传到下方富文本编辑器造成光标跳动。
    fn render_notes_ai_assistant_panel(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let can_send = self.notes_ai_can_send();
        let is_streaming = self.notes.ai.streaming_task.is_some();
        div()
            .id("notes-ai-assistant")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(NOTES_AI_ASSISTANT_WIDTH))
            .min_w(px(NOTES_AI_ASSISTANT_WIDTH))
            .h_full()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(self.render_notes_ai_header(palette, context))
            .child(self.render_notes_ai_messages(palette, context))
            .child(self.render_notes_ai_input(can_send, is_streaming, palette, context))
    }

    /// 渲染笔记 AI 侧边栏头部。
    fn render_notes_ai_header(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("notes-ai-header")
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .w_full()
            .min_w_0()
            .flex_none()
            .h(px(44.0))
            .px_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                // UI 约束：
                // - 标题区在模型名称较长时优先收缩，避免右侧模型选择和关闭按钮越界。
                // - 侧栏宽度固定，所有头部子元素都必须显式声明 min_w_0/flex_none 才能稳定截断。
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(Self::render_lucide_icon(
                        Some(Icon::Sparkles),
                        16.0,
                        15.0,
                        palette.accent,
                    ))
                    .child("AI 工具"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .flex_none()
                    .min_w_0()
                    .child(self.render_notes_ai_model_selector(palette, context))
                    .child(
                        div()
                            .id("notes-ai-close")
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(28.0))
                            .h(px(28.0))
                            .rounded(px(6.0))
                            .cursor_pointer()
                            .hover(move |button| button.bg(rgb(palette.hover)))
                            .child(Self::render_lucide_icon(
                                Some(Icon::X),
                                14.0,
                                14.0,
                                palette.muted_text,
                            ))
                            .on_click(context.listener(
                                |view, _event: &ClickEvent, _window, context| {
                                    view.stop_notes_ai_streaming_without_notify(
                                        AiChatMessageStatus::Stopped,
                                        None,
                                    );
                                    view.notes.ai.is_open = false;
                                    context.stop_propagation();
                                    context.notify();
                                },
                            )),
                    ),
            )
            .when(self.notes.ai.model_menu_open, |header| {
                header.child(self.render_notes_ai_model_menu(palette, context))
            })
    }

    /// 渲染笔记 AI 模型选择按钮。
    fn render_notes_ai_model_selector(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let label = self
            .active_notes_ai_model_profile()
            .map(|profile| profile.name)
            .unwrap_or_else(|| "选择模型".to_string());
        div()
            .id("notes-ai-model-selector")
            .flex()
            .items_center()
            .gap_1()
            .w(px(136.0))
            .flex_none()
            .h(px(28.0))
            .px_2()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.input))
            .text_xs()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .overflow_hidden()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(div().flex_none().child(Self::render_lucide_icon(
                Some(Icon::ChevronDown),
                12.0,
                12.0,
                palette.muted_text,
            )))
            .on_click(
                context.listener(|view, _event: &ClickEvent, _window, context| {
                    view.notes.ai.model_menu_open = !view.notes.ai.model_menu_open;
                    context.stop_propagation();
                    context.notify();
                }),
            )
    }

    /// 渲染笔记 AI 模型下拉菜单。
    fn render_notes_ai_model_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("notes-ai-model-menu")
            .absolute()
            .right(px(40.0))
            .top(px(38.0))
            .w(px(220.0))
            .max_h(px(240.0))
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .rounded(px(7.0))
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
            .when(self.model_config.model_config_profiles.is_empty(), |menu| {
                menu.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_xs()
                        .text_color(rgb(palette.muted_text))
                        .child("暂无模型配置"),
                )
            })
            .children(
                self.model_config
                    .model_config_profiles
                    .iter()
                    .map(|profile| {
                        let profile_id = profile.id.clone();
                        let selected = self.notes.ai.selected_model_profile_id.as_deref()
                            == Some(profile.id.as_str());
                        div()
                            .id(SharedString::from(format!("notes-ai-model-{}", profile.id)))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .text_xs()
                            .text_color(rgb(palette.text))
                            .bg(rgb(if selected {
                                palette.selected
                            } else {
                                palette.menu
                            }))
                            .cursor_pointer()
                            .hover(move |item| item.bg(rgb(palette.hover)))
                            .child(div().truncate().child(profile.name.clone()))
                            .when(selected, |item| {
                                item.child(Self::render_lucide_icon(
                                    Some(Icon::Check),
                                    12.0,
                                    12.0,
                                    palette.accent,
                                ))
                            })
                            .on_click(context.listener(
                                move |view, _event: &ClickEvent, _window, context| {
                                    view.select_notes_ai_model_profile(profile_id.clone(), context);
                                    context.stop_propagation();
                                },
                            ))
                    }),
            )
    }

    /// 渲染笔记 AI 消息区。
    fn render_notes_ai_messages(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("notes-ai-messages")
            .flex()
            .flex_col()
            .gap_2()
            .flex_1()
            .w_full()
            .min_w_0()
            .min_h_0()
            .p_3()
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .when(self.notes.ai.messages.is_empty(), |messages| {
                messages.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_3()
                        .rounded(px(7.0))
                        .border_1()
                        .border_color(rgb(palette.border))
                        .text_sm()
                        .line_height(px(20.0))
                        .text_color(rgb(palette.muted_text))
                        .child("输入生成、改写或总结要求。发送时会携带当前笔记标题和未保存的 Markdown 草稿。"),
                )
            })
            .children(
                self.notes
                    .ai
                    .messages
                    .iter()
                    .map(|message| self.render_notes_ai_message(message, palette, context)),
            )
            .children(self.notes.ai.error_message.as_ref().map(|error| {
                div()
                    .rounded(px(7.0))
                    .border_1()
                    .border_color(rgb(0xfca5a5))
                    .bg(rgb(0xfef2f2))
                    .p_2()
                    .text_xs()
                    .line_height(px(18.0))
                    .text_color(rgb(0xb91c1c))
                    .child(error.clone())
            }))
    }

    /// 渲染笔记 AI 单条消息。
    fn render_notes_ai_message(
        &self,
        message: &NotesAiMessage,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let is_user = message.role == NotesAiMessageRole::User;
        let message_id = message.id.clone();
        let status_text = match message.status {
            AiChatMessageStatus::Streaming => Some("生成中..."),
            AiChatMessageStatus::Stopped => Some("已停止"),
            AiChatMessageStatus::Failed => message.error_message.as_deref().or(Some("请求失败")),
            AiChatMessageStatus::Complete => None,
        };
        div()
            .id(SharedString::from(format!(
                "notes-ai-message-{}",
                message.id
            )))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(if is_user {
                palette.accent
            } else {
                palette.border
            }))
            .bg(rgb(if is_user {
                palette.selected
            } else {
                palette.background
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(if is_user {
                        palette.accent
                    } else {
                        palette.muted_text
                    }))
                    .child(if is_user { "我" } else { "AI" })
                    .children(status_text.map(|status| {
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(rgb(palette.muted_text))
                            .child(status.to_string())
                    })),
            )
            .child(
                div()
                    .text_sm()
                    .line_height(px(20.0))
                    .text_color(rgb(palette.text))
                    .child(if message.content.is_empty() {
                        "等待模型响应".to_string()
                    } else {
                        message.content.clone()
                    }),
            )
            .when(
                !is_user
                    && !message.content.trim().is_empty()
                    && matches!(
                        message.status,
                        AiChatMessageStatus::Complete | AiChatMessageStatus::Stopped
                    ),
                |card| {
                    card.child(
                        div()
                            .flex()
                            .justify_end()
                            .child(self.render_notes_ai_small_button(
                                "插入",
                                Icon::CornerDownLeft,
                                palette,
                                context.listener(
                                    move |view, _event: &ClickEvent, window, context| {
                                        view.insert_notes_ai_message_into_editor(
                                            message_id.clone(),
                                            window,
                                            context,
                                        );
                                        context.stop_propagation();
                                    },
                                ),
                            )),
                    )
                },
            )
    }

    /// 渲染笔记 AI 输入框。
    fn render_notes_ai_input(
        &self,
        can_send: bool,
        is_streaming: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("notes-ai-input-wrap")
            .w_full()
            .min_w_0()
            .flex_none()
            .p_3()
            .border_t_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .id("notes-ai-input")
                    .relative()
                    .w_full()
                    .min_w_0()
                    .flex_none()
                    .h(px(NOTES_AI_INPUT_HEIGHT))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.input))
                    .track_focus(&self.notes.ai.input_focus)
                    .key_context("notes-ai-input")
                    .on_key_down(context.listener(
                        |view, event: &KeyDownEvent, _window, context| {
                            view.handle_notes_ai_input_key_down(event, context);
                        },
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, event: &MouseDownEvent, window, context| {
                            view.start_notes_ai_input_mouse_selection(event, context);
                            window.focus(&view.notes.ai.input_focus);
                            context.stop_propagation();
                        }),
                    )
                    .on_mouse_move(context.listener(
                        |view, event: &MouseMoveEvent, _window, context| {
                            view.update_notes_ai_input_mouse_selection(event.position, context);
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.finish_notes_ai_input_mouse_selection(context);
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.finish_notes_ai_input_mouse_selection(context);
                        }),
                    )
                    .child(
                        div()
                            .id("notes-ai-input-scroll")
                            .size_full()
                            .min_w_0()
                            .px_3()
                            .pt(px(8.0))
                            .pb(px(NOTES_AI_INPUT_BOTTOM_PADDING))
                            .overflow_y_scroll()
                            .scrollbar_width(px(6.0))
                            .text_size(px(14.0))
                            .line_height(px(NOTES_AI_INPUT_LINE_HEIGHT))
                            .text_color(rgb(palette.text))
                            .child(NotesAiInputElement {
                                view: context.entity(),
                                focus_handle: self.notes.ai.input_focus.clone(),
                                placeholder: "输入要求，Enter 发送，Shift+Enter 换行",
                                palette,
                            }),
                    )
                    .child(self.render_notes_ai_input_controls(
                        can_send,
                        is_streaming,
                        palette,
                        context,
                    )),
            )
    }

    /// 渲染笔记 AI 输入框底部控件。
    fn render_notes_ai_input_controls(
        &self,
        can_send: bool,
        is_streaming: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("notes-ai-input-controls")
            .absolute()
            .left(px(8.0))
            .right(px(8.0))
            .bottom(px(8.0))
            .flex()
            .items_center()
            .justify_end()
            .min_w_0()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(self.render_notes_ai_send_button(can_send, is_streaming, palette, context))
    }

    /// 渲染笔记 AI 发送或停止按钮。
    fn render_notes_ai_send_button(
        &self,
        can_send: bool,
        is_streaming: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let enabled = can_send || is_streaming;
        div()
            .id("notes-ai-send-button")
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .h(px(30.0))
            .px_3()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(if enabled {
                palette.accent
            } else {
                palette.border
            }))
            .bg(rgb(if enabled {
                palette.accent
            } else {
                palette.panel
            }))
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(if enabled {
                palette.on_accent
            } else {
                palette.muted_text
            }))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.accent_hover)))
            })
            .when(!enabled, |button| button.opacity(0.55))
            .child(Self::render_lucide_icon(
                Some(if is_streaming { Icon::X } else { Icon::Send }),
                13.0,
                13.0,
                if enabled {
                    palette.on_accent
                } else {
                    palette.muted_text
                },
            ))
            .child(if is_streaming { "停止" } else { "发送" })
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    if is_streaming {
                        view.stop_notes_ai_streaming(context);
                    } else if can_send {
                        view.start_notes_ai_send(context);
                    }
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染笔记 AI 轻量操作按钮。
    fn render_notes_ai_small_button(
        &self,
        label: &'static str,
        icon: Icon,
        palette: AppThemePalette,
        listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("notes-ai-small-button-{label}")))
            .flex()
            .items_center()
            .gap_1()
            .h(px(28.0))
            .px_2()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.input))
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(palette.accent))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(Self::render_lucide_icon(
                Some(icon),
                12.0,
                12.0,
                palette.accent,
            ))
            .child(label)
            .on_click(listener)
    }

    /// 渲染笔记正文使用的 A4 纸张区域。
    ///
    /// 业务意图：
    /// - 编辑态和预览态都需要像文档一样有清晰页面边界，避免长段落随着窗口变宽而无限延展。
    /// - 两种状态共用同一套纸张容器，保证编辑后的换行和预览看到的版心一致。
    /// - 外层内容宽度包含左右安全留白，窄窗口时由滚动容器提供横向滚动，宽窗口时纸张保持居中。
    ///
    /// 边界条件：
    /// - A4 高度只作为最小视觉高度；正文超过一页时富文本元素会继续撑高纸张，不做分页裁剪。
    /// - 代码块语言菜单只在编辑态出现，并且必须放在纸张容器内使用同一坐标系，避免居中和横向滚动后错位。
    fn render_note_a4_page(
        &self,
        id_prefix: &'static str,
        editable: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let page = div()
            .id(SharedString::from(format!("{id_prefix}-a4-page")))
            .relative()
            .w(px(NOTES_A4_PAGE_WIDTH))
            .min_h(px(NOTES_A4_PAGE_HEIGHT))
            .flex_none()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_md()
            .child(NoteRichTextElement {
                view: context.entity(),
                editable,
                paper_layout: true,
                palette,
                theme: self.effective_theme(),
            });
        let page = if editable {
            page.child(self.render_note_rich_code_language_menu(palette, context))
        } else {
            page
        };

        div()
            .id(SharedString::from(format!("{id_prefix}-scroll-content")))
            .relative()
            .flex()
            .justify_center()
            .items_start()
            .min_w(px(NOTES_A4_PAGE_WIDTH + NOTES_A4_PAGE_GUTTER * 2.0))
            .w_full()
            .px(px(NOTES_A4_PAGE_GUTTER))
            .py(px(NOTES_A4_PAGE_VERTICAL_MARGIN))
            .child(page)
    }

    /// 渲染富文本编辑工具栏。
    fn render_note_rich_text_toolbar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        // 工具栏是正文编辑器的一部分：固定在编辑框顶部，共用编辑器边框和背景，
        // 这样标题输入、工具栏、正文之间的层级清晰，也避免浮层点击透传成正文选区变更。
        div()
            .id("note-rich-toolbar")
            .relative()
            .flex()
            .items_center()
            .gap_1()
            .h(px(NOTES_RICH_TEXT_TOOLBAR_HEIGHT))
            .flex_none()
            .w_full()
            .px_2()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.input))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(self.render_note_rich_font_size_button(palette, context))
            .child(
                self.render_note_rich_toolbar_icon(
                    "bold",
                    Icon::Bold,
                    self.notes
                        .rich_editor
                        .current_inline_command_active(NoteRichTextInlineCommand::Bold),
                    palette,
                    context.listener(|view, _event: &ClickEvent, _window, context| {
                        view.toggle_note_rich_text_inline_style(
                            NoteRichTextInlineCommand::Bold,
                            context,
                        );
                        context.stop_propagation();
                    }),
                ),
            )
            .child(
                self.render_note_rich_toolbar_icon(
                    "italic",
                    Icon::Italic,
                    self.notes
                        .rich_editor
                        .current_inline_command_active(NoteRichTextInlineCommand::Italic),
                    palette,
                    context.listener(|view, _event: &ClickEvent, _window, context| {
                        view.toggle_note_rich_text_inline_style(
                            NoteRichTextInlineCommand::Italic,
                            context,
                        );
                        context.stop_propagation();
                    }),
                ),
            )
            .child(
                self.render_note_rich_toolbar_icon(
                    "underline",
                    Icon::Underline,
                    self.notes
                        .rich_editor
                        .current_inline_command_active(NoteRichTextInlineCommand::Underline),
                    palette,
                    context.listener(|view, _event: &ClickEvent, _window, context| {
                        view.toggle_note_rich_text_inline_style(
                            NoteRichTextInlineCommand::Underline,
                            context,
                        );
                        context.stop_propagation();
                    }),
                ),
            )
            .child(
                self.render_note_rich_toolbar_icon(
                    "strikethrough",
                    Icon::Strikethrough,
                    self.notes
                        .rich_editor
                        .current_inline_command_active(NoteRichTextInlineCommand::Strikethrough),
                    palette,
                    context.listener(|view, _event: &ClickEvent, _window, context| {
                        view.toggle_note_rich_text_inline_style(
                            NoteRichTextInlineCommand::Strikethrough,
                            context,
                        );
                        context.stop_propagation();
                    }),
                ),
            )
            .child(self.render_note_rich_color_button(palette, context))
            .child(self.render_note_rich_background_color_button(palette, context))
            .child(self.render_note_rich_toolbar_icon(
                "code-block",
                Icon::Code,
                self.notes.rich_editor.current_block_is_code(),
                palette,
                context.listener(|view, _event: &ClickEvent, _window, context| {
                    view.toggle_note_rich_text_code_block(context);
                    context.stop_propagation();
                }),
            ))
            .child(self.render_note_rich_toolbar_icon(
                "bullet-list",
                Icon::List,
                false,
                palette,
                context.listener(|view, _event: &ClickEvent, _window, context| {
                    view.toggle_note_rich_text_list(
                        NoteRichTextBlockKind::UnorderedListItem,
                        context,
                    );
                    context.stop_propagation();
                }),
            ))
            .child(self.render_note_rich_toolbar_icon(
                "ordered-list",
                Icon::ListOrdered,
                false,
                palette,
                context.listener(|view, _event: &ClickEvent, _window, context| {
                    view.toggle_note_rich_text_list(
                        NoteRichTextBlockKind::OrderedListItem,
                        context,
                    );
                    context.stop_propagation();
                }),
            ))
            .child(self.render_note_rich_toolbar_icon(
                "ai",
                Icon::Sparkles,
                self.notes.ai.is_open,
                palette,
                context.listener(|view, _event: &ClickEvent, window, context| {
                    view.toggle_notes_ai_assistant(window, context);
                    context.stop_propagation();
                }),
            ))
    }

    /// 渲染富文本工具栏浮层菜单。
    fn render_note_rich_toolbar_menus(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let menu_open = self.notes.rich_editor.font_size_menu_open
            || self.notes.rich_editor.color_menu_open
            || self.notes.rich_editor.background_color_menu_open;
        if !menu_open {
            return div().id("note-rich-toolbar-menus-empty").hidden();
        }
        // 下拉菜单必须作为编辑器容器最后绘制的浮层，而不是工具栏内部子节点。
        // GPUI 同级元素按子节点顺序绘制；如果菜单留在工具栏内，后绘制的正文滚动区会覆盖菜单。
        div()
            .id("note-rich-toolbar-menus")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .child(self.render_note_rich_toolbar_menu_backdrop(context))
            .child(self.render_note_rich_font_size_menu(palette, context))
            .child(self.render_note_rich_color_menu(palette, context))
            .child(self.render_note_rich_background_color_menu(palette, context))
    }

    /// 渲染富文本工具栏菜单透明遮罩。
    fn render_note_rich_toolbar_menu_backdrop(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("note-rich-toolbar-menu-backdrop")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .bg(rgba(0x00000000))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.close_note_rich_text_menus(context);
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染富文本工具栏图标按钮。
    fn render_note_rich_toolbar_icon(
        &self,
        id: &'static str,
        icon: Icon,
        active: bool,
        palette: AppThemePalette,
        listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("note-rich-toolbar-{id}")))
            .flex()
            .items_center()
            .justify_center()
            .w(px(30.0))
            .h(px(28.0))
            .rounded(px(6.0))
            .bg(rgb(if active {
                palette.selected
            } else {
                palette.panel
            }))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(Self::render_lucide_icon(
                Some(icon),
                16.0,
                15.0,
                if active { palette.accent } else { palette.text },
            ))
            .on_click(listener)
    }

    /// 渲染字号菜单按钮。
    fn render_note_rich_font_size_button(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let size = self.notes.rich_editor.pending_style.font_size_px;
        div()
            .id("note-rich-font-size")
            .flex()
            .items_center()
            .gap_1()
            .h(px(28.0))
            .px_2()
            .rounded(px(6.0))
            .text_sm()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(Self::render_lucide_icon(
                Some(Icon::Type),
                16.0,
                15.0,
                palette.muted_text,
            ))
            .child(format!("{size}px"))
            .on_click(
                context.listener(|view, _event: &ClickEvent, _window, context| {
                    view.notes.rich_editor.font_size_menu_open =
                        !view.notes.rich_editor.font_size_menu_open;
                    view.notes.rich_editor.color_menu_open = false;
                    view.notes.rich_editor.background_color_menu_open = false;
                    view.notes.rich_editor.code_language_menu_open = false;
                    view.notes.rich_editor.code_language_menu_anchor = None;
                    context.stop_propagation();
                    context.notify();
                }),
            )
    }

    /// 渲染字号下拉菜单。
    fn render_note_rich_font_size_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if !self.notes.rich_editor.font_size_menu_open {
            return div().id("note-rich-font-size-menu-empty").hidden();
        }
        div()
            .id("note-rich-font-size-menu")
            .absolute()
            .left(px(8.0))
            .top(px(NOTES_RICH_TEXT_TOOLBAR_HEIGHT - 2.0))
            .w(px(94.0))
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
            .children([12_u32, 14, 16, 18, 20, 24, 32].into_iter().map(|size| {
                div()
                    .id(SharedString::from(format!("note-rich-font-size-{size}")))
                    .h(px(28.0))
                    .flex()
                    .items_center()
                    .px_3()
                    .text_sm()
                    .text_color(rgb(palette.text))
                    .cursor_pointer()
                    .hover(move |item| item.bg(rgb(palette.hover)))
                    .child(format!("{size}px"))
                    .on_click(context.listener(
                        move |view, _event: &ClickEvent, _window, context| {
                            view.apply_note_rich_text_font_size(size, context);
                            context.stop_propagation();
                        },
                    ))
            }))
    }

    /// 渲染文字颜色按钮。
    fn render_note_rich_color_button(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.render_note_rich_toolbar_icon(
            "color",
            Icon::Palette,
            self.notes.rich_editor.color_menu_open,
            palette,
            context.listener(|view, _event: &ClickEvent, _window, context| {
                view.notes.rich_editor.color_menu_open = !view.notes.rich_editor.color_menu_open;
                view.notes.rich_editor.font_size_menu_open = false;
                view.notes.rich_editor.background_color_menu_open = false;
                view.notes.rich_editor.code_language_menu_open = false;
                view.notes.rich_editor.code_language_menu_anchor = None;
                context.stop_propagation();
                context.notify();
            }),
        )
    }

    /// 渲染背景颜色按钮。
    fn render_note_rich_background_color_button(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.render_note_rich_toolbar_icon(
            "background-color",
            Icon::Highlighter,
            self.notes.rich_editor.background_color_menu_open,
            palette,
            context.listener(|view, _event: &ClickEvent, _window, context| {
                view.notes.rich_editor.background_color_menu_open =
                    !view.notes.rich_editor.background_color_menu_open;
                view.notes.rich_editor.font_size_menu_open = false;
                view.notes.rich_editor.color_menu_open = false;
                view.notes.rich_editor.code_language_menu_open = false;
                view.notes.rich_editor.code_language_menu_anchor = None;
                context.stop_propagation();
                context.notify();
            }),
        )
    }

    /// 渲染文字颜色色板。
    fn render_note_rich_color_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if !self.notes.rich_editor.color_menu_open {
            return div().id("note-rich-color-menu-empty").hidden();
        }
        let colors = [
            ("theme", None),
            ("red", Some(NoteRichTextColor::rgb(220, 38, 38))),
            ("orange", Some(NoteRichTextColor::rgb(234, 88, 12))),
            ("green", Some(NoteRichTextColor::rgb(22, 163, 74))),
            ("blue", Some(NoteRichTextColor::rgb(37, 99, 235))),
            ("purple", Some(NoteRichTextColor::rgb(124, 58, 237))),
        ];
        div()
            .id("note-rich-color-menu")
            .absolute()
            .left(px(174.0))
            .top(px(NOTES_RICH_TEXT_TOOLBAR_HEIGHT - 2.0))
            .flex()
            .gap_1()
            .p_2()
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
            .children(colors.into_iter().map(|(name, color)| {
                let swatch = color
                    .map(|color| {
                        rgb(((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32)
                    })
                    .unwrap_or_else(|| rgb(palette.text));
                div()
                    .id(SharedString::from(format!("note-rich-color-{name}")))
                    .w(px(22.0))
                    .h(px(22.0))
                    .rounded(px(11.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(swatch)
                    .cursor_pointer()
                    .on_click(context.listener(
                        move |view, _event: &ClickEvent, _window, context| {
                            view.apply_note_rich_text_color(color, context);
                            context.stop_propagation();
                        },
                    ))
            }))
    }

    /// 渲染背景颜色色板。
    fn render_note_rich_background_color_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if !self.notes.rich_editor.background_color_menu_open {
            return div().id("note-rich-background-color-menu-empty").hidden();
        }
        let colors = [
            ("clear", None),
            ("yellow", Some(NoteRichTextColor::rgb(254, 240, 138))),
            ("green", Some(NoteRichTextColor::rgb(187, 247, 208))),
            ("blue", Some(NoteRichTextColor::rgb(191, 219, 254))),
            ("purple", Some(NoteRichTextColor::rgb(221, 214, 254))),
            ("pink", Some(NoteRichTextColor::rgb(251, 207, 232))),
        ];
        div()
            .id("note-rich-background-color-menu")
            .absolute()
            .left(px(212.0))
            .top(px(NOTES_RICH_TEXT_TOOLBAR_HEIGHT - 2.0))
            .flex()
            .gap_1()
            .p_2()
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
            .children(colors.into_iter().map(|(name, color)| {
                let swatch = color
                    .map(|color| {
                        rgb(((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32)
                    })
                    .unwrap_or_else(|| rgb(palette.input));
                div()
                    .id(SharedString::from(format!(
                        "note-rich-background-color-{name}"
                    )))
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(22.0))
                    .h(px(22.0))
                    .rounded(px(11.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(swatch)
                    .cursor_pointer()
                    .child(div().when(color.is_none(), |marker| {
                        marker.w(px(12.0)).h(px(1.5)).bg(rgb(palette.muted_text))
                    }))
                    .on_click(context.listener(
                        move |view, _event: &ClickEvent, _window, context| {
                            view.apply_note_rich_text_background_color(color, context);
                            context.stop_propagation();
                        },
                    ))
            }))
    }

    /// 渲染代码块语言下拉菜单。
    fn render_note_rich_code_language_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if !self.notes.rich_editor.code_language_menu_open {
            return div().id("note-rich-code-language-menu-empty").hidden();
        }
        let (left, top) = self.notes.rich_editor.code_language_menu_position();
        div()
            .id("note-rich-code-language-menu")
            .absolute()
            .left(left)
            .top(top)
            .w(px(132.0))
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
            .children(NOTES_CODE_BLOCK_LANGUAGES.iter().copied().map(|language| {
                div()
                    .id(SharedString::from(format!(
                        "note-rich-code-language-{language}"
                    )))
                    .h(px(28.0))
                    .flex()
                    .items_center()
                    .px_3()
                    .text_sm()
                    .text_color(rgb(palette.text))
                    .cursor_pointer()
                    .hover(move |item| item.bg(rgb(palette.hover)))
                    .child(language.to_string())
                    .on_click(context.listener(
                        move |view, _event: &ClickEvent, _window, context| {
                            view.apply_note_rich_text_code_language(language, context);
                            context.stop_propagation();
                        },
                    ))
            }))
    }

    /// 渲染未保存修改确认弹窗。
    fn render_notes_unsaved_dialog(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if self.notes.unsaved_dialog.is_none() {
            return div().id("notes-unsaved-dialog-empty").hidden();
        }
        self.render_notes_confirm_dialog(
            "notes-unsaved-dialog",
            "笔记有未保存修改",
            "保存当前修改后继续，或放弃修改。",
            palette,
            vec![
                ("保存", NotesUnsavedChoice::Save),
                ("放弃", NotesUnsavedChoice::Discard),
                ("取消", NotesUnsavedChoice::Cancel),
            ],
            context,
        )
    }

    /// 渲染删除确认弹窗。
    fn render_notes_delete_confirm_dialog(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(dialog) = &self.notes.delete_confirm_dialog else {
            return div().id("notes-delete-dialog-empty").hidden();
        };
        let message = match dialog.target.kind {
            NoteTreeRowKind::Directory => {
                format!("删除目录“{}”会同时删除其子目录和笔记。", dialog.title)
            }
            NoteTreeRowKind::Note => format!("删除笔记“{}”后无法从应用内恢复。", dialog.title),
        };
        self.render_notes_delete_dialog_content(&message, palette, context)
    }

    /// 渲染重命名弹窗。
    fn render_notes_rename_dialog(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(dialog) = &self.notes.rename_dialog else {
            return div().id("notes-rename-dialog-empty").hidden();
        };
        let title = match dialog.target.kind {
            NoteTreeRowKind::Directory => "重命名目录",
            NoteTreeRowKind::Note => "重命名笔记",
        };
        let mut backdrop = rgb(0x000000);
        backdrop.a = 0.34;
        div()
            .id("notes-rename-dialog")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(backdrop)
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(420.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.menu))
                    .shadow_lg()
                    .p_4()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(title),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_sm()
                            .line_height(px(20.0))
                            .text_color(rgb(palette.muted_text))
                            .child("输入新的名称，按 Enter 保存。"),
                    )
                    .child(
                        div()
                            .id("notes-rename-input")
                            .mt_3()
                            .h(px(34.0))
                            .flex()
                            .items_center()
                            .px_3()
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(rgb(palette.border))
                            .bg(rgb(palette.input))
                            .text_sm()
                            .text_color(rgb(palette.text))
                            .track_focus(&self.notes.title_focus)
                            .key_context("note-rename")
                            .on_key_down(context.listener(
                                |view, event: &KeyDownEvent, _window, context| {
                                    view.handle_note_title_key_down(event, context);
                                },
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                context.listener(
                                    |view, event: &MouseDownEvent, window, context| {
                                        view.start_note_title_mouse_selection(event, context);
                                        window.focus(&view.notes.title_focus);
                                        context.stop_propagation();
                                    },
                                ),
                            )
                            .child(NoteTitleElement {
                                view: context.entity(),
                                focus_handle: self.notes.title_focus.clone(),
                                palette,
                                placeholder: "输入名称",
                            }),
                    )
                    .child(
                        div()
                            .mt_4()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(self.render_notes_dialog_button(
                                "取消",
                                palette,
                                context.listener(|view, _event: &ClickEvent, _window, context| {
                                    view.cancel_note_rename(context);
                                    context.stop_propagation();
                                }),
                            ))
                            .child(self.render_notes_dialog_button(
                                "保存",
                                palette,
                                context.listener(|view, _event: &ClickEvent, _window, context| {
                                    view.confirm_note_rename(context);
                                    context.stop_propagation();
                                }),
                            )),
                    ),
            )
    }

    /// 渲染删除确认内容。
    fn render_notes_delete_dialog_content(
        &self,
        message: &str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut backdrop = rgb(0x000000);
        backdrop.a = 0.34;
        div()
            .id("notes-delete-dialog")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(backdrop)
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(380.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.menu))
                    .shadow_lg()
                    .p_4()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child("确认删除"),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_sm()
                            .line_height(px(20.0))
                            .text_color(rgb(palette.muted_text))
                            .child(message.to_string()),
                    )
                    .child(
                        div()
                            .mt_4()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(self.render_notes_dialog_button(
                                "取消",
                                palette,
                                context.listener(|view, _event: &ClickEvent, _window, context| {
                                    view.cancel_note_delete(context);
                                    context.stop_propagation();
                                }),
                            ))
                            .child(self.render_notes_dialog_button(
                                "删除",
                                palette,
                                context.listener(|view, _event: &ClickEvent, _window, context| {
                                    view.confirm_note_delete(context);
                                    context.stop_propagation();
                                }),
                            )),
                    ),
            )
    }

    /// 渲染未保存确认通用弹窗。
    fn render_notes_confirm_dialog(
        &self,
        id: &'static str,
        title: &'static str,
        message: &'static str,
        palette: AppThemePalette,
        choices: Vec<(&'static str, NotesUnsavedChoice)>,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut backdrop = rgb(0x000000);
        backdrop.a = 0.34;
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
            .bg(backdrop)
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(380.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.menu))
                    .shadow_lg()
                    .p_4()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(title),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_sm()
                            .line_height(px(20.0))
                            .text_color(rgb(palette.muted_text))
                            .child(message),
                    )
                    .child(div().mt_4().flex().justify_end().gap_2().children(
                        choices.into_iter().map(|(label, choice)| {
                            self.render_notes_dialog_button(
                                label,
                                palette,
                                context.listener(
                                    move |view, _event: &ClickEvent, window, context| {
                                        view.resolve_notes_unsaved_dialog(choice, window, context);
                                        context.stop_propagation();
                                    },
                                ),
                            )
                        }),
                    )),
            )
    }

    /// 渲染确认弹窗按钮。
    fn render_notes_dialog_button(
        &self,
        label: &'static str,
        palette: AppThemePalette,
        listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("notes-dialog-{label}")))
            .px_3()
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .text_sm()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(label)
            .on_click(listener)
    }
}
