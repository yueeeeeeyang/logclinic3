// 笔记 MainView 状态方法。
//
// 业务意图：
// - 本文件集中维护笔记树创建/选择/删除、阅读器选区、编辑器快捷键、保存和确认弹窗状态流转。
// - 方法仍实现到 MainView 上，保持和其它 app UI 动作模块一致的调用方式，不引入新的公开状态对象。

use super::*;

impl MainView {
    /// 返回笔记数据库路径，失败时同步记录错误。
    pub(in crate::app) fn notes_database_path_or_error(&mut self) -> Option<PathBuf> {
        let path = notes_database_path();
        if path.is_none() {
            self.notes.database_error =
                Some("当前平台没有可用的应用配置目录，无法保存笔记".to_string());
        }
        path
    }

    /// 重新加载笔记树并保持可见状态。
    pub(in crate::app) fn reload_notes_tree(&mut self) {
        let Some(path) = notes_database_path() else {
            self.notes.database_error =
                Some("当前平台没有可用的应用配置目录，无法保存笔记".to_string());
            return;
        };
        match load_note_tree(&path) {
            Ok(rows) => {
                self.notes.tree_rows = rows;
                self.notes.database_error = None;
                self.notes.rebuild_visible_rows();
            }
            Err(error) => self.notes.database_error = Some(error),
        }
    }

    /// 选中笔记树节点。
    pub(in crate::app) fn select_note_tree_row(
        &mut self,
        selection: NotesTreeSelection,
        context: &mut Context<Self>,
    ) {
        if self.notes.has_unsaved_changes() {
            self.notes.unsaved_dialog = Some(NotesUnsavedDialog {
                action: NotesPendingAction::Select(selection),
            });
            context.notify();
            return;
        }
        self.apply_note_tree_selection(selection, context);
    }

    /// 处理笔记树左键按下。
    ///
    /// 业务意图：
    /// - 笔记树和日志目录树一样使用整行鼠标命中；左键负责选中并打开笔记或展开目录。
    /// - 右键菜单打开时，新的左键应先关闭菜单，再执行本次真实点击，避免菜单遮挡状态残留。
    pub(in crate::app) fn handle_note_tree_left_mouse_down(
        &mut self,
        selection: NotesTreeSelection,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        self.notes.tree_context_menu = None;
        if event.click_count == 1 {
            self.select_note_tree_row(selection, context);
        } else {
            context.notify();
        }
        context.stop_propagation();
    }

    /// 应用笔记树选择，不再触发未保存确认。
    pub(in crate::app) fn apply_note_tree_selection(
        &mut self,
        selection: NotesTreeSelection,
        context: &mut Context<Self>,
    ) {
        self.notes.selected = Some(selection.clone());
        self.notes.source_selection = None;
        self.notes.source_selection_drag_anchor = None;
        self.notes.unsaved_dialog = None;
        self.notes.rename_dialog = None;
        self.notes.tree_context_menu = None;
        self.notes.delete_confirm_dialog = None;

        match selection.kind {
            NoteTreeRowKind::Directory => {
                self.toggle_note_directory(&selection.id);
                self.notes.active_note = None;
                self.notes.is_editing = false;
                self.notes.editor_text.clear();
                self.notes.editor_title.clear();
                self.notes.title_selection_range = 0..0;
                self.notes.title_marked_range = None;
                self.notes.editor_selection_range = 0..0;
            }
            NoteTreeRowKind::Note => {
                self.load_note_into_workspace(&selection.id);
            }
        }
        context.notify();
    }

    /// 展开或收起目录。
    pub(in crate::app) fn toggle_note_directory(&mut self, directory_id: &str) {
        let Some(row) = self
            .notes
            .tree_rows
            .iter()
            .find(|row| row.id == directory_id && row.kind == NoteTreeRowKind::Directory)
        else {
            return;
        };
        if !row.has_children {
            return;
        }
        if !self.notes.expanded_directory_ids.remove(directory_id) {
            self.notes
                .expanded_directory_ids
                .insert(directory_id.to_string());
        }
        self.notes.rebuild_visible_rows();
    }

    /// 加载笔记正文到右侧工作区。
    pub(in crate::app) fn load_note_into_workspace(&mut self, note_id: &str) {
        let Some(path) = self.notes_database_path_or_error() else {
            return;
        };
        match load_note(&path, note_id) {
            Ok(Some(note)) => {
                self.notes.reader_mode = if note.content_format == NoteContentFormat::Markdown {
                    NoteReaderMode::Preview
                } else {
                    NoteReaderMode::Source
                };
                self.notes.editor_title = note.title.clone();
                let title_cursor = self.notes.editor_title.len();
                self.notes.title_selection_range = title_cursor..title_cursor;
                self.notes.title_marked_range = None;
                self.notes.editor_text = note.content.clone();
                self.notes.editor_format = note.content_format;
                self.notes.editor_selection_range = 0..0;
                self.notes.editor_marked_range = None;
                self.notes.editor_undo_stack.clear();
                self.notes.editor_redo_stack.clear();
                self.notes.active_note = Some(note);
                self.notes.is_editing = false;
                self.notes.database_error = None;
            }
            Ok(None) => {
                self.notes.active_note = None;
                self.notes.database_error = Some("选择的笔记不存在".to_string());
            }
            Err(error) => self.notes.database_error = Some(error),
        }
    }

    /// 返回当前选择对应的新建笔记父目录。
    ///
    /// 业务意图：
    /// - 顶部“新建笔记”在选中目录时放入该目录，选中笔记时放入同级目录，符合多数笔记树中新建同级笔记的操作习惯。
    pub(in crate::app) fn selected_note_parent_directory_id(&self) -> Option<String> {
        let selected = self.notes.selected.as_ref()?;
        match selected.kind {
            NoteTreeRowKind::Directory => Some(selected.id.clone()),
            NoteTreeRowKind::Note => self
                .notes
                .tree_rows
                .iter()
                .find(|row| row.id == selected.id && row.kind == NoteTreeRowKind::Note)
                .and_then(|row| row.parent_id.clone()),
        }
    }

    /// 返回顶部“新建目录”应使用的父目录。
    ///
    /// 业务意图：
    /// - 用户要求顶部只保留一个新增目录按钮：没有选中目录时创建根目录，选中目录时创建子目录。
    /// - 选中笔记不视为选中目录，避免误把目录建到笔记所在目录下造成语义不清。
    pub(in crate::app) fn selected_directory_id_for_new_directory(&self) -> Option<String> {
        self.notes
            .selected
            .as_ref()
            .and_then(|selection| match selection.kind {
                NoteTreeRowKind::Directory => Some(selection.id.clone()),
                NoteTreeRowKind::Note => None,
            })
    }

    /// 顶部按钮创建目录。
    pub(in crate::app) fn create_note_directory_from_toolbar(
        &mut self,
        context: &mut Context<Self>,
    ) {
        let parent_id = self.selected_directory_id_for_new_directory();
        self.request_create_note_directory(parent_id, context);
    }

    /// 请求创建目录，必要时先处理未保存编辑草稿。
    pub(in crate::app) fn request_create_note_directory(
        &mut self,
        parent_id: Option<String>,
        context: &mut Context<Self>,
    ) {
        if self.notes.has_unsaved_changes() {
            self.notes.unsaved_dialog = Some(NotesUnsavedDialog {
                action: NotesPendingAction::CreateDirectory(parent_id),
            });
            context.notify();
            return;
        }
        self.create_note_directory_under(parent_id, context);
    }

    /// 创建目录并刷新树。
    pub(in crate::app) fn create_note_directory_under(
        &mut self,
        parent_id: Option<String>,
        context: &mut Context<Self>,
    ) {
        let Some(path) = self.notes_database_path_or_error() else {
            context.notify();
            return;
        };
        match create_note_directory(&path, parent_id.clone(), "新建目录".to_string()) {
            Ok(directory) => {
                if let Some(parent_id) = parent_id {
                    self.notes.expanded_directory_ids.insert(parent_id);
                }
                self.reload_notes_tree();
                self.notes.selected = Some(NotesTreeSelection {
                    id: directory.id,
                    kind: NoteTreeRowKind::Directory,
                });
                self.notes.active_note = None;
                self.notes.is_editing = false;
                self.notes.editor_text.clear();
                self.notes.editor_title.clear();
                self.notes.title_selection_range = 0..0;
                self.notes.editor_selection_range = 0..0;
            }
            Err(error) => self.notes.database_error = Some(error),
        }
        context.notify();
    }

    /// 创建笔记。
    pub(in crate::app) fn create_note_in_selected_directory(
        &mut self,
        context: &mut Context<Self>,
    ) {
        let directory_id = self.selected_note_parent_directory_id();
        self.request_create_note(directory_id, context);
    }

    /// 请求创建笔记，必要时先处理未保存编辑草稿。
    pub(in crate::app) fn request_create_note(
        &mut self,
        directory_id: Option<String>,
        context: &mut Context<Self>,
    ) {
        if self.notes.has_unsaved_changes() {
            self.notes.unsaved_dialog = Some(NotesUnsavedDialog {
                action: NotesPendingAction::CreateNote(directory_id),
            });
            context.notify();
            return;
        }
        self.create_note_under(directory_id, context);
    }

    /// 创建笔记并自动进入编辑状态。
    pub(in crate::app) fn create_note_under(
        &mut self,
        directory_id: Option<String>,
        context: &mut Context<Self>,
    ) {
        let Some(path) = self.notes_database_path_or_error() else {
            context.notify();
            return;
        };
        match create_note(&path, directory_id.clone(), "新建笔记".to_string()) {
            Ok(note) => {
                if let Some(directory_id) = directory_id {
                    self.notes.expanded_directory_ids.insert(directory_id);
                }
                self.reload_notes_tree();
                self.notes.selected = Some(NotesTreeSelection {
                    id: note.id.clone(),
                    kind: NoteTreeRowKind::Note,
                });
                self.notes.active_note = Some(note.clone());
                self.notes.editor_title = note.title;
                let title_cursor = self.notes.editor_title.len();
                self.notes.title_selection_range = title_cursor..title_cursor;
                self.notes.title_marked_range = None;
                self.notes.editor_text = note.content;
                self.notes.editor_format = note.content_format;
                self.notes.editor_selection_range = 0..0;
                self.notes.editor_undo_stack.clear();
                self.notes.editor_redo_stack.clear();
                self.notes.is_editing = true;
                self.notes.reader_mode = NoteReaderMode::Preview;
            }
            Err(error) => self.notes.database_error = Some(error),
        }
        context.notify();
    }

    /// 请求重命名指定节点。
    pub(in crate::app) fn request_rename_note_tree_item(
        &mut self,
        target: NotesTreeSelection,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.notes.has_unsaved_changes() {
            self.notes.unsaved_dialog = Some(NotesUnsavedDialog {
                action: NotesPendingAction::Rename(target),
            });
            context.notify();
            return;
        }
        self.open_note_rename_dialog(target, Some(window), context);
    }

    /// 读取笔记树节点标题。
    fn note_tree_title_for_selection(&self, target: &NotesTreeSelection) -> String {
        self.notes
            .tree_rows
            .iter()
            .find(|row| row.id == target.id && row.kind == target.kind)
            .map(|row| row.title.clone())
            .unwrap_or_else(|| match target.kind {
                NoteTreeRowKind::Directory => "未命名目录".to_string(),
                NoteTreeRowKind::Note => "未命名笔记".to_string(),
            })
    }

    /// 打开重命名弹窗。
    ///
    /// 业务意图：
    /// - 目录和笔记都在树上重命名，弹窗复用标题输入框的 IME、复制粘贴和 UTF-8 边界处理，避免为目录另写一套不完整输入逻辑。
    pub(in crate::app) fn open_note_rename_dialog(
        &mut self,
        target: NotesTreeSelection,
        window: Option<&mut Window>,
        context: &mut Context<Self>,
    ) {
        let original_title = self.note_tree_title_for_selection(&target);
        self.notes.editor_title = original_title.clone();
        self.notes.title_selection_range = 0..self.notes.editor_title.len();
        self.notes.title_marked_range = None;
        self.notes.title_last_layout = None;
        self.notes.title_last_bounds = None;
        self.notes.rename_dialog = Some(NotesRenameDialog {
            target,
            original_title,
        });
        self.notes.tree_context_menu = None;
        if let Some(window) = window {
            window.focus(&self.notes.title_focus);
        }
        context.notify();
    }

    /// 取消重命名弹窗。
    pub(in crate::app) fn cancel_note_rename(&mut self, context: &mut Context<Self>) {
        self.notes.rename_dialog = None;
        if let Some(note) = &self.notes.active_note {
            self.notes.editor_title = note.title.clone();
            let cursor = self.notes.editor_title.len();
            self.notes.title_selection_range = cursor..cursor;
        } else {
            self.notes.editor_title.clear();
            self.notes.title_selection_range = 0..0;
        }
        self.notes.title_marked_range = None;
        context.notify();
    }

    /// 确认重命名弹窗并写入 SQLite。
    pub(in crate::app) fn confirm_note_rename(&mut self, context: &mut Context<Self>) -> bool {
        let Some(dialog) = self.notes.rename_dialog.take() else {
            return false;
        };
        let Some(path) = self.notes_database_path_or_error() else {
            self.notes.rename_dialog = Some(dialog);
            context.notify();
            return false;
        };
        let trimmed_title = self.notes.editor_title.trim();
        let next_title = if trimmed_title.is_empty() {
            match dialog.target.kind {
                NoteTreeRowKind::Directory => "未命名目录",
                NoteTreeRowKind::Note => "未命名笔记",
            }
        } else {
            trimmed_title
        };
        if next_title == dialog.original_title {
            self.notes.database_error = None;
            self.cancel_note_rename(context);
            return true;
        }
        let result = match dialog.target.kind {
            NoteTreeRowKind::Directory => {
                rename_note_directory(&path, &dialog.target.id, next_title)
            }
            NoteTreeRowKind::Note => rename_note(&path, &dialog.target.id, next_title),
        };
        match result {
            Ok(()) => {
                if let Some(note) = self.notes.active_note.as_mut()
                    && note.id == dialog.target.id
                {
                    note.title = next_title.to_string();
                }
                self.notes.editor_title = self
                    .notes
                    .active_note
                    .as_ref()
                    .map(|note| note.title.clone())
                    .unwrap_or_default();
                let cursor = self.notes.editor_title.len();
                self.notes.title_selection_range = cursor..cursor;
                self.notes.title_marked_range = None;
                self.notes.database_error = None;
                self.reload_notes_tree();
                context.notify();
                true
            }
            Err(error) => {
                self.notes.database_error = Some(error);
                self.notes.rename_dialog = Some(dialog);
                context.notify();
                false
            }
        }
    }

    /// 请求删除指定节点。
    pub(in crate::app) fn request_delete_note_tree_item(
        &mut self,
        target: NotesTreeSelection,
        context: &mut Context<Self>,
    ) {
        if self.notes.has_unsaved_changes() {
            self.notes.unsaved_dialog = Some(NotesUnsavedDialog {
                action: NotesPendingAction::Delete(target),
            });
            context.notify();
            return;
        }
        self.open_note_delete_confirm(target, context);
    }

    /// 打开删除确认弹窗。
    pub(in crate::app) fn open_note_delete_confirm(
        &mut self,
        target: NotesTreeSelection,
        context: &mut Context<Self>,
    ) {
        let title = self
            .notes
            .tree_rows
            .iter()
            .find(|row| row.id == target.id && row.kind == target.kind)
            .map(|row| row.title.clone())
            .unwrap_or_else(|| "选中项".to_string());
        self.notes.delete_confirm_dialog = Some(NotesDeleteConfirmDialog { target, title });
        self.notes.tree_context_menu = None;
        context.notify();
    }

    /// 确认删除当前弹窗目标。
    pub(in crate::app) fn confirm_note_delete(&mut self, context: &mut Context<Self>) {
        let Some(dialog) = self.notes.delete_confirm_dialog.take() else {
            return;
        };
        let Some(path) = self.notes_database_path_or_error() else {
            context.notify();
            return;
        };
        let should_clear_active_note = self.note_delete_target_contains_active_note(&dialog.target);
        let result = match dialog.target.kind {
            NoteTreeRowKind::Directory => delete_note_directory(&path, &dialog.target.id),
            NoteTreeRowKind::Note => delete_note(&path, &dialog.target.id),
        };
        match result {
            Ok(()) => {
                if should_clear_active_note {
                    self.notes.active_note = None;
                    self.notes.is_editing = false;
                    self.notes.editor_text.clear();
                    self.notes.editor_title.clear();
                    self.notes.editor_selection_range = 0..0;
                    self.notes.title_selection_range = 0..0;
                }
                self.notes.selected = None;
                self.notes.tree_context_menu = None;
                self.reload_notes_tree();
            }
            Err(error) => self.notes.database_error = Some(error),
        }
        context.notify();
    }

    /// 判断删除目标是否覆盖当前右侧打开的笔记。
    ///
    /// 业务意图：
    /// - 删除目录会由 SQLite 外键级联删除子目录和子笔记，UI 也必须同步清空右侧旧正文，避免用户继续查看或编辑已经不存在的笔记。
    fn note_delete_target_contains_active_note(&self, target: &NotesTreeSelection) -> bool {
        let Some(active_note) = self.notes.active_note.as_ref() else {
            return false;
        };
        match target.kind {
            NoteTreeRowKind::Note => active_note.id == target.id,
            NoteTreeRowKind::Directory => {
                let Some((start_index, target_depth)) = self
                    .notes
                    .tree_rows
                    .iter()
                    .enumerate()
                    .find(|(_, row)| row.id == target.id && row.kind == NoteTreeRowKind::Directory)
                    .map(|(index, row)| (index, row.depth))
                else {
                    return false;
                };
                self.notes
                    .tree_rows
                    .iter()
                    .skip(start_index + 1)
                    .take_while(|row| row.depth > target_depth)
                    .any(|row| row.kind == NoteTreeRowKind::Note && row.id == active_note.id)
            }
        }
    }

    /// 取消删除确认。
    pub(in crate::app) fn cancel_note_delete(&mut self, context: &mut Context<Self>) {
        self.notes.delete_confirm_dialog = None;
        context.notify();
    }

    /// 进入笔记编辑状态。
    pub(in crate::app) fn start_note_editing(
        &mut self,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(note) = &self.notes.active_note else {
            return;
        };
        self.notes.editor_title = note.title.clone();
        let title_cursor = self.notes.editor_title.len();
        self.notes.title_selection_range = title_cursor..title_cursor;
        self.notes.title_marked_range = None;
        self.notes.editor_text = note.content.clone();
        self.notes.editor_format = note.content_format;
        let cursor = self.notes.editor_text.len();
        self.notes.editor_selection_range = cursor..cursor;
        self.notes.editor_marked_range = None;
        self.notes.editor_undo_stack.clear();
        self.notes.editor_redo_stack.clear();
        self.notes.is_editing = true;
        window.focus(&self.notes.editor_focus);
        context.notify();
    }

    /// 取消笔记编辑。
    pub(in crate::app) fn cancel_note_editing(&mut self, context: &mut Context<Self>) {
        if let Some(note) = &self.notes.active_note {
            self.notes.editor_title = note.title.clone();
            let title_cursor = self.notes.editor_title.len();
            self.notes.title_selection_range = title_cursor..title_cursor;
            self.notes.title_marked_range = None;
            self.notes.editor_text = note.content.clone();
            self.notes.editor_format = note.content_format;
        }
        self.notes.editor_marked_range = None;
        self.notes.editor_selection_range = 0..0;
        self.notes.editor_undo_stack.clear();
        self.notes.editor_redo_stack.clear();
        self.notes.is_editing = false;
        context.notify();
    }

    /// 保存当前笔记草稿。
    pub(in crate::app) fn save_active_note(&mut self, context: &mut Context<Self>) -> bool {
        let Some(note) = self.notes.active_note.clone() else {
            return false;
        };
        let Some(path) = self.notes_database_path_or_error() else {
            context.notify();
            return false;
        };
        let trimmed_title = self.notes.editor_title.trim();
        let title = if trimmed_title.is_empty() {
            "未命名笔记"
        } else {
            trimmed_title
        };
        let result = update_note(
            &path,
            &note.id,
            title,
            &self.notes.editor_text,
            self.notes.editor_format,
        );
        match result {
            Ok(updated_at_ms) => {
                let mut updated = note;
                updated.title = title.to_string();
                updated.content = self.notes.editor_text.clone();
                updated.content_format = self.notes.editor_format;
                updated.updated_at_ms = updated_at_ms;
                self.notes.active_note = Some(updated);
                self.notes.is_editing = false;
                self.notes.editor_marked_range = None;
                self.notes.editor_undo_stack.clear();
                self.notes.editor_redo_stack.clear();
                self.notes.database_error = None;
                self.reload_notes_tree();
                context.notify();
                true
            }
            Err(error) => {
                self.notes.database_error = Some(error);
                context.notify();
                false
            }
        }
    }

    /// 切换编辑草稿格式。
    pub(in crate::app) fn toggle_note_editor_format(&mut self, context: &mut Context<Self>) {
        self.notes.editor_format = match self.notes.editor_format {
            NoteContentFormat::PlainText => NoteContentFormat::Markdown,
            NoteContentFormat::Markdown => NoteContentFormat::PlainText,
        };
        context.notify();
    }

    /// 切换 Markdown 阅读模式。
    pub(in crate::app) fn toggle_note_reader_mode(&mut self, context: &mut Context<Self>) {
        self.notes.reader_mode = match self.notes.reader_mode {
            NoteReaderMode::Preview => NoteReaderMode::Source,
            NoteReaderMode::Source => NoteReaderMode::Preview,
        };
        self.notes.source_selection = None;
        self.notes.source_selection_drag_anchor = None;
        context.notify();
    }

    /// 返回笔记 Markdown 预览文档。
    ///
    /// 业务意图：
    /// - 笔记阅读器会在滚动、窗口重绘和主题变化时反复渲染；Markdown 解析和代码高亮必须缓存，避免大笔记卡住 UI。
    /// - 缓存只按当前笔记 ID、正文哈希和主题命中，保存后的正文变化会自然失效，源码模式不依赖该缓存。
    pub(in crate::app) fn note_markdown_preview_document(
        &self,
        note: &Note,
        theme: EffectiveTheme,
    ) -> AppMarkdownDocument {
        let content_hash = ai_chat_markdown_content_hash(&note.content);
        {
            let cache = self.notes.markdown_preview_cache.borrow();
            if let Some(entry) = cache.as_ref()
                && entry.note_id == note.id
                && entry.content_hash == content_hash
                && entry.theme == theme
            {
                return entry.document.clone();
            }
        }

        let document = parse_ai_chat_markdown(&note.content, theme);
        self.notes
            .markdown_preview_cache
            .replace(Some(NoteMarkdownPreviewCacheEntry {
                note_id: note.id.clone(),
                content_hash,
                theme,
                document: document.clone(),
            }));
        document
    }

    /// 处理未保存确认选择。
    pub(in crate::app) fn resolve_notes_unsaved_dialog(
        &mut self,
        choice: NotesUnsavedChoice,
        context: &mut Context<Self>,
    ) {
        let Some(dialog) = self.notes.unsaved_dialog.take() else {
            return;
        };
        match choice {
            NotesUnsavedChoice::Cancel => {
                context.notify();
            }
            NotesUnsavedChoice::Save => {
                if self.save_active_note(context) {
                    self.continue_notes_pending_action(dialog.action, context);
                }
            }
            NotesUnsavedChoice::Discard => {
                self.cancel_note_editing(context);
                self.continue_notes_pending_action(dialog.action, context);
            }
        }
    }

    /// 继续执行未保存确认后的动作。
    pub(in crate::app) fn continue_notes_pending_action(
        &mut self,
        action: NotesPendingAction,
        context: &mut Context<Self>,
    ) {
        match action {
            NotesPendingAction::Select(selection) => {
                self.apply_note_tree_selection(selection, context)
            }
            NotesPendingAction::SwitchFeature(feature) => {
                self.navigation.active_main_feature = feature;
                context.notify();
            }
            NotesPendingAction::CreateDirectory(parent_id) => {
                self.create_note_directory_under(parent_id, context)
            }
            NotesPendingAction::CreateNote(directory_id) => {
                self.create_note_under(directory_id, context)
            }
            NotesPendingAction::Delete(selection) => {
                self.open_note_delete_confirm(selection, context)
            }
            NotesPendingAction::Rename(selection) => {
                self.open_note_rename_dialog(selection, None, context)
            }
        }
    }

    /// 计算笔记树右键菜单横坐标。
    ///
    /// 业务意图：
    /// - 笔记页和日志页一样位于固定主导航右侧，窗口坐标进入笔记树面板前必须扣除主导航宽度。
    pub(in crate::app) fn notes_tree_context_menu_x(window_x: f32) -> f32 {
        (window_x - MAIN_NAV_WIDTH)
            .max(0.0)
            .clamp(0.0, NOTES_TREE_WIDTH - LOG_TREE_CONTEXT_MENU_WIDTH)
    }

    /// 打开笔记树右键菜单。
    pub(in crate::app) fn open_notes_tree_context_menu(
        &mut self,
        target: NotesTreeSelection,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        self.notes.selected = Some(target.clone());
        self.notes.tree_context_menu = Some(NotesTreeContextMenu {
            target,
            x: Self::notes_tree_context_menu_x(f32::from(event.position.x)),
            y: f32::from(event.position.y).max(0.0),
        });
        self.notes.rename_dialog = None;
        self.notes.delete_confirm_dialog = None;
        context.notify();
        context.stop_propagation();
    }

    /// 关闭笔记树右键菜单。
    pub(in crate::app) fn close_notes_tree_context_menu(&mut self, context: &mut Context<Self>) {
        self.notes.tree_context_menu = None;
        context.notify();
    }

    /// 执行笔记树右键菜单命令。
    pub(in crate::app) fn handle_notes_tree_context_menu_action(
        &mut self,
        action: NotesTreeContextMenuAction,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(menu) = self.notes.tree_context_menu.take() else {
            return;
        };
        let target = menu.target;
        match action {
            NotesTreeContextMenuAction::NewDirectory => {
                let parent_id = (target.kind == NoteTreeRowKind::Directory).then_some(target.id);
                self.request_create_note_directory(parent_id, context);
            }
            NotesTreeContextMenuAction::NewNote => {
                let directory_id = (target.kind == NoteTreeRowKind::Directory).then_some(target.id);
                self.request_create_note(directory_id, context);
            }
            NotesTreeContextMenuAction::Rename => {
                self.request_rename_note_tree_item(target, window, context);
            }
            NotesTreeContextMenuAction::Delete => {
                self.request_delete_note_tree_item(target, context);
            }
        }
        context.stop_propagation();
    }

    /// 读取笔记编辑器当前文本、选择范围和组合文本范围快照。
    pub(in crate::app) fn note_editor_text_snapshot(
        &self,
    ) -> (String, Range<usize>, Option<Range<usize>>) {
        (
            self.notes.editor_text.clone(),
            Self::clamp_search_text_range(
                &self.notes.editor_text,
                self.notes.editor_selection_range.clone(),
            ),
            self.notes.editor_marked_range.clone(),
        )
    }

    /// 根据窗口坐标返回笔记标题 UTF-8 字节下标。
    pub(in crate::app) fn note_title_index_for_point(&self, position: Point<Pixels>) -> usize {
        let Some(layout) = self.notes.title_last_layout.as_ref() else {
            return self.notes.editor_title.len();
        };
        let Some(bounds) = self.notes.title_last_bounds.as_ref() else {
            return self.notes.editor_title.len();
        };
        layout
            .closest_index_for_x(position.x - bounds.left())
            .min(self.notes.editor_title.len())
    }

    /// 开始标题鼠标选择。
    pub(in crate::app) fn start_note_title_mouse_selection(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let index = self.note_title_index_for_point(event.position);
        self.notes.title_marked_range = None;
        match event.click_count {
            0 | 1 => self.notes.title_selection_range = index..index,
            2 => {
                self.notes.title_selection_range =
                    Self::search_text_word_range_for_index(&self.notes.editor_title, index);
            }
            _ => self.notes.title_selection_range = 0..self.notes.editor_title.len(),
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 返回标题当前选中文本。
    pub(in crate::app) fn selected_note_title_text(&self) -> Option<String> {
        let range = Self::clamp_search_text_range(
            &self.notes.editor_title,
            self.notes.title_selection_range.clone(),
        );
        (range.start < range.end).then(|| self.notes.editor_title[range].to_string())
    }

    /// 替换标题当前选区。
    pub(in crate::app) fn replace_note_title_selection(&mut self, replacement: &str) {
        let replacement = Self::sanitize_search_input_text(replacement);
        let range = self.notes.title_marked_range.take().unwrap_or_else(|| {
            Self::clamp_search_text_range(
                &self.notes.editor_title,
                self.notes.title_selection_range.clone(),
            )
        });
        self.notes
            .editor_title
            .replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        self.notes.title_selection_range = cursor..cursor;
    }

    /// 处理标题输入框按键。
    pub(in crate::app) fn handle_note_title_key_down(
        &mut self,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_paste_keystroke(&event.keystroke) {
            if let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) {
                self.replace_note_title_selection(&text);
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }
        if Self::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_note_title_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return;
        }
        if Self::is_cut_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_note_title_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                self.replace_note_title_selection("");
                context.notify();
            }
            context.stop_propagation();
            return;
        }
        if Self::is_select_all_keystroke(&event.keystroke) {
            self.notes.title_selection_range = 0..self.notes.editor_title.len();
            self.notes.title_marked_range = None;
            context.stop_propagation();
            context.notify();
            return;
        }
        if Self::is_save_keystroke(&event.keystroke) || event.keystroke.key == "enter" {
            if self.notes.rename_dialog.is_some() {
                self.confirm_note_rename(context);
            } else {
                self.save_active_note(context);
            }
            context.stop_propagation();
            return;
        }
        match event.keystroke.key.as_str() {
            "left" => {
                self.notes.title_marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.notes.title_selection_range.end = Self::previous_search_text_boundary(
                        &self.notes.editor_title,
                        self.notes.title_selection_range.end,
                    );
                } else {
                    let cursor = Self::previous_search_text_boundary(
                        &self.notes.editor_title,
                        self.notes.title_selection_range.end,
                    );
                    self.notes.title_selection_range = cursor..cursor;
                }
            }
            "right" => {
                self.notes.title_marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.notes.title_selection_range.end = Self::next_search_text_boundary(
                        &self.notes.editor_title,
                        self.notes.title_selection_range.end,
                    );
                } else {
                    let cursor = Self::next_search_text_boundary(
                        &self.notes.editor_title,
                        self.notes.title_selection_range.end,
                    );
                    self.notes.title_selection_range = cursor..cursor;
                }
            }
            "backspace" => {
                if self.notes.title_selection_range.start != self.notes.title_selection_range.end
                    || self.notes.title_marked_range.is_some()
                {
                    self.replace_note_title_selection("");
                } else if let Some((previous_index, _)) = self.notes.editor_title
                    [..self.notes.title_selection_range.end]
                    .char_indices()
                    .next_back()
                {
                    let cursor = self.notes.title_selection_range.end;
                    self.notes
                        .editor_title
                        .replace_range(previous_index..cursor, "");
                    self.notes.title_selection_range = previous_index..previous_index;
                }
            }
            "delete" => {
                if self.notes.title_selection_range.start != self.notes.title_selection_range.end
                    || self.notes.title_marked_range.is_some()
                {
                    self.replace_note_title_selection("");
                } else if let Some((next_index, next_character)) = self.notes.editor_title
                    [self.notes.title_selection_range.end..]
                    .char_indices()
                    .next()
                {
                    let start = self.notes.title_selection_range.end + next_index;
                    let end = start + next_character.len_utf8();
                    self.notes.editor_title.replace_range(start..end, "");
                    self.notes.title_selection_range = start..start;
                }
            }
            "escape" => {
                if self.notes.rename_dialog.is_some() {
                    self.cancel_note_rename(context);
                }
            }
            _ => return,
        }
        self.touch_search_text_cursor_activity();
        context.stop_propagation();
        context.notify();
    }

    /// 保存笔记编辑器最近一次排版结果。
    pub(in crate::app) fn store_note_editor_text_layouts(
        &mut self,
        layouts: Vec<NoteEditorLineLayout>,
        bounds: Bounds<Pixels>,
    ) {
        self.notes.editor_last_layouts = layouts;
        self.notes.editor_last_bounds = Some(bounds);
    }

    /// 返回笔记编辑器当前内容需要的可视行数。
    pub(in crate::app) fn note_editor_visual_line_count(&self) -> usize {
        Self::thread_analysis_filter_line_ranges(&self.notes.editor_text)
            .len()
            .max(1)
    }

    /// 根据窗口坐标返回笔记编辑器 UTF-8 字节下标。
    pub(in crate::app) fn note_editor_index_for_point(&self, position: Point<Pixels>) -> usize {
        if self.notes.editor_text.is_empty() {
            return 0;
        }
        for layout in &self.notes.editor_last_layouts {
            if position.y >= layout.bounds.top() && position.y <= layout.bounds.bottom() {
                return layout
                    .byte_range
                    .start
                    .saturating_add(
                        layout
                            .line
                            .closest_index_for_x(position.x - layout.bounds.left()),
                    )
                    .min(layout.byte_range.end);
            }
        }
        if let Some(bounds) = &self.notes.editor_last_bounds
            && position.y < bounds.top()
        {
            return 0;
        }
        self.notes.editor_text.len()
    }

    /// 记录撤销快照。
    pub(in crate::app) fn record_note_editor_undo_snapshot(&mut self) {
        if self
            .notes
            .editor_undo_stack
            .last()
            .is_some_and(|text| text == &self.notes.editor_text)
        {
            return;
        }
        self.notes
            .editor_undo_stack
            .push(self.notes.editor_text.clone());
        if self.notes.editor_undo_stack.len() > NOTE_EDITOR_HISTORY_LIMIT {
            self.notes.editor_undo_stack.remove(0);
        }
        self.notes.editor_redo_stack.clear();
    }

    /// 替换笔记编辑器当前选区。
    pub(in crate::app) fn replace_note_editor_selection(&mut self, replacement: &str) {
        self.record_note_editor_undo_snapshot();
        let replacement = replacement.replace("\r\n", "\n").replace('\r', "\n");
        let range = self.notes.editor_marked_range.take().unwrap_or_else(|| {
            Self::clamp_search_text_range(
                &self.notes.editor_text,
                self.notes.editor_selection_range.clone(),
            )
        });
        self.notes
            .editor_text
            .replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        self.notes.editor_selection_range = cursor..cursor;
    }

    /// 返回笔记编辑器当前选中文本。
    pub(in crate::app) fn selected_note_editor_text(&self) -> Option<String> {
        let range = Self::clamp_search_text_range(
            &self.notes.editor_text,
            self.notes.editor_selection_range.clone(),
        );
        (range.start < range.end).then(|| self.notes.editor_text[range].to_string())
    }

    /// 开始笔记编辑器鼠标选择。
    pub(in crate::app) fn start_note_editor_mouse_selection(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let index = self.note_editor_index_for_point(event.position);
        self.notes.editor_marked_range = None;
        match event.click_count {
            0 | 1 => {
                if event.modifiers.shift {
                    self.notes.editor_selection_range.end = index;
                    self.notes.editor_selection_range = Self::clamp_search_text_range(
                        &self.notes.editor_text,
                        self.notes.editor_selection_range.clone(),
                    );
                } else {
                    self.notes.editor_selection_range = index..index;
                }
                self.notes.editor_selection_drag = Some(self.notes.editor_selection_range.start);
            }
            2 => {
                self.notes.editor_selection_range =
                    Self::search_text_word_range_for_index(&self.notes.editor_text, index);
                self.notes.editor_selection_drag = None;
            }
            _ => {
                self.notes.editor_selection_range = 0..self.notes.editor_text.len();
                self.notes.editor_selection_drag = None;
            }
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 鼠标拖拽时更新笔记编辑器选区终点。
    pub(in crate::app) fn update_note_editor_mouse_selection(
        &mut self,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) {
        let Some(anchor) = self.notes.editor_selection_drag else {
            return;
        };
        let index = self.note_editor_index_for_point(position);
        self.notes.editor_marked_range = None;
        self.notes.editor_selection_range =
            Self::clamp_search_text_range(&self.notes.editor_text, anchor..index);
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 结束笔记编辑器鼠标拖拽选择。
    pub(in crate::app) fn finish_note_editor_mouse_selection(
        &mut self,
        context: &mut Context<Self>,
    ) {
        if self.notes.editor_selection_drag.take().is_some() {
            context.notify();
        }
    }

    /// 处理笔记编辑器按键。
    pub(in crate::app) fn handle_note_editor_key_down(
        &mut self,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_paste_keystroke(&event.keystroke) {
            if let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) {
                self.replace_note_editor_selection(&text);
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }
        if Self::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_note_editor_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return;
        }
        if Self::is_cut_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_note_editor_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                self.replace_note_editor_selection("");
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }
        if Self::is_select_all_keystroke(&event.keystroke) {
            self.notes.editor_marked_range = None;
            self.notes.editor_selection_range = 0..self.notes.editor_text.len();
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return;
        }
        if Self::is_save_keystroke(&event.keystroke) {
            self.save_active_note(context);
            context.stop_propagation();
            return;
        }
        if Self::is_undo_keystroke(&event.keystroke) {
            self.undo_note_editor(context);
            context.stop_propagation();
            return;
        }
        if Self::is_redo_keystroke(&event.keystroke) {
            self.redo_note_editor(context);
            context.stop_propagation();
            return;
        }

        match event.keystroke.key.as_str() {
            "tab" => self.replace_note_editor_selection("    "),
            "enter" => self.replace_note_editor_selection("\n"),
            "left" => self.move_note_editor_cursor_left(event.keystroke.modifiers.shift),
            "right" => self.move_note_editor_cursor_right(event.keystroke.modifiers.shift),
            "home" => self.move_note_editor_cursor_home(event.keystroke.modifiers.shift),
            "end" => self.move_note_editor_cursor_end(event.keystroke.modifiers.shift),
            "up" => self.move_note_editor_cursor_vertical(-1, event.keystroke.modifiers.shift),
            "down" => self.move_note_editor_cursor_vertical(1, event.keystroke.modifiers.shift),
            "pageup" => self.move_note_editor_cursor_vertical(
                -(NOTE_EDITOR_PAGE_STEP_LINES as isize),
                event.keystroke.modifiers.shift,
            ),
            "pagedown" => self.move_note_editor_cursor_vertical(
                NOTE_EDITOR_PAGE_STEP_LINES as isize,
                event.keystroke.modifiers.shift,
            ),
            "backspace" => self.delete_note_editor_backward(),
            "delete" => self.delete_note_editor_forward(),
            "escape" => {}
            _ => return,
        }
        self.touch_search_text_cursor_activity();
        context.stop_propagation();
        context.notify();
    }

    /// 判断是否为保存快捷键。
    pub(in crate::app) fn is_save_keystroke(keystroke: &Keystroke) -> bool {
        Self::keystroke_matches_letter_or_control_code(keystroke, "s", "\u{13}")
            && (keystroke.modifiers.control || keystroke.modifiers.platform)
    }

    /// 判断是否为撤销快捷键。
    pub(in crate::app) fn is_undo_keystroke(keystroke: &Keystroke) -> bool {
        Self::keystroke_matches_letter_or_control_code(keystroke, "z", "\u{1a}")
            && (keystroke.modifiers.control || keystroke.modifiers.platform)
            && !keystroke.modifiers.shift
    }

    /// 判断是否为重做快捷键。
    pub(in crate::app) fn is_redo_keystroke(keystroke: &Keystroke) -> bool {
        (Self::keystroke_matches_letter_or_control_code(keystroke, "y", "\u{19}")
            && keystroke.modifiers.control)
            || (Self::keystroke_matches_letter_or_control_code(keystroke, "z", "\u{1a}")
                && keystroke.modifiers.platform
                && keystroke.modifiers.shift)
    }

    /// 撤销笔记编辑器文本。
    pub(in crate::app) fn undo_note_editor(&mut self, context: &mut Context<Self>) {
        let Some(previous) = self.notes.editor_undo_stack.pop() else {
            return;
        };
        self.notes
            .editor_redo_stack
            .push(self.notes.editor_text.clone());
        self.notes.editor_text = previous;
        let cursor = self.notes.editor_text.len();
        self.notes.editor_selection_range = cursor..cursor;
        context.notify();
    }

    /// 重做笔记编辑器文本。
    pub(in crate::app) fn redo_note_editor(&mut self, context: &mut Context<Self>) {
        let Some(next) = self.notes.editor_redo_stack.pop() else {
            return;
        };
        self.notes
            .editor_undo_stack
            .push(self.notes.editor_text.clone());
        self.notes.editor_text = next;
        let cursor = self.notes.editor_text.len();
        self.notes.editor_selection_range = cursor..cursor;
        context.notify();
    }

    /// 光标左移。
    fn move_note_editor_cursor_left(&mut self, extend: bool) {
        self.notes.editor_marked_range = None;
        if extend {
            self.notes.editor_selection_range.end = Self::previous_search_text_boundary(
                &self.notes.editor_text,
                self.notes.editor_selection_range.end,
            );
        } else if self.notes.editor_selection_range.start != self.notes.editor_selection_range.end {
            self.notes.editor_selection_range =
                self.notes.editor_selection_range.start..self.notes.editor_selection_range.start;
        } else {
            let cursor = Self::previous_search_text_boundary(
                &self.notes.editor_text,
                self.notes.editor_selection_range.end,
            );
            self.notes.editor_selection_range = cursor..cursor;
        }
        self.notes.editor_selection_range = Self::clamp_search_text_range(
            &self.notes.editor_text,
            self.notes.editor_selection_range.clone(),
        );
    }

    /// 光标右移。
    fn move_note_editor_cursor_right(&mut self, extend: bool) {
        self.notes.editor_marked_range = None;
        if extend {
            self.notes.editor_selection_range.end = Self::next_search_text_boundary(
                &self.notes.editor_text,
                self.notes.editor_selection_range.end,
            );
        } else if self.notes.editor_selection_range.start != self.notes.editor_selection_range.end {
            self.notes.editor_selection_range =
                self.notes.editor_selection_range.end..self.notes.editor_selection_range.end;
        } else {
            let cursor = Self::next_search_text_boundary(
                &self.notes.editor_text,
                self.notes.editor_selection_range.end,
            );
            self.notes.editor_selection_range = cursor..cursor;
        }
        self.notes.editor_selection_range = Self::clamp_search_text_range(
            &self.notes.editor_text,
            self.notes.editor_selection_range.clone(),
        );
    }

    /// 光标移动到文档起点。
    fn move_note_editor_cursor_home(&mut self, extend: bool) {
        self.notes.editor_marked_range = None;
        if extend {
            self.notes.editor_selection_range.end = 0;
        } else {
            self.notes.editor_selection_range = 0..0;
        }
    }

    /// 光标移动到文档末尾。
    fn move_note_editor_cursor_end(&mut self, extend: bool) {
        self.notes.editor_marked_range = None;
        let end = self.notes.editor_text.len();
        if extend {
            self.notes.editor_selection_range.end = end;
        } else {
            self.notes.editor_selection_range = end..end;
        }
    }

    /// 按可视行移动笔记编辑器光标。
    ///
    /// 业务意图：
    /// - 多行笔记需要符合常见编辑器的上下方向键语义：保留当前字符列并移动到上一行或下一行。
    /// - `PageUp`/`PageDown` 复用同一套计算，只是行数更大，避免跳到全文首尾导致用户丢失编辑位置。
    fn move_note_editor_cursor_vertical(&mut self, line_delta: isize, extend: bool) {
        self.notes.editor_marked_range = None;
        let selection = Self::clamp_search_text_range(
            &self.notes.editor_text,
            self.notes.editor_selection_range.clone(),
        );
        let cursor = if !extend && selection.start != selection.end {
            if line_delta < 0 {
                selection.start
            } else {
                selection.end
            }
        } else {
            selection.end
        };
        let target =
            Self::note_editor_vertical_target_index(&self.notes.editor_text, cursor, line_delta);
        if extend {
            self.notes.editor_selection_range = selection.start..target;
        } else {
            self.notes.editor_selection_range = target..target;
        }
    }

    /// 根据行偏移计算笔记编辑器目标 UTF-8 字节下标。
    ///
    /// 边界条件：
    /// - 中文、emoji 等多字节字符必须按字符列移动，返回值始终是合法 UTF-8 边界。
    /// - 目标行比当前列短时夹到目标行末尾；越过首尾行时夹到第一行或最后一行。
    pub(in crate::app) fn note_editor_vertical_target_index(
        text: &str,
        cursor: usize,
        line_delta: isize,
    ) -> usize {
        let lines = Self::thread_analysis_filter_line_ranges(text);
        if lines.is_empty() {
            return 0;
        }
        let cursor = Self::search_input_clamp_byte_index(text, cursor);
        let current_line_index = Self::note_editor_line_index_for_offset(text, cursor);
        let current_line = &lines[current_line_index];
        let current_local_cursor = cursor
            .saturating_sub(current_line.start)
            .min(current_line.end.saturating_sub(current_line.start));
        let current_line_text = &text[current_line.clone()];
        let target_column =
            Self::char_column_for_byte_index(current_line_text, current_local_cursor);
        let target_line_index = if line_delta < 0 {
            current_line_index.saturating_sub((-line_delta) as usize)
        } else {
            current_line_index
                .saturating_add(line_delta as usize)
                .min(lines.len().saturating_sub(1))
        };
        let target_line = &lines[target_line_index];
        let target_line_text = &text[target_line.clone()];
        target_line.start + Self::byte_index_for_char_column(target_line_text, target_column)
    }

    /// 返回 UTF-8 字节下标所在的笔记编辑器行号。
    ///
    /// 业务意图：
    /// - 自绘编辑器内部没有系统文本控件的行列模型，需要把单一光标偏移映射回逻辑行，供上下方向键复用。
    pub(in crate::app) fn note_editor_line_index_for_offset(text: &str, offset: usize) -> usize {
        let offset = Self::search_input_clamp_byte_index(text, offset);
        let lines = Self::thread_analysis_filter_line_ranges(text);
        lines
            .iter()
            .position(|range| offset <= range.end)
            .unwrap_or_else(|| lines.len().saturating_sub(1))
    }

    /// 删除光标前一个字符或当前选区。
    fn delete_note_editor_backward(&mut self) {
        if self.notes.editor_selection_range.start != self.notes.editor_selection_range.end
            || self.notes.editor_marked_range.is_some()
        {
            self.replace_note_editor_selection("");
        } else if let Some((previous_index, _)) = self.notes.editor_text
            [..self.notes.editor_selection_range.end]
            .char_indices()
            .next_back()
        {
            self.record_note_editor_undo_snapshot();
            let cursor = self.notes.editor_selection_range.end;
            self.notes
                .editor_text
                .replace_range(previous_index..cursor, "");
            self.notes.editor_selection_range = previous_index..previous_index;
            self.notes.editor_marked_range = None;
        }
    }

    /// 删除光标后一个字符或当前选区。
    fn delete_note_editor_forward(&mut self) {
        if self.notes.editor_selection_range.start != self.notes.editor_selection_range.end
            || self.notes.editor_marked_range.is_some()
        {
            self.replace_note_editor_selection("");
        } else if let Some((next_index, next_character)) = self.notes.editor_text
            [self.notes.editor_selection_range.end..]
            .char_indices()
            .next()
        {
            self.record_note_editor_undo_snapshot();
            let start = self.notes.editor_selection_range.end + next_index;
            let end = start + next_character.len_utf8();
            self.notes.editor_text.replace_range(start..end, "");
            self.notes.editor_selection_range = start..start;
            self.notes.editor_marked_range = None;
        }
    }

    /// 根据只读源码行和横坐标生成文本位置。
    pub(in crate::app) fn note_source_position_from_pointer(
        &self,
        line_index: usize,
        line: &str,
        pointer_x: Pixels,
    ) -> NoteTextPosition {
        let local_x = (f32::from(pointer_x) - NOTES_SOURCE_HORIZONTAL_PADDING).max(0.0);
        let column = (local_x / NOTES_SOURCE_CHAR_WIDTH).round().max(0.0) as usize;
        NoteTextPosition {
            line_index,
            column: column.min(line.chars().count()),
        }
    }

    /// 开始只读源码选区。
    pub(in crate::app) fn start_note_source_selection(
        &mut self,
        line_index: usize,
        line: &str,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let position = self.note_source_position_from_pointer(line_index, line, event.position.x);
        window.focus(&self.root_focus_handle);
        let selection = match event.click_count {
            0 | 1 => NoteTextSelection {
                anchor: position,
                focus: position,
            },
            2 => Self::note_word_selection_for_position(line_index, line, position).unwrap_or(
                NoteTextSelection {
                    anchor: position,
                    focus: position,
                },
            ),
            _ => NoteTextSelection {
                anchor: NoteTextPosition {
                    line_index,
                    column: 0,
                },
                focus: NoteTextPosition {
                    line_index,
                    column: line.chars().count(),
                },
            },
        };
        self.notes.source_selection = Some(selection);
        self.notes.source_selection_drag_anchor = (event.click_count <= 1).then_some(position);
        context.notify();
    }

    /// 更新只读源码拖拽选区。
    pub(in crate::app) fn update_note_source_selection(
        &mut self,
        line_index: usize,
        line: &str,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        if !event.dragging() {
            self.finish_note_source_selection(context);
            return;
        }
        let Some(anchor) = self.notes.source_selection_drag_anchor else {
            return;
        };
        let position = self.note_source_position_from_pointer(line_index, line, event.position.x);
        self.notes.source_selection = Some(NoteTextSelection {
            anchor,
            focus: position,
        });
        context.notify();
    }

    /// 结束只读源码选区拖拽。
    pub(in crate::app) fn finish_note_source_selection(&mut self, context: &mut Context<Self>) {
        if self.notes.source_selection_drag_anchor.take().is_some() {
            context.notify();
        }
    }

    /// 返回当前笔记源码选中文本。
    pub(in crate::app) fn selected_note_source_text(&self) -> Option<String> {
        let note = self.notes.active_note.as_ref()?;
        let selection = self.notes.source_selection.as_ref()?;
        if selection.is_empty() {
            return None;
        }
        let lines = Self::note_source_lines(&note.content);
        let (start, end) = selection.normalized();
        if lines.is_empty() || start.line_index >= lines.len() {
            return None;
        }
        let end_line_index = end.line_index.min(lines.len().saturating_sub(1));
        if start.line_index > end_line_index {
            return None;
        }
        let mut text = String::new();
        for line_index in start.line_index..=end_line_index {
            if line_index > start.line_index {
                text.push('\n');
            }
            let line = lines[line_index];
            let Some(range) = Self::selected_byte_range_for_note_line(selection, line_index, line)
            else {
                continue;
            };
            text.push_str(&line[range]);
        }
        Some(text)
    }

    /// 复制当前笔记源码选区。
    pub(in crate::app) fn copy_selected_note_source_text(
        &self,
        context: &mut Context<Self>,
    ) -> bool {
        let Some(text) = self.selected_note_source_text() else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        context.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// 将笔记正文拆成源码阅读器行。
    pub(in crate::app) fn note_source_lines(content: &str) -> Vec<&str> {
        if content.is_empty() {
            Vec::new()
        } else {
            content.split('\n').collect()
        }
    }

    /// 返回某一行被选择覆盖的 UTF-8 字节范围。
    pub(in crate::app) fn selected_byte_range_for_note_line(
        selection: &NoteTextSelection,
        line_index: usize,
        line: &str,
    ) -> Option<Range<usize>> {
        let (start, end) = selection.normalized();
        if line_index < start.line_index || line_index > end.line_index {
            return None;
        }
        let line_char_count = line.chars().count();
        let start_column = if line_index == start.line_index {
            start.column.min(line_char_count)
        } else {
            0
        };
        let end_column = if line_index == end.line_index {
            end.column.min(line_char_count)
        } else {
            line_char_count
        };
        let start_byte = Self::byte_index_for_char_column(line, start_column);
        let end_byte = Self::byte_index_for_char_column(line, end_column);
        (start_byte < end_byte).then_some(start_byte..end_byte)
    }

    /// 根据点击位置构造当前词选区。
    pub(in crate::app) fn note_word_selection_for_position(
        line_index: usize,
        line: &str,
        position: NoteTextPosition,
    ) -> Option<NoteTextSelection> {
        let chars = line.chars().collect::<Vec<_>>();
        if chars.is_empty() {
            return None;
        }
        let mut token_column = position.column.min(chars.len().saturating_sub(1));
        if !chars[token_column].is_alphanumeric() && token_column > 0 {
            token_column -= 1;
        }
        if !chars[token_column].is_alphanumeric() {
            return None;
        }
        let mut start_column = token_column;
        while start_column > 0 && chars[start_column - 1].is_alphanumeric() {
            start_column -= 1;
        }
        let mut end_column = token_column + 1;
        while end_column < chars.len() && chars[end_column].is_alphanumeric() {
            end_column += 1;
        }
        Some(NoteTextSelection {
            anchor: NoteTextPosition {
                line_index,
                column: start_column,
            },
            focus: NoteTextPosition {
                line_index,
                column: end_column,
            },
        })
    }
}
