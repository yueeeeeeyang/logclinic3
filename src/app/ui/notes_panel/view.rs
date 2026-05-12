// 笔记页面渲染方法。
//
// 业务意图：
// - 该文件只描述笔记树、阅读器、编辑器和确认弹窗的 GPUI 结构。
// - 状态流转、SQLite 操作和输入编辑逻辑集中在 actions.rs，避免渲染路径混入持久化副作用。

use super::*;

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
            .child(self.render_notes_tree_panel(palette, context))
            .child(self.render_notes_workspace(palette, context))
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
        div()
            .id("notes-tree-panel")
            .relative()
            .flex()
            .flex_col()
            .w(px(NOTES_TREE_WIDTH))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .child(self.render_notes_tree_toolbar(palette, context))
            .child(self.render_notes_tree_body(palette, context))
            .child(self.render_notes_tree_context_menu_dismiss_overlay(context))
            .child(self.render_notes_tree_context_menu(palette, context))
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
                        "notes-new-directory",
                        Icon::FolderPlus,
                        palette,
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.create_note_directory_from_toolbar(context);
                            context.stop_propagation();
                        }),
                    ))
                    .child(self.render_notes_toolbar_button(
                        "notes-new-note",
                        Icon::FilePlus,
                        palette,
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.create_note_in_selected_directory(context);
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
        let visible_row_count = self.notes.visible_rows.len();
        div()
            .id("notes-tree-list")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
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
        let expand_icon = if can_toggle {
            Some(if self.notes.expanded_directory_ids.contains(&row.id) {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            })
        } else {
            None
        };
        let item_icon = match row.kind {
            NoteTreeRowKind::Directory if self.notes.expanded_directory_ids.contains(&row.id) => {
                Icon::FolderOpen
            }
            NoteTreeRowKind::Directory => Icon::Folder,
            NoteTreeRowKind::Note => Icon::FileText,
        };
        let selection = NotesTreeSelection {
            id: row.id.clone(),
            kind: row.kind,
        };
        let selection_for_left_click = selection.clone();
        let selection_for_right_click = selection.clone();
        let background = if selected {
            palette.selected
        } else {
            palette.panel
        };
        let hover_background =
            Self::log_tree_row_hover_background(selected, self.effective_theme());
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
            .gap_1()
            .h(px(LOG_TREE_ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .pl(px(
                LOG_TREE_ROW_HORIZONTAL_PADDING + row.depth as f32 * LOG_TREE_ROW_INDENT
            ))
            .pr(px(LOG_TREE_ROW_HORIZONTAL_PADDING))
            .bg(rgb(background))
            .text_size(px(LOG_TREE_FONT_SIZE))
            .text_color(rgb(if selected {
                palette.accent
            } else {
                palette.text
            }))
            .cursor_pointer()
            .hover(move |item| item.bg(rgb(hover_background)))
            .child(Self::render_lucide_icon(
                expand_icon,
                LOG_TREE_CHEVRON_WIDTH,
                LOG_TREE_CHEVRON_SIZE,
                palette.muted_text,
            ))
            .child(Self::render_lucide_icon(
                Some(item_icon),
                LOG_TREE_ITEM_ICON_WIDTH,
                LOG_TREE_ITEM_ICON_SIZE,
                palette.muted_text,
            ))
            .child(div().flex_1().min_w_0().truncate().child(row.title))
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
        if self.notes.tree_context_menu.is_none() {
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
                    view.close_notes_tree_context_menu(context);
                    context.stop_propagation();
                }),
            )
            .on_click(
                context.listener(|view, _event: &ClickEvent, _window, context| {
                    view.close_notes_tree_context_menu(context);
                    context.stop_propagation();
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
                    "新建目录",
                    Icon::FolderPlus,
                    palette,
                    context,
                ))
                .child(self.render_notes_tree_context_menu_item(
                    NotesTreeContextMenuAction::NewNote,
                    "新建笔记",
                    Icon::FilePlus,
                    palette,
                    context,
                ))
            })
            .child(self.render_notes_tree_context_menu_item(
                NotesTreeContextMenuAction::Rename,
                "重命名",
                Icon::Pencil,
                palette,
                context,
            ))
            .child(self.render_notes_tree_context_menu_item(
                NotesTreeContextMenuAction::Delete,
                "删除",
                Icon::Trash2,
                palette,
                context,
            ))
    }

    /// 渲染笔记树右键菜单单项。
    fn render_notes_tree_context_menu_item(
        &self,
        action: NotesTreeContextMenuAction,
        label: &'static str,
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
                    view.handle_notes_tree_context_menu_action(action, window, context);
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
                    .children(self.notes.active_note.as_ref().map(|note| {
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child(note.content_format.label())
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
        let Some(note) = self.notes.active_note.as_ref() else {
            return div().id("notes-header-actions-empty").hidden();
        };
        div()
            .id("notes-header-actions")
            .flex()
            .items_center()
            .gap_2()
            .when(
                !self.notes.is_editing && note.content_format == NoteContentFormat::Markdown,
                |actions| {
                    actions.child(self.render_notes_text_button(
                        if self.notes.reader_mode == NoteReaderMode::Preview {
                            "源码"
                        } else {
                            "预览"
                        },
                        Icon::BookOpenText,
                        palette,
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.toggle_note_reader_mode(context);
                            context.stop_propagation();
                        }),
                    ))
                },
            )
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
                        self.notes.editor_format.label(),
                        Icon::FileText,
                        palette,
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.toggle_note_editor_format(context);
                            context.stop_propagation();
                        }),
                    ))
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
        let note = self.notes.active_note.as_ref().expect("已检查存在笔记");
        if note.content_format == NoteContentFormat::Markdown
            && self.notes.reader_mode == NoteReaderMode::Preview
        {
            let theme = self.effective_theme();
            let document = self.note_markdown_preview_document(note, theme);
            return div()
                .id("note-markdown-preview")
                .size_full()
                .overflow_y_scroll()
                .scrollbar_width(px(6.0))
                .px_6()
                .py_5()
                .child(render_app_markdown_document(&document, &note.id, palette));
        }
        self.render_note_source_reader(&note.content, palette, context)
    }

    /// 渲染只读源码阅读器。
    fn render_note_source_reader(
        &self,
        content: &str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let lines = Self::note_source_lines(content);
        if lines.is_empty() {
            return div()
                .id("note-source-empty")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(rgb(palette.muted_text))
                .child("空笔记");
        }
        div()
            .id("note-source-reader")
            .size_full()
            .overflow_scroll()
            .scrollbar_width(px(6.0))
            .font_family(LOG_VIEWER_FONT_FAMILY)
            .text_size(px(NOTES_TEXT_FONT_SIZE))
            .line_height(px(NOTES_TEXT_LINE_HEIGHT))
            .text_color(rgb(palette.text))
            .py_4()
            .children(
                lines.into_iter().enumerate().map(|(index, line)| {
                    self.render_note_source_line(index, line, palette, context)
                }),
            )
    }

    /// 渲染只读源码单行。
    fn render_note_source_line(
        &self,
        line_index: usize,
        line: &str,
        _palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let highlights = self
            .notes
            .source_selection
            .as_ref()
            .and_then(|selection| {
                Self::selected_byte_range_for_note_line(selection, line_index, line)
            })
            .map(|range| vec![(range, Self::log_text_selection_highlight_style())])
            .unwrap_or_default();
        let line_text = if line.is_empty() { " " } else { line };
        let line_for_down = line.to_string();
        let line_for_move = line.to_string();
        div()
            .id(SharedString::from(format!("note-source-line-{line_index}")))
            .h(px(NOTES_TEXT_LINE_HEIGHT))
            .px(px(NOTES_SOURCE_HORIZONTAL_PADDING))
            .whitespace_nowrap()
            .child(StyledText::new(line_text.to_string()).with_highlights(highlights))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, window, context| {
                    view.start_note_source_selection(
                        line_index,
                        &line_for_down,
                        event,
                        window,
                        context,
                    );
                    context.stop_propagation();
                }),
            )
            .on_mouse_move(context.listener(
                move |view, event: &MouseMoveEvent, _window, context| {
                    view.update_note_source_selection(line_index, &line_for_move, event, context);
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    view.finish_note_source_selection(context);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    view.finish_note_source_selection(context);
                }),
            )
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
            .flex_col()
            .size_full()
            .p(px(NOTES_EDITOR_PADDING))
            .gap_3()
            .child(
                div()
                    .id("note-editor-title")
                    .h(px(34.0))
                    .flex()
                    .items_center()
                    .px_3()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.panel))
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .track_focus(&self.notes.title_focus)
                    .key_context("note-title")
                    .on_key_down(context.listener(
                        |view, event: &KeyDownEvent, _window, context| {
                            view.handle_note_title_key_down(event, context);
                        },
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, event: &MouseDownEvent, window, context| {
                            view.start_note_title_mouse_selection(event, context);
                            window.focus(&view.notes.title_focus);
                            context.stop_propagation();
                        }),
                    )
                    .child(NoteTitleElement {
                        view: context.entity(),
                        focus_handle: self.notes.title_focus.clone(),
                        palette,
                        placeholder: "未命名笔记",
                    }),
            )
            .child(
                div()
                    .id("note-editor-content")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.input))
                    .track_focus(&self.notes.editor_focus)
                    .key_context("note-editor")
                    .on_key_down(context.listener(
                        |view, event: &KeyDownEvent, _window, context| {
                            view.handle_note_editor_key_down(event, context);
                        },
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, event: &MouseDownEvent, window, context| {
                            view.start_note_editor_mouse_selection(event, context);
                            window.focus(&view.notes.editor_focus);
                            context.stop_propagation();
                        }),
                    )
                    .on_mouse_move(context.listener(
                        |view, event: &MouseMoveEvent, _window, context| {
                            view.update_note_editor_mouse_selection(event.position, context);
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.finish_note_editor_mouse_selection(context);
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.finish_note_editor_mouse_selection(context);
                        }),
                    )
                    .child(
                        div()
                            .id("note-editor-scroll")
                            .size_full()
                            .px_3()
                            .py_3()
                            .overflow_scroll()
                            .scrollbar_width(px(6.0))
                            .font_family(LOG_VIEWER_FONT_FAMILY)
                            .text_size(px(NOTES_TEXT_FONT_SIZE))
                            .line_height(px(NOTES_TEXT_LINE_HEIGHT))
                            .text_color(rgb(palette.text))
                            .child(NoteEditorElement {
                                view: context.entity(),
                                focus_handle: self.notes.editor_focus.clone(),
                                palette,
                            }),
                    ),
            )
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
                                    move |view, _event: &ClickEvent, _window, context| {
                                        view.resolve_notes_unsaved_dialog(choice, context);
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
