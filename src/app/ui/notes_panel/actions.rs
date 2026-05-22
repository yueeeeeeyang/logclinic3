// 笔记 MainView 状态方法。
//
// 业务意图：
// - 本文件集中维护笔记树创建/选择/删除、阅读器选区、编辑器快捷键、保存和确认弹窗状态流转。
// - 方法仍实现到 MainView 上，保持和其它 app UI 动作模块一致的调用方式，不引入新的公开状态对象。

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use super::*;

impl MainView {
    /// 返回笔记物理根目录路径，失败时同步记录错误。
    pub(in crate::app) fn notes_root_dir_or_error(&mut self) -> Option<PathBuf> {
        let path = notes_root_dir();
        if path.is_none() {
            self.notes.database_error =
                Some("当前平台没有可用的应用配置目录，无法保存笔记文件".to_string());
        }
        path
    }

    /// 清空并关闭当前笔记 AI 临时会话。
    ///
    /// 业务意图：
    /// - 笔记 AI 请求会携带当前文档全文；切换、删除或退出编辑时必须停止旧任务并清空消息，避免旧上下文误用于其它笔记。
    /// - 该方法只影响当前内存会话，不触碰全局 AI 对话数据库和笔记 Markdown 文件。
    pub(in crate::app) fn reset_notes_ai_assistant_session(&mut self) {
        self.notes.ai.is_open = false;
        self.notes.ai.reset_session();
    }

    /// 切换笔记 AI 侧边栏开关。
    ///
    /// UI 约束：
    /// - 入口只在编辑态可用；如果通过快捷路径误调用，必须静默返回，避免只读阅读器出现可写 AI 工具。
    /// - 打开时会选择默认模型并聚焦输入框，让用户可以直接输入生成要求。
    pub(in crate::app) fn toggle_notes_ai_assistant(
        &mut self,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.notes.active_note.is_none() || !self.notes.is_editing {
            return;
        }
        if self.notes.ai.is_open {
            self.stop_notes_ai_streaming_without_notify(AiChatMessageStatus::Stopped, None);
            self.notes.ai.is_open = false;
        } else {
            self.ensure_notes_ai_model_selection();
            self.notes.ai.is_open = true;
            self.notes.ai.error_message = None;
            window.focus(&self.notes.ai.input_focus);
        }
        context.notify();
    }

    /// 确保笔记 AI 有一个可用模型选择。
    ///
    /// 业务意图：
    /// - 模型配置可能在设置页被删除或修改；发送前和打开侧边栏时都要重新校正，避免持有悬空模型 ID。
    pub(in crate::app) fn ensure_notes_ai_model_selection(&mut self) {
        let current_is_valid = self
            .notes
            .ai
            .selected_model_profile_id
            .as_deref()
            .is_some_and(|profile_id| {
                self.model_config
                    .model_config_profiles
                    .iter()
                    .any(|profile| profile.id == profile_id)
            });
        if current_is_valid {
            return;
        }
        self.notes.ai.selected_model_profile_id = ai_chat_default_model_profile_id(
            &self.model_config.model_config_profiles,
            self.model_config.model_config_default_profile_id.as_deref(),
        );
    }

    /// 返回笔记 AI 当前选择的模型配置。
    pub(in crate::app) fn active_notes_ai_model_profile(&self) -> Option<ModelProfile> {
        let profile_id = self.notes.ai.selected_model_profile_id.as_deref()?;
        self.model_config
            .model_config_profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
    }

    /// 判断笔记 AI 当前是否可以发送。
    ///
    /// 边界条件：
    /// - 必须处于编辑态并有当前笔记；AI 生成结果只写入编辑草稿，不允许在只读态暗中修改文档。
    /// - 正在流式生成时禁止再次发送，避免多个后台任务同时写同一个临时助手消息。
    pub(in crate::app) fn notes_ai_can_send(&self) -> bool {
        self.notes.active_note.is_some()
            && self.notes.is_editing
            && self.notes.ai.streaming_task.is_none()
            && self.active_notes_ai_model_profile().is_some()
            && !self.notes.ai.input_text.trim().is_empty()
    }

    /// 返回当前笔记 AI 请求应携带的文档上下文。
    ///
    /// 业务意图：
    /// - 上下文必须来自当前编辑器草稿，包括尚未保存的标题和正文 Markdown，不能重新读取磁盘上的旧文件。
    pub(in crate::app) fn current_notes_ai_document_context(
        &self,
    ) -> Option<NotesAiDocumentContext> {
        self.notes.active_note.as_ref()?;
        Some(NotesAiDocumentContext {
            title: self.notes.editor_title.clone(),
            markdown: self.notes.rich_editor.markdown_content(),
        })
    }

    /// 选择笔记 AI 使用的模型配置。
    pub(in crate::app) fn select_notes_ai_model_profile(
        &mut self,
        profile_id: String,
        context: &mut Context<Self>,
    ) {
        if self
            .model_config
            .model_config_profiles
            .iter()
            .any(|profile| profile.id == profile_id)
        {
            self.notes.ai.selected_model_profile_id = Some(profile_id);
            self.notes.ai.model_menu_open = false;
            self.notes.ai.error_message = None;
        }
        context.notify();
    }

    /// 重新加载笔记树并保持可见状态。
    pub(in crate::app) fn reload_notes_tree(&mut self) {
        let Some(path) = notes_root_dir() else {
            self.notes.database_error =
                Some("当前平台没有可用的应用配置目录，无法保存笔记文件".to_string());
            return;
        };
        match load_note_tree(&path) {
            Ok(rows) => {
                self.notes.tree_rows = rows;
                self.notes.database_error = None;
                self.notes.rebuild_visible_rows();
                self.reconcile_active_note_after_tree_reload();
            }
            Err(error) => self.notes.database_error = Some(error),
        }
    }

    /// 清空笔记树搜索框并恢复普通树展开状态。
    ///
    /// 业务意图：
    /// - 清空搜索不应改变用户原先展开/收起的目录集合，只重新用当前展开状态派生可见行。
    /// - 搜索框布局缓存包含旧文本的字形位置，清空后必须一起丢弃，避免下一次点击命中到旧宽度。
    pub(in crate::app) fn clear_notes_tree_search(&mut self, context: &mut Context<Self>) {
        self.notes.tree_search.input = SingleLineTextInputState::empty();
        self.notes.tree_search.clear_layout();
        self.notes.rebuild_visible_rows();
        self.notes.tree_context_menu = None;
        self.notes.tree_create_menu_open = false;
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 返回笔记树搜索框绘制快照。
    pub(in crate::app) fn notes_tree_search_text_snapshot(
        &self,
    ) -> Option<SingleLineTextInputSnapshot> {
        Some(SingleLineTextInputSnapshot {
            text: self.notes.tree_search.input.text.clone(),
            selection_range: self.notes.tree_search.input.selection_range.clone(),
            marked_range: self.notes.tree_search.input.marked_range.clone(),
            horizontal_scroll_px: self.notes.tree_search.input.horizontal_scroll_px,
        })
    }

    /// 保存笔记树搜索框最近一次文本布局。
    pub(in crate::app) fn store_notes_tree_search_text_layout(
        &mut self,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
        horizontal_scroll_px: f32,
    ) {
        self.notes
            .tree_search
            .store_layout(line, bounds, horizontal_scroll_px);
    }

    /// 根据鼠标窗口坐标返回笔记树搜索框中的 UTF-8 字节下标。
    pub(in crate::app) fn notes_tree_search_text_index_for_point(
        &self,
        position: gpui::Point<Pixels>,
    ) -> usize {
        let state = &self.notes.tree_search;
        let text = &state.input.text;
        let (Some(layout), Some(bounds)) = (state.last_layout.as_ref(), state.last_bounds.as_ref())
        else {
            return text.len();
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return text.len();
        }
        let display_index = layout
            .closest_index_for_x(position.x - bounds.left() + px(state.input.horizontal_scroll_px));
        text_input_clamp_byte_index(text, display_index.min(text.len()))
    }

    /// 处理笔记树搜索框按键。
    ///
    /// 业务意图：
    /// - 普通字符和中文 IME 由 `EntityInputHandler` 提交；这里只处理复制粘贴、方向键、删除和 Escape 清空。
    /// - 搜索框只过滤已加载的树标题，不触发文件扫描或保存。
    pub(in crate::app) fn handle_notes_tree_search_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.handle_notes_tree_search_single_line_key_down(event, context) {
            return;
        }

        if event.keystroke.key == "escape" && !self.notes.tree_search.input.text.is_empty() {
            self.clear_notes_tree_search(context);
            context.stop_propagation();
        }
    }

    /// 处理笔记树搜索框的通用单行编辑按键。
    fn handle_notes_tree_search_single_line_key_down(
        &mut self,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) -> bool {
        if Self::is_paste_keystroke(&event.keystroke) {
            if let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) {
                replace_text_input_selection(
                    &mut self.notes.tree_search.input,
                    &Self::sanitize_search_input_text(&text),
                );
                self.after_notes_tree_search_changed();
            }
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return true;
        }

        let input = &mut self.notes.tree_search.input;
        if Self::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = text_input_selected_text(input) {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return true;
        }
        if Self::is_cut_keystroke(&event.keystroke) {
            if let Some(text) = text_input_selected_text(input) {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                replace_text_input_selection(input, "");
                self.after_notes_tree_search_changed();
            }
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return true;
        }
        if Self::is_select_all_keystroke(&event.keystroke) {
            select_all_text_input(input);
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return true;
        }

        let outcome = match event.keystroke.key.as_str() {
            "left" => move_text_input_left(input, event.keystroke.modifiers.shift),
            "right" => move_text_input_right(input, event.keystroke.modifiers.shift),
            "home" | "up" => move_text_input_home(input, event.keystroke.modifiers.shift),
            "end" | "down" => move_text_input_end(input, event.keystroke.modifiers.shift),
            "backspace" => backspace_text_input(input),
            "delete" => delete_text_input(input),
            _ => TextInputEditOutcome::default(),
        };
        if outcome.consumed {
            if outcome.changed {
                self.after_notes_tree_search_changed();
            }
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return true;
        }
        false
    }

    /// 笔记树搜索文本变化后的派生状态更新。
    pub(in crate::app) fn after_notes_tree_search_changed(&mut self) {
        self.notes.tree_search.clear_layout();
        self.notes.rebuild_visible_rows();
        self.notes.tree_context_menu = None;
        self.notes.tree_create_menu_open = false;
    }

    /// 请求手动刷新物理笔记目录。
    ///
    /// 业务意图：
    /// - 物理 Markdown 文件可能被外部编辑器新增、删除或重命名；本版本不做文件监听，用户通过刷新按钮显式同步。
    /// - 如果当前富文本编辑器有未保存修改，必须沿用现有确认弹窗，避免刷新时丢失草稿。
    pub(in crate::app) fn request_refresh_notes_tree(&mut self, context: &mut Context<Self>) {
        if self.notes.has_unsaved_changes() {
            self.notes.unsaved_dialog = Some(NotesUnsavedDialog {
                action: NotesPendingAction::Refresh,
            });
            context.notify();
            return;
        }
        self.reload_notes_tree();
        context.notify();
    }

    /// 刷新笔记树后校正当前打开的笔记状态。
    ///
    /// 边界条件：
    /// - 外部删除当前文件后，右侧不能继续显示已经不存在的正文。
    /// - 外部修改当前文件且当前不在编辑态时，刷新应重新读取正文。
    fn reconcile_active_note_after_tree_reload(&mut self) {
        let Some(active_note) = self.notes.active_note.clone() else {
            return;
        };
        let still_exists = self
            .notes
            .tree_rows
            .iter()
            .any(|row| row.kind == NoteTreeRowKind::Note && row.id == active_note.id);
        if !still_exists {
            self.notes.active_note = None;
            self.notes.selected = None;
            self.notes.is_editing = false;
            self.notes
                .rich_editor
                .reset_document(NoteRichTextDocument::empty());
            self.notes.editor_title.clear();
            self.notes.title_selection_range = 0..0;
            self.reset_notes_ai_assistant_session();
            return;
        }
        if !self.notes.is_editing {
            self.load_note_into_workspace(&active_note.id);
        }
    }

    /// 将笔记正文加载为富文本文档。
    ///
    /// 业务意图：
    /// - Markdown 笔记在打开时转换为富文本文档，保存时再导出 Markdown，避免编辑器状态直接泄漏到物理文件格式。
    /// - 富文本 JSON 解析失败说明单篇笔记数据损坏，需要展示中文错误并阻止用户在错误文档上误保存。
    ///
    /// 边界条件：
    /// - `focus` 只在真实窗口交互路径启用；测试路径可只重置文档而不触碰平台焦点。
    fn reset_note_rich_editor_from_note(
        &mut self,
        note: &Note,
        focus: bool,
        window: Option<&mut Window>,
    ) -> bool {
        match rich_text_document_from_note(note) {
            Ok(document) => {
                self.notes.rich_editor.reset_document(document);
                if focus && let Some(window) = window {
                    window.focus(&self.notes.rich_editor.focus);
                }
                self.notes.database_error = None;
                true
            }
            Err(error) => {
                self.notes
                    .rich_editor
                    .reset_document(NoteRichTextDocument::empty());
                self.notes.database_error = Some(error);
                false
            }
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
        self.notes.tree_create_menu_open = false;
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
        self.notes.tree_create_menu_open = false;
        self.notes.delete_confirm_dialog = None;
        self.reset_notes_ai_assistant_session();

        match selection.kind {
            NoteTreeRowKind::Directory => {
                // 搜索模式的目录只是命中笔记的路径上下文，点击时不能写入普通树展开集合；
                // 否则清空搜索后会看到目录被搜索过程意外展开或收起。
                if !note_tree_search_is_active(&self.notes.tree_search.input.text) {
                    self.toggle_note_directory(&selection.id);
                }
                self.notes.active_note = None;
                self.notes.is_editing = false;
                self.notes
                    .rich_editor
                    .reset_document(NoteRichTextDocument::empty());
                self.notes.editor_title.clear();
                self.notes.title_selection_range = 0..0;
                self.notes.title_marked_range = None;
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
        let Some(path) = self.notes_root_dir_or_error() else {
            return;
        };
        match load_note(&path, note_id) {
            Ok(Some(note)) => {
                self.reset_notes_ai_assistant_session();
                self.notes.editor_title = note.title.clone();
                let title_cursor = self.notes.editor_title.len();
                self.notes.title_selection_range = title_cursor..title_cursor;
                self.notes.title_marked_range = None;
                self.reset_note_rich_editor_from_note(&note, false, None);
                self.notes.active_note = Some(note);
                self.notes.is_editing = false;
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

    /// 切换笔记树顶部新增菜单。
    ///
    /// 业务意图：
    /// - 工具栏只暴露一个新增入口，点击后再通过菜单区分新建目录或笔记。
    /// - 打开新增菜单时需要关闭右键菜单，避免两个浮层同时存在并争抢点击遮罩。
    pub(in crate::app) fn toggle_notes_tree_create_menu(&mut self, context: &mut Context<Self>) {
        self.notes.tree_create_menu_open = !self.notes.tree_create_menu_open;
        self.notes.tree_context_menu = None;
        context.notify();
    }

    /// 执行笔记树顶部新增菜单命令。
    pub(in crate::app) fn handle_notes_tree_create_menu_action(
        &mut self,
        action: NotesTreeContextMenuAction,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.notes.tree_create_menu_open = false;
        match action {
            NotesTreeContextMenuAction::NewDirectory => {
                self.create_note_directory_from_toolbar(context);
            }
            NotesTreeContextMenuAction::NewNote => {
                self.create_note_in_selected_directory(window, context);
            }
            NotesTreeContextMenuAction::Rename
            | NotesTreeContextMenuAction::Delete
            | NotesTreeContextMenuAction::Plugin { .. } => {}
        }
        context.stop_propagation();
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
        let Some(path) = self.notes_root_dir_or_error() else {
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
                self.notes
                    .rich_editor
                    .reset_document(NoteRichTextDocument::empty());
                self.notes.editor_title.clear();
                self.notes.title_selection_range = 0..0;
                self.reset_notes_ai_assistant_session();
            }
            Err(error) => self.notes.database_error = Some(error),
        }
        context.notify();
    }

    /// 创建笔记。
    pub(in crate::app) fn create_note_in_selected_directory(
        &mut self,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let directory_id = self.selected_note_parent_directory_id();
        self.request_create_note(directory_id, Some(window), context);
    }

    /// 请求创建笔记，必要时先处理未保存编辑草稿。
    pub(in crate::app) fn request_create_note(
        &mut self,
        directory_id: Option<String>,
        window: Option<&mut Window>,
        context: &mut Context<Self>,
    ) {
        if self.notes.has_unsaved_changes() {
            self.notes.unsaved_dialog = Some(NotesUnsavedDialog {
                action: NotesPendingAction::CreateNote(directory_id),
            });
            context.notify();
            return;
        }
        self.create_note_under(directory_id, window, context);
    }

    /// 创建笔记并自动进入编辑状态。
    pub(in crate::app) fn create_note_under(
        &mut self,
        directory_id: Option<String>,
        window: Option<&mut Window>,
        context: &mut Context<Self>,
    ) {
        let Some(path) = self.notes_root_dir_or_error() else {
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
                self.notes.editor_title = note.title.clone();
                let title_cursor = self.notes.editor_title.len();
                self.notes.title_selection_range = title_cursor..title_cursor;
                self.notes.title_marked_range = None;
                self.reset_note_rich_editor_from_note(&note, false, None);
                self.notes.is_editing = true;
                self.reset_notes_ai_assistant_session();
                if let Some(window) = window {
                    window.focus(&self.notes.rich_editor.focus);
                }
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
        self.notes.tree_create_menu_open = false;
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

    /// 确认重命名弹窗并重命名真实目录或 Markdown 文件。
    pub(in crate::app) fn confirm_note_rename(&mut self, context: &mut Context<Self>) -> bool {
        let Some(dialog) = self.notes.rename_dialog.take() else {
            return false;
        };
        let Some(path) = self.notes_root_dir_or_error() else {
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
            Ok(next_id) => {
                if let Some(note) = self.notes.active_note.as_mut()
                    && note.id == dialog.target.id
                {
                    note.id = next_id.clone();
                    note.title = next_title.to_string();
                }
                if dialog.target.kind == NoteTreeRowKind::Directory {
                    if let Some(note) = self.notes.active_note.as_mut() {
                        Self::rewrite_note_path_value_prefix(
                            &mut note.id,
                            &dialog.target.id,
                            &next_id,
                        );
                        Self::rewrite_optional_note_path_prefix(
                            &mut note.directory_id,
                            &dialog.target.id,
                            &next_id,
                        );
                    }
                    if let Some(selection) = self.notes.selected.as_mut() {
                        Self::rewrite_note_path_value_prefix(
                            &mut selection.id,
                            &dialog.target.id,
                            &next_id,
                        );
                    }
                    self.rewrite_expanded_note_directory_prefix(&dialog.target.id, &next_id);
                }
                if let Some(selection) = self.notes.selected.as_mut()
                    && selection.id == dialog.target.id
                    && selection.kind == dialog.target.kind
                {
                    selection.id = next_id.clone();
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
        let Some(path) = self.notes_root_dir_or_error() else {
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
                    self.notes
                        .rich_editor
                        .reset_document(NoteRichTextDocument::empty());
                    self.notes.editor_title.clear();
                    self.notes.title_selection_range = 0..0;
                    self.reset_notes_ai_assistant_session();
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
    /// - 删除目录会把真实目录移入系统回收站，UI 也必须同步清空右侧旧正文，避免用户继续查看或编辑已经不存在的笔记。
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

    /// 重写已展开目录集合中的路径前缀。
    ///
    /// 业务意图：
    /// - 物理文件存储下目录 ID 就是相对路径，重命名目录会同步改变所有子路径；展开状态也要跟随路径变化。
    /// - 如果不处理，用户重命名展开目录后会看到子树意外折叠，且旧路径会残留在会话状态中。
    fn rewrite_expanded_note_directory_prefix(&mut self, old_id: &str, next_id: &str) {
        let old_prefix = format!("{old_id}/");
        self.notes.expanded_directory_ids = self
            .notes
            .expanded_directory_ids
            .iter()
            .map(|id| {
                if id == old_id {
                    next_id.to_string()
                } else if let Some(suffix) = id.strip_prefix(&old_prefix) {
                    format!("{next_id}/{suffix}")
                } else {
                    id.clone()
                }
            })
            .collect();
    }

    /// 重写单个笔记路径 ID 的目录前缀。
    ///
    /// 业务意图：
    /// - 目录重命名后，所有子目录和子笔记的 ID 都随真实路径变化；当前打开笔记和选中节点必须同步更新，刷新树时才不会被误判为已删除。
    fn rewrite_note_path_value_prefix(value: &mut String, old_id: &str, next_id: &str) -> bool {
        if value == old_id {
            *value = next_id.to_string();
            return true;
        }
        let old_prefix = format!("{old_id}/");
        if let Some(suffix) = value.strip_prefix(&old_prefix) {
            *value = format!("{next_id}/{suffix}");
            return true;
        }
        false
    }

    /// 重写可选笔记路径 ID 的目录前缀。
    fn rewrite_optional_note_path_prefix(
        value: &mut Option<String>,
        old_id: &str,
        next_id: &str,
    ) -> bool {
        let Some(value) = value.as_mut() else {
            return false;
        };
        Self::rewrite_note_path_value_prefix(value, old_id, next_id)
    }

    /// 取消删除确认。
    pub(in crate::app) fn cancel_note_delete(&mut self, context: &mut Context<Self>) {
        self.notes.delete_confirm_dialog = None;
        context.notify();
    }

    /// 读取笔记 AI 输入区当前文本、选择范围和组合文本范围的快照。
    pub(in crate::app) fn notes_ai_input_text_snapshot(
        &self,
    ) -> (String, Range<usize>, Option<Range<usize>>) {
        (
            self.notes.ai.input_text.clone(),
            Self::clamp_search_text_range(
                &self.notes.ai.input_text,
                self.notes.ai.input_selection_range.clone(),
            ),
            self.notes.ai.input_marked_range.clone(),
        )
    }

    /// 保存笔记 AI 输入区最近一次多行排版结果。
    pub(in crate::app) fn store_notes_ai_input_text_layouts(
        &mut self,
        layouts: Vec<NotesAiInputLineLayout>,
        bounds: Bounds<Pixels>,
    ) -> bool {
        let layout_count_changed = self.notes.ai.input_last_layouts.len() != layouts.len();
        self.notes.ai.input_last_layouts = layouts;
        self.notes.ai.input_last_bounds = Some(bounds);
        layout_count_changed
    }

    /// 返回笔记 AI 输入区当前内容需要的可视行数。
    pub(in crate::app) fn notes_ai_input_visual_line_count(&self) -> usize {
        let hard_line_count = Self::thread_analysis_filter_line_ranges(&self.notes.ai.input_text)
            .len()
            .max(1);
        self.notes
            .ai
            .input_last_layouts
            .len()
            .max(hard_line_count)
            .max(1)
    }

    /// 根据窗口坐标返回笔记 AI 输入区 UTF-8 字节下标。
    pub(in crate::app) fn notes_ai_input_index_for_point(&self, position: Point<Pixels>) -> usize {
        if self.notes.ai.input_text.is_empty() {
            return 0;
        }
        for layout in &self.notes.ai.input_last_layouts {
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
        if let Some(bounds) = &self.notes.ai.input_last_bounds
            && position.y < bounds.top()
        {
            return 0;
        }
        self.notes.ai.input_text.len()
    }

    /// 开始笔记 AI 输入区鼠标选择。
    pub(in crate::app) fn start_notes_ai_input_mouse_selection(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let index = self.notes_ai_input_index_for_point(event.position);
        self.notes.ai.input_marked_range = None;
        match event.click_count {
            0 | 1 => {
                if event.modifiers.shift {
                    self.notes.ai.input_selection_range.end = index;
                    self.notes.ai.input_selection_range = Self::clamp_search_text_range(
                        &self.notes.ai.input_text,
                        self.notes.ai.input_selection_range.clone(),
                    );
                } else {
                    self.notes.ai.input_selection_range = index..index;
                }
                self.notes.ai.input_selection_drag =
                    Some(self.notes.ai.input_selection_range.start);
            }
            2 => {
                self.notes.ai.input_selection_range =
                    Self::search_text_word_range_for_index(&self.notes.ai.input_text, index);
                self.notes.ai.input_selection_drag = None;
            }
            _ => {
                self.notes.ai.input_selection_range = 0..self.notes.ai.input_text.len();
                self.notes.ai.input_selection_drag = None;
            }
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 鼠标拖拽时更新笔记 AI 输入区选区终点。
    pub(in crate::app) fn update_notes_ai_input_mouse_selection(
        &mut self,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) {
        let Some(anchor) = self.notes.ai.input_selection_drag else {
            return;
        };
        let index = self.notes_ai_input_index_for_point(position);
        self.notes.ai.input_marked_range = None;
        self.notes.ai.input_selection_range =
            Self::clamp_search_text_range(&self.notes.ai.input_text, anchor..index);
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 结束笔记 AI 输入区鼠标拖拽选择。
    pub(in crate::app) fn finish_notes_ai_input_mouse_selection(
        &mut self,
        context: &mut Context<Self>,
    ) {
        if self.notes.ai.input_selection_drag.take().is_some() {
            context.notify();
        }
    }

    /// 返回笔记 AI 输入区当前选中文本。
    pub(in crate::app) fn selected_notes_ai_input_text(&self) -> Option<String> {
        let range = Self::clamp_search_text_range(
            &self.notes.ai.input_text,
            self.notes.ai.input_selection_range.clone(),
        );
        (range.start < range.end).then(|| self.notes.ai.input_text[range].to_string())
    }

    /// 用给定文本替换笔记 AI 输入区当前选区。
    pub(in crate::app) fn replace_notes_ai_input_selection(&mut self, replacement: &str) {
        let replacement = replacement.replace("\r\n", "\n").replace('\r', "\n");
        let range = self.notes.ai.input_marked_range.take().unwrap_or_else(|| {
            Self::clamp_search_text_range(
                &self.notes.ai.input_text,
                self.notes.ai.input_selection_range.clone(),
            )
        });
        self.notes
            .ai
            .input_text
            .replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        self.notes.ai.input_selection_range = cursor..cursor;
    }

    /// 处理笔记 AI 输入区按键。
    ///
    /// 业务意图：
    /// - Enter 发送、Shift+Enter 换行；普通字符和中文 IME 提交继续交给平台输入协议，避免手写按键字符破坏输入法。
    pub(in crate::app) fn handle_notes_ai_input_key_down(
        &mut self,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_paste_keystroke(&event.keystroke) {
            if let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) {
                self.replace_notes_ai_input_selection(&text);
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if Self::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_notes_ai_input_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return;
        }

        if Self::is_cut_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_notes_ai_input_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                self.replace_notes_ai_input_selection("");
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if Self::is_select_all_keystroke(&event.keystroke) {
            self.notes.ai.input_marked_range = None;
            self.notes.ai.input_selection_range = 0..self.notes.ai.input_text.len();
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return;
        }

        match event.keystroke.key.as_str() {
            "enter" => {
                if event.keystroke.modifiers.shift {
                    self.replace_notes_ai_input_selection("\n");
                    self.touch_search_text_cursor_activity();
                } else {
                    self.start_notes_ai_send(context);
                }
                context.stop_propagation();
                context.notify();
            }
            "left" => {
                self.notes.ai.input_marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.notes.ai.input_selection_range.end = Self::previous_search_text_boundary(
                        &self.notes.ai.input_text,
                        self.notes.ai.input_selection_range.end,
                    );
                    self.notes.ai.input_selection_range = Self::clamp_search_text_range(
                        &self.notes.ai.input_text,
                        self.notes.ai.input_selection_range.clone(),
                    );
                } else if self.notes.ai.input_selection_range.start
                    != self.notes.ai.input_selection_range.end
                {
                    self.notes.ai.input_selection_range = self.notes.ai.input_selection_range.start
                        ..self.notes.ai.input_selection_range.start;
                } else {
                    let cursor = Self::previous_search_text_boundary(
                        &self.notes.ai.input_text,
                        self.notes.ai.input_selection_range.end,
                    );
                    self.notes.ai.input_selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "right" => {
                self.notes.ai.input_marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.notes.ai.input_selection_range.end = Self::next_search_text_boundary(
                        &self.notes.ai.input_text,
                        self.notes.ai.input_selection_range.end,
                    );
                    self.notes.ai.input_selection_range = Self::clamp_search_text_range(
                        &self.notes.ai.input_text,
                        self.notes.ai.input_selection_range.clone(),
                    );
                } else if self.notes.ai.input_selection_range.start
                    != self.notes.ai.input_selection_range.end
                {
                    self.notes.ai.input_selection_range = self.notes.ai.input_selection_range.end
                        ..self.notes.ai.input_selection_range.end;
                } else {
                    let cursor = Self::next_search_text_boundary(
                        &self.notes.ai.input_text,
                        self.notes.ai.input_selection_range.end,
                    );
                    self.notes.ai.input_selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "up" => {
                self.notes.ai.input_marked_range = None;
                self.notes.ai.input_selection_range = 0..0;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "down" => {
                self.notes.ai.input_marked_range = None;
                let cursor = self.notes.ai.input_text.len();
                self.notes.ai.input_selection_range = cursor..cursor;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "backspace" => {
                if self.notes.ai.input_selection_range.start
                    != self.notes.ai.input_selection_range.end
                    || self.notes.ai.input_marked_range.is_some()
                {
                    self.replace_notes_ai_input_selection("");
                } else if let Some((previous_index, _)) = self.notes.ai.input_text
                    [..self.notes.ai.input_selection_range.end]
                    .char_indices()
                    .next_back()
                {
                    let cursor = self.notes.ai.input_selection_range.end;
                    self.notes
                        .ai
                        .input_text
                        .replace_range(previous_index..cursor, "");
                    self.notes.ai.input_selection_range = previous_index..previous_index;
                    self.notes.ai.input_marked_range = None;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "delete" => {
                if self.notes.ai.input_selection_range.start
                    != self.notes.ai.input_selection_range.end
                    || self.notes.ai.input_marked_range.is_some()
                {
                    self.replace_notes_ai_input_selection("");
                } else if let Some((next_index, next_character)) = self.notes.ai.input_text
                    [self.notes.ai.input_selection_range.end..]
                    .char_indices()
                    .next()
                {
                    let start = self.notes.ai.input_selection_range.end + next_index;
                    let end = start + next_character.len_utf8();
                    self.notes.ai.input_text.replace_range(start..end, "");
                    self.notes.ai.input_selection_range = start..start;
                    self.notes.ai.input_marked_range = None;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "escape" => {}
            _ => {}
        }
    }

    /// 开始发送笔记 AI 生成请求。
    ///
    /// 业务意图：
    /// - 请求必须在后台执行，并且每次发送前都重新读取当前编辑器草稿 Markdown，确保模型使用最新未保存内容。
    /// - 笔记 AI 不写入 `ai-chat.db`；消息只追加到当前侧边栏内存列表。
    pub(in crate::app) fn start_notes_ai_send(&mut self, context: &mut Context<Self>) {
        self.ensure_notes_ai_model_selection();
        if !self.notes_ai_can_send() {
            if self.active_notes_ai_model_profile().is_none() {
                self.notes.ai.error_message = Some("需要先在设置中配置一个可用模型".to_string());
                context.notify();
            }
            return;
        }
        let Some(profile) = self.active_notes_ai_model_profile() else {
            self.notes.ai.error_message = Some("需要选择一个可用模型配置".to_string());
            context.notify();
            return;
        };
        let Some(document) = self.current_notes_ai_document_context() else {
            self.notes.ai.error_message = Some("请先打开并编辑一篇笔记".to_string());
            context.notify();
            return;
        };
        let prompt = self.notes.ai.input_text.trim().to_string();
        let request_messages =
            build_notes_ai_request_messages(&document, &self.notes.ai.messages, &prompt);

        let user_message = NotesAiMessage {
            id: new_ai_chat_entity_id("notes-ai-message"),
            role: NotesAiMessageRole::User,
            content: prompt,
            reasoning_content: String::new(),
            status: AiChatMessageStatus::Complete,
            error_message: None,
        };
        let assistant_message = NotesAiMessage {
            id: new_ai_chat_entity_id("notes-ai-message"),
            role: NotesAiMessageRole::Assistant,
            content: String::new(),
            reasoning_content: String::new(),
            status: AiChatMessageStatus::Streaming,
            error_message: None,
        };
        let assistant_message_id = assistant_message.id.clone();
        self.notes.ai.messages.push(user_message);
        self.notes.ai.messages.push(assistant_message);
        self.notes.ai.input_text.clear();
        self.notes.ai.input_selection_range = 0..0;
        self.notes.ai.input_marked_range = None;
        self.notes.ai.error_message = None;

        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_task = cancel.clone();
        let job_id = self.notes.ai.next_job_id;
        self.notes.ai.next_job_id = self.notes.ai.next_job_id.saturating_add(1);
        self.notes.ai.streaming_task = Some(NotesAiStreamingTask {
            job_id,
            assistant_message_id,
            receiver,
            cancel,
        });

        context
            .background_spawn(async move {
                stream_openai_compatible_chat_messages(
                    profile,
                    request_messages,
                    false,
                    AiChatReasoningEffort::High,
                    cancel_for_task,
                    sender,
                );
            })
            .detach();
        self.schedule_notes_ai_stream_poll(context);
        context.notify();
    }

    /// 停止当前笔记 AI 流式生成。
    pub(in crate::app) fn stop_notes_ai_streaming(&mut self, context: &mut Context<Self>) {
        self.stop_notes_ai_streaming_without_notify(AiChatMessageStatus::Stopped, None);
        context.notify();
    }

    /// 在不通知 UI 的情况下结束笔记 AI 流式任务。
    ///
    /// 业务意图：
    /// - 切换笔记、关闭侧边栏或退出编辑态时经常需要先同步清理任务，再由外层统一 `notify`，避免重复重绘。
    pub(in crate::app) fn stop_notes_ai_streaming_without_notify(
        &mut self,
        status: AiChatMessageStatus,
        error_message: Option<String>,
    ) {
        let Some(task) = self.notes.ai.streaming_task.take() else {
            return;
        };
        task.cancel.store(true, Ordering::Relaxed);
        self.finish_notes_ai_assistant_message(&task.assistant_message_id, status, error_message);
    }

    /// 安排 UI 线程轮询笔记 AI 后台流式事件。
    ///
    /// 实现原因：
    /// - GPUI 状态只能在实体更新闭包内修改；后台请求通过通道回传事件，再由该轮询把增量应用到侧边栏消息。
    pub(in crate::app) fn schedule_notes_ai_stream_poll(&self, context: &mut Context<Self>) {
        context
            .spawn(async move |view, app| {
                loop {
                    app.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                    let keep_polling = view
                        .update(app, |view, context| {
                            view.drain_notes_ai_stream_events(context);
                            view.notes.ai.streaming_task.is_some()
                        })
                        .unwrap_or(false);
                    if !keep_polling {
                        break;
                    }
                }
            })
            .detach();
    }

    /// 读取并应用笔记 AI 后台流式事件。
    pub(in crate::app) fn drain_notes_ai_stream_events(&mut self, context: &mut Context<Self>) {
        let mut received_event = false;
        while let Some(task) = self.notes.ai.streaming_task.as_ref() {
            let event = match task.receiver.try_recv() {
                Ok(event) => {
                    received_event = true;
                    event
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    received_event = true;
                    AiChatStreamEvent::Done
                }
            };
            self.apply_notes_ai_stream_event(event);
        }
        if received_event {
            context.notify();
        }
    }

    /// 应用单个笔记 AI 流式事件。
    pub(in crate::app) fn apply_notes_ai_stream_event(&mut self, event: AiChatStreamEvent) {
        let Some(task_snapshot) = self
            .notes
            .ai
            .streaming_task
            .as_ref()
            .map(|task| (task.assistant_message_id.clone(), task.job_id))
        else {
            return;
        };
        let (assistant_message_id, _job_id) = task_snapshot;
        match event {
            AiChatStreamEvent::Delta(delta) => {
                if let Some(message) = self
                    .notes
                    .ai
                    .messages
                    .iter_mut()
                    .find(|message| message.id == assistant_message_id)
                {
                    message.content.push_str(&delta);
                }
            }
            AiChatStreamEvent::ReasoningDelta(delta) => {
                if let Some(message) = self
                    .notes
                    .ai
                    .messages
                    .iter_mut()
                    .find(|message| message.id == assistant_message_id)
                {
                    message.reasoning_content.push_str(&delta);
                }
            }
            AiChatStreamEvent::Done => {
                self.finish_notes_ai_assistant_message(
                    &assistant_message_id,
                    AiChatMessageStatus::Complete,
                    None,
                );
                self.notes.ai.streaming_task = None;
            }
            AiChatStreamEvent::Stopped => {
                self.finish_notes_ai_assistant_message(
                    &assistant_message_id,
                    AiChatMessageStatus::Stopped,
                    None,
                );
                self.notes.ai.streaming_task = None;
            }
            AiChatStreamEvent::Error(message) => {
                self.finish_notes_ai_assistant_message(
                    &assistant_message_id,
                    AiChatMessageStatus::Failed,
                    Some(message.clone()),
                );
                self.notes.ai.error_message = Some(message);
                self.notes.ai.streaming_task = None;
            }
        }
    }

    /// 完成、停止或失败笔记 AI 助手消息。
    pub(in crate::app) fn finish_notes_ai_assistant_message(
        &mut self,
        message_id: &str,
        status: AiChatMessageStatus,
        error_message: Option<String>,
    ) {
        if let Some(message) = self
            .notes
            .ai
            .messages
            .iter_mut()
            .find(|message| message.id == message_id)
        {
            message.status = status;
            message.error_message = error_message;
        }
    }

    /// 将一条笔记 AI 回复插入当前富文本编辑器。
    ///
    /// 业务意图：
    /// - AI 回复先在侧边栏预览，只有用户明确点击插入才会修改当前编辑器草稿。
    /// - 插入使用点击时的当前选区；有选区则替换，无选区则插入光标处，之后仍由用户手动保存 Markdown 文件。
    pub(in crate::app) fn insert_notes_ai_message_into_editor(
        &mut self,
        message_id: String,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if !self.notes.is_editing || self.notes.active_note.is_none() {
            return;
        }
        let Some(content) = self
            .notes
            .ai
            .messages
            .iter()
            .find(|message| message.id == message_id)
            .map(|message| message.content.clone())
        else {
            return;
        };
        if content.trim().is_empty() {
            self.notes.ai.error_message = Some("AI 回复为空，无法插入".to_string());
            context.notify();
            return;
        }
        let document = NoteRichTextDocument::from_markdown(&content);
        self.notes
            .rich_editor
            .replace_selection_with_document(&document);
        self.notes.ai.error_message = None;
        window.focus(&self.notes.rich_editor.focus);
        context.notify();
    }

    /// 进入笔记编辑状态。
    pub(in crate::app) fn start_note_editing(
        &mut self,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(note) = self.notes.active_note.clone() else {
            return;
        };
        self.notes.editor_title = note.title.clone();
        let title_cursor = self.notes.editor_title.len();
        self.notes.title_selection_range = title_cursor..title_cursor;
        self.notes.title_marked_range = None;
        // 富文本 JSON 解析失败时不能进入编辑态，否则编辑器中的空文档会覆盖原始损坏内容。
        // 这里先完成正文加载，再打开编辑状态；失败时保留只读错误展示，等待用户处理原始数据。
        if !self.reset_note_rich_editor_from_note(&note, true, Some(window)) {
            self.notes.is_editing = false;
            self.reset_notes_ai_assistant_session();
            context.notify();
            return;
        }
        self.notes.is_editing = true;
        self.reset_notes_ai_assistant_session();
        context.notify();
    }

    /// 取消笔记编辑。
    pub(in crate::app) fn cancel_note_editing(&mut self, context: &mut Context<Self>) {
        if let Some(note) = self.notes.active_note.clone() {
            self.notes.editor_title = note.title.clone();
            let title_cursor = self.notes.editor_title.len();
            self.notes.title_selection_range = title_cursor..title_cursor;
            self.notes.title_marked_range = None;
            self.reset_note_rich_editor_from_note(&note, false, None);
        }
        self.notes.is_editing = false;
        self.reset_notes_ai_assistant_session();
        context.notify();
    }

    /// 保存当前笔记草稿。
    pub(in crate::app) fn save_active_note(&mut self, context: &mut Context<Self>) -> bool {
        // 保存只允许发生在显式编辑态；只读阅读器复用正文选区和焦点，不能因为快捷键或未来调用路径误触发写库。
        if !self.notes.is_editing {
            return false;
        }
        let Some(note) = self.notes.active_note.clone() else {
            return false;
        };
        let Some(path) = self.notes_root_dir_or_error() else {
            context.notify();
            return false;
        };
        let content = self.notes.rich_editor.markdown_content();
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
            &content,
            NoteContentFormat::Markdown,
        );
        match result {
            Ok(updated) => {
                self.notes.selected = Some(NotesTreeSelection {
                    id: updated.id.clone(),
                    kind: NoteTreeRowKind::Note,
                });
                self.notes.active_note = Some(updated);
                self.notes.is_editing = false;
                self.reset_notes_ai_assistant_session();
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

    /// 处理未保存确认选择。
    pub(in crate::app) fn resolve_notes_unsaved_dialog(
        &mut self,
        choice: NotesUnsavedChoice,
        window: &mut Window,
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
                    self.continue_notes_pending_action(dialog.action, window, context);
                }
            }
            NotesUnsavedChoice::Discard => {
                self.cancel_note_editing(context);
                self.continue_notes_pending_action(dialog.action, window, context);
            }
        }
    }

    /// 继续执行未保存确认后的动作。
    pub(in crate::app) fn continue_notes_pending_action(
        &mut self,
        action: NotesPendingAction,
        window: &mut Window,
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
                self.create_note_under(directory_id, Some(window), context)
            }
            NotesPendingAction::Delete(selection) => {
                self.open_note_delete_confirm(selection, context)
            }
            NotesPendingAction::Rename(selection) => {
                self.open_note_rename_dialog(selection, Some(window), context)
            }
            NotesPendingAction::Refresh => {
                self.reload_notes_tree();
                context.notify();
            }
        }
    }

    /// 开始拖拽调整笔记树宽度。
    ///
    /// 业务意图：
    /// - 用户可以像日志页分栏一样临时调整笔记树宽度，用于查看较长目录名或给右侧 A4 纸张留出更多空间。
    ///
    /// 边界条件：
    /// - 只记录拖拽起点和起始宽度，实际宽度在鼠标移动时统一经过边界约束。
    /// - 开始拖拽时关闭笔记树浮层菜单，避免拖动过程中菜单遮挡或消费鼠标释放事件。
    pub(in crate::app) fn start_notes_tree_resize(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        self.notes.tree_resize_drag = Some(NotesTreeResizeDrag {
            start_x: event.position.x,
            start_width: self.notes.tree_width,
        });
        self.notes.tree_context_menu = None;
        self.notes.tree_create_menu_open = false;
        context.notify();
    }

    /// 根据鼠标移动更新笔记树宽度。
    ///
    /// 边界条件：
    /// - 拖拽可能跨过右侧编辑区，因此移动事件由笔记页根节点接管。
    /// - 宽度同时受固定最小值、固定最大值和当前窗口内右侧工作区最小可用宽度约束。
    pub(in crate::app) fn update_notes_tree_resize_drag(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.notes.tree_resize_drag else {
            return;
        };
        if !event.dragging() {
            self.stop_notes_tree_resize_drag(context);
            return;
        }
        let delta = f32::from(event.position.x - drag.start_x);
        let window_width = f32::from(window.viewport_size().width);
        self.notes.tree_width =
            Self::clamp_notes_tree_width(drag.start_width + delta, window_width);
        context.notify();
    }

    /// 结束笔记树宽度拖拽。
    ///
    /// 边界条件：
    /// - 当前不做宽度持久化，因此释放鼠标只清理内存拖拽状态。
    pub(in crate::app) fn stop_notes_tree_resize_drag(&mut self, context: &mut Context<Self>) {
        if self.notes.tree_resize_drag.take().is_some() {
            context.notify();
        }
    }

    /// 处理笔记页鼠标移动事件。
    pub(in crate::app) fn handle_notes_page_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let was_resizing = self.notes.tree_resize_drag.is_some();
        self.update_notes_tree_resize_drag(event, window, context);
        if was_resizing {
            context.stop_propagation();
        }
    }

    /// 处理笔记页鼠标左键释放事件。
    pub(in crate::app) fn handle_notes_page_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let was_resizing = self.notes.tree_resize_drag.is_some();
        self.stop_notes_tree_resize_drag(context);
        if was_resizing {
            context.stop_propagation();
        }
    }

    /// 将笔记树宽度限制在当前窗口可用范围内。
    ///
    /// 业务意图：
    /// - 拖拽宽度必须保留笔记树本身的可读性，也必须保留右侧编辑区的基本操作空间。
    ///
    /// 边界条件：
    /// - 如果窗口极窄导致右侧工作区最小宽度无法满足，仍优先保证笔记树不低于最小可用宽度。
    /// - 非有限数通常来自异常测试输入或平台窗口尺寸不可用，直接回退到默认宽度，避免布局写入 NaN。
    pub(in crate::app) fn clamp_notes_tree_width(requested_width: f32, window_width: f32) -> f32 {
        if !requested_width.is_finite() {
            return NOTES_TREE_DEFAULT_WIDTH;
        }
        let feature_width = if window_width.is_finite() {
            (window_width - MAIN_NAV_WIDTH).max(0.0)
        } else {
            NOTES_TREE_DEFAULT_WIDTH + NOTES_WORKSPACE_MIN_WIDTH
        };
        let max_tree_width = (feature_width - NOTES_WORKSPACE_MIN_WIDTH)
            .max(NOTES_TREE_MIN_WIDTH)
            .min(NOTES_TREE_MAX_WIDTH);
        requested_width.clamp(NOTES_TREE_MIN_WIDTH, max_tree_width)
    }

    /// 计算笔记树右键菜单横坐标。
    ///
    /// 业务意图：
    /// - 笔记页和日志页一样位于固定主导航右侧，窗口坐标进入笔记树面板前必须扣除主导航宽度。
    pub(in crate::app) fn notes_tree_context_menu_x(window_x: f32, tree_width: f32) -> f32 {
        (window_x - MAIN_NAV_WIDTH)
            .max(0.0)
            .clamp(0.0, (tree_width - LOG_TREE_CONTEXT_MENU_WIDTH).max(0.0))
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
            x: Self::notes_tree_context_menu_x(f32::from(event.position.x), self.notes.tree_width),
            y: f32::from(event.position.y).max(0.0),
        });
        self.notes.rename_dialog = None;
        self.notes.delete_confirm_dialog = None;
        self.notes.tree_create_menu_open = false;
        context.notify();
        context.stop_propagation();
    }

    /// 关闭笔记树所有浮层菜单。
    ///
    /// 业务意图：
    /// - 右键菜单和顶部新增菜单共享同一层透明遮罩，外部点击时必须一起关闭，避免隐藏菜单残留状态。
    pub(in crate::app) fn close_notes_tree_menus(&mut self, context: &mut Context<Self>) {
        self.notes.tree_context_menu = None;
        self.notes.tree_create_menu_open = false;
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
                self.request_create_note(directory_id, Some(window), context);
            }
            NotesTreeContextMenuAction::Rename => {
                self.request_rename_note_tree_item(target, window, context);
            }
            NotesTreeContextMenuAction::Delete => {
                self.request_delete_note_tree_item(target, context);
            }
            NotesTreeContextMenuAction::Plugin {
                plugin_id,
                menu_id,
                command_id,
            } => {
                let title = self.note_tree_title_for_selection(&target);
                let plugin_target = PluginNoteTreeTarget {
                    id: target.id,
                    title,
                    kind: Self::plugin_note_tree_kind_label(target.kind).to_string(),
                };
                self.invoke_notes_tree_plugin_menu(
                    plugin_id,
                    menu_id,
                    command_id,
                    plugin_target,
                    context,
                );
            }
        }
        context.stop_propagation();
    }

    /// 返回插件协议中的笔记树节点类型标签。
    ///
    /// 业务意图：
    /// - 插件只接收稳定、可序列化的节点类型字符串，不依赖 Rust 内部枚举名称，便于第三方语言实现。
    fn plugin_note_tree_kind_label(kind: NoteTreeRowKind) -> &'static str {
        match kind {
            NoteTreeRowKind::Directory => "directory",
            NoteTreeRowKind::Note => "note",
        }
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

    /// 判断是否为保存快捷键。
    pub(in crate::app) fn is_save_keystroke(keystroke: &Keystroke) -> bool {
        Self::keystroke_matches_letter_or_control_code(keystroke, "s", "\u{13}")
            && (keystroke.modifiers.control || keystroke.modifiers.platform)
    }

    /// 处理正文富文本编辑器快捷键。
    ///
    /// 业务意图：
    /// - 富文本编辑器是项目内自绘控件，必须显式处理保存、复制、剪切、粘贴、撤销重做和基础导航。
    /// - 普通文本输入和中文 IME 仍走 `EntityInputHandler::replace_text_in_range`，这里不处理 printable 字符，避免截断组合输入。
    pub(in crate::app) fn handle_note_editor_container_key_down(
        &mut self,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_save_keystroke(&event.keystroke) {
            self.save_active_note(context);
            context.stop_propagation();
            return;
        }
        if Self::is_copy_keystroke(&event.keystroke) {
            self.copy_selected_note_rich_text(context);
            context.stop_propagation();
            return;
        }
        if Self::is_cut_keystroke(&event.keystroke) {
            self.cut_selected_note_rich_text(context);
            context.stop_propagation();
            return;
        }
        if Self::is_paste_keystroke(&event.keystroke) {
            self.paste_note_rich_text_from_clipboard(context);
            context.stop_propagation();
            return;
        }
        if Self::is_select_all_keystroke(&event.keystroke) {
            let len = self.notes.rich_editor.document.len();
            self.notes.rich_editor.set_selection(0..len);
            context.stop_propagation();
            context.notify();
            return;
        }
        if Self::is_undo_keystroke(&event.keystroke) {
            if self.notes.rich_editor.undo() {
                context.notify();
            }
            context.stop_propagation();
            return;
        }
        if Self::is_redo_keystroke(&event.keystroke) {
            if self.notes.rich_editor.redo() {
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        match event.keystroke.key.as_str() {
            "backspace" => self.notes.rich_editor.delete_backward(),
            "delete" => self.notes.rich_editor.delete_forward(),
            "enter" => self.notes.rich_editor.replace_range_with_text(None, "\n"),
            "tab" => self.notes.rich_editor.replace_range_with_text(None, "    "),
            "left" => {
                let text = self.notes.rich_editor.plain_text();
                let cursor = self.notes.rich_editor.clamped_selection().end;
                let target = text[..cursor]
                    .char_indices()
                    .next_back()
                    .map(|(index, _)| index)
                    .unwrap_or(0);
                self.notes
                    .rich_editor
                    .move_cursor(target, event.keystroke.modifiers.shift);
            }
            "right" => {
                let text = self.notes.rich_editor.plain_text();
                let cursor = self.notes.rich_editor.clamped_selection().end;
                let target = text[cursor..]
                    .char_indices()
                    .next()
                    .map(|(offset, character)| cursor + offset + character.len_utf8())
                    .unwrap_or(text.len());
                self.notes
                    .rich_editor
                    .move_cursor(target, event.keystroke.modifiers.shift);
            }
            "home" => {
                let cursor = self.notes.rich_editor.clamped_selection().end;
                let target =
                    Self::note_rich_text_line_start(&self.notes.rich_editor.plain_text(), cursor);
                self.notes
                    .rich_editor
                    .move_cursor(target, event.keystroke.modifiers.shift);
            }
            "end" => {
                let cursor = self.notes.rich_editor.clamped_selection().end;
                let target =
                    Self::note_rich_text_line_end(&self.notes.rich_editor.plain_text(), cursor);
                self.notes
                    .rich_editor
                    .move_cursor(target, event.keystroke.modifiers.shift);
            }
            "pageup" => self
                .notes
                .rich_editor
                .move_cursor(0, event.keystroke.modifiers.shift),
            "pagedown" => {
                let len = self.notes.rich_editor.document.len();
                self.notes
                    .rich_editor
                    .move_cursor(len, event.keystroke.modifiers.shift);
            }
            _ => return,
        }
        self.touch_search_text_cursor_activity();
        context.stop_propagation();
        context.notify();
    }

    /// 处理只读富文本阅读器快捷键。
    ///
    /// 业务意图：
    /// - 只读状态不注册平台文本输入处理器，但仍必须像普通阅读器一样支持 `Ctrl/Cmd+C` 复制选区。
    /// - `Ctrl/Cmd+A` 只修改阅读器选区，不进入编辑态也不写库，保证只读阅读不会产生持久化副作用。
    pub(in crate::app) fn handle_note_reader_key_down(
        &mut self,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_copy_keystroke(&event.keystroke) {
            self.copy_selected_note_rich_text(context);
            context.stop_propagation();
            return;
        }
        if Self::is_select_all_keystroke(&event.keystroke) {
            let len = self.notes.rich_editor.document.len();
            self.notes.rich_editor.set_selection(0..len);
            context.stop_propagation();
            context.notify();
        }
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

    /// 返回光标所在行起点。
    pub(in crate::app) fn note_rich_text_line_start(text: &str, cursor: usize) -> usize {
        let cursor = clamp_note_rich_text_boundary(text, cursor);
        text[..cursor]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    /// 返回光标所在行终点。
    pub(in crate::app) fn note_rich_text_line_end(text: &str, cursor: usize) -> usize {
        let cursor = clamp_note_rich_text_boundary(text, cursor);
        text[cursor..]
            .find('\n')
            .map(|offset| cursor + offset)
            .unwrap_or(text.len())
    }

    /// 开始富文本鼠标选择。
    pub(in crate::app) fn start_note_rich_text_selection(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.notes.is_editing
            && self
                .notes
                .rich_editor
                .start_code_language_selection(event.position)
        {
            window.focus(&self.notes.rich_editor.focus);
            context.notify();
            return;
        }
        // 代码块语言菜单现在绘制在 A4 纸张内部，没有全屏遮罩；当用户点击其它正文位置时，
        // 应立即关闭旧菜单，避免菜单悬停在新的编辑目标上方。
        if self.notes.rich_editor.code_language_menu_open {
            self.notes.rich_editor.code_language_menu_open = false;
            self.notes.rich_editor.code_language_menu_anchor = None;
        }
        if self
            .notes
            .rich_editor
            .start_code_block_scroll_drag(event.position)
        {
            context.notify();
            return;
        }
        if self.notes.is_editing
            && self
                .notes
                .rich_editor
                .place_cursor_after_code_block_at_point(event.position)
        {
            window.focus(&self.notes.rich_editor.focus);
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        let index = self.notes.rich_editor.index_for_point(event.position);
        window.focus(&self.notes.rich_editor.focus);
        match event.click_count {
            0 | 1 => self.notes.rich_editor.set_selection(index..index),
            2 => {
                let text = self.notes.rich_editor.plain_text();
                self.notes
                    .rich_editor
                    .set_selection(Self::search_text_word_range_for_index(&text, index));
            }
            _ => {
                let text = self.notes.rich_editor.plain_text();
                let start = Self::note_rich_text_line_start(&text, index);
                let end = Self::note_rich_text_line_end(&text, index);
                self.notes.rich_editor.set_selection(start..end);
            }
        }
        self.notes.rich_editor.drag_anchor = (event.click_count <= 1).then_some(index);
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 更新富文本鼠标拖选。
    pub(in crate::app) fn update_note_rich_text_selection(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        if self
            .notes
            .rich_editor
            .update_code_block_scroll_drag(event.position)
        {
            context.notify();
            return;
        }
        if !event.dragging() {
            self.finish_note_rich_text_selection(context);
            return;
        }
        let Some(anchor) = self.notes.rich_editor.drag_anchor else {
            return;
        };
        let focus = self.notes.rich_editor.index_for_point(event.position);
        self.notes.rich_editor.set_selection(anchor..focus);
        context.notify();
    }

    /// 完成富文本鼠标拖选。
    pub(in crate::app) fn finish_note_rich_text_selection(&mut self, context: &mut Context<Self>) {
        if self.notes.rich_editor.finish_code_block_scroll_drag() {
            context.notify();
            return;
        }
        if self.notes.rich_editor.drag_anchor.take().is_some() {
            context.notify();
        }
    }

    /// 复制当前富文本选区。
    pub(in crate::app) fn copy_selected_note_rich_text(&self, context: &mut Context<Self>) -> bool {
        let Some(text) = self.notes.rich_editor.selected_text() else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        let Some(document) = self.notes.rich_editor.selected_document() else {
            context.write_to_clipboard(ClipboardItem::new_string(text));
            return true;
        };
        let payload = NoteRichTextClipboardPayload::new(document);
        context.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(text, payload));
        true
    }

    /// 剪切当前富文本选区。
    pub(in crate::app) fn cut_selected_note_rich_text(
        &mut self,
        context: &mut Context<Self>,
    ) -> bool {
        if !self.copy_selected_note_rich_text(context) {
            return false;
        }
        self.notes.rich_editor.delete_backward();
        context.notify();
        true
    }

    /// 从剪贴板粘贴富文本或纯文本。
    pub(in crate::app) fn paste_note_rich_text_from_clipboard(
        &mut self,
        context: &mut Context<Self>,
    ) -> bool {
        let Some(item) = context.read_from_clipboard() else {
            return false;
        };
        if let Some(metadata) = item.metadata()
            && let Ok(payload) = serde_json::from_str::<NoteRichTextClipboardPayload>(metadata)
            && let Some(document) = payload.into_document()
        {
            self.notes
                .rich_editor
                .replace_selection_with_document(&document);
            context.notify();
            return true;
        }
        let Some(text) = item.text() else {
            return false;
        };
        self.notes.rich_editor.replace_range_with_text(None, &text);
        context.notify();
        true
    }

    /// 处理富文本代码块的横向滚轮。
    ///
    /// 业务意图：
    /// - 只在指针位于代码块且产生横向滚动时消费事件，普通纵向滚动继续交给外层 GPUI 滚动容器。
    /// - 这样编辑态和只读态都能滚动整篇笔记，同时代码块超宽内容不会把普通段落变成横向滚动区域。
    pub(in crate::app) fn handle_note_rich_text_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        context: &mut Context<Self>,
    ) -> bool {
        let pixel_delta = event.delta.pixel_delta(px(20.0));
        let scrolled = self.notes.rich_editor.scroll_code_block_at_point(
            event.position,
            pixel_delta,
            event.modifiers.shift,
        );
        if scrolled {
            context.notify();
        }
        scrolled
    }

    /// 对富文本当前选区设置字号。
    pub(in crate::app) fn apply_note_rich_text_font_size(
        &mut self,
        font_size_px: u32,
        context: &mut Context<Self>,
    ) {
        self.notes.rich_editor.font_size_menu_open = false;
        self.notes.rich_editor.color_menu_open = false;
        self.notes.rich_editor.background_color_menu_open = false;
        self.notes.rich_editor.code_language_menu_open = false;
        self.notes.rich_editor.code_language_menu_anchor = None;
        self.notes
            .rich_editor
            .apply_style_patch(NoteRichTextStylePatch {
                font_size_px: Some(font_size_px),
                ..Default::default()
            });
        context.notify();
    }

    /// 切换富文本内联样式。
    pub(in crate::app) fn toggle_note_rich_text_inline_style(
        &mut self,
        command: NoteRichTextInlineCommand,
        context: &mut Context<Self>,
    ) {
        self.notes.rich_editor.toggle_inline_command(command);
        context.notify();
    }

    /// 设置富文本文字颜色。
    pub(in crate::app) fn apply_note_rich_text_color(
        &mut self,
        color: Option<NoteRichTextColor>,
        context: &mut Context<Self>,
    ) {
        self.notes.rich_editor.color_menu_open = false;
        self.notes.rich_editor.font_size_menu_open = false;
        self.notes.rich_editor.background_color_menu_open = false;
        self.notes.rich_editor.code_language_menu_open = false;
        self.notes.rich_editor.code_language_menu_anchor = None;
        self.notes
            .rich_editor
            .apply_style_patch(NoteRichTextStylePatch {
                color: Some(color),
                ..Default::default()
            });
        context.notify();
    }

    /// 设置富文本文字背景颜色。
    pub(in crate::app) fn apply_note_rich_text_background_color(
        &mut self,
        color: Option<NoteRichTextColor>,
        context: &mut Context<Self>,
    ) {
        self.notes.rich_editor.background_color_menu_open = false;
        self.notes.rich_editor.font_size_menu_open = false;
        self.notes.rich_editor.color_menu_open = false;
        self.notes.rich_editor.code_language_menu_open = false;
        self.notes.rich_editor.code_language_menu_anchor = None;
        self.notes
            .rich_editor
            .apply_style_patch(NoteRichTextStylePatch {
                background_color: Some(color),
                ..Default::default()
            });
        context.notify();
    }

    /// 关闭富文本工具栏所有下拉菜单。
    pub(in crate::app) fn close_note_rich_text_menus(&mut self, context: &mut Context<Self>) {
        self.notes.rich_editor.font_size_menu_open = false;
        self.notes.rich_editor.color_menu_open = false;
        self.notes.rich_editor.background_color_menu_open = false;
        self.notes.rich_editor.code_language_menu_open = false;
        self.notes.rich_editor.code_language_menu_anchor = None;
        context.notify();
    }

    /// 切换富文本列表块类型。
    pub(in crate::app) fn toggle_note_rich_text_list(
        &mut self,
        kind: NoteRichTextBlockKind,
        context: &mut Context<Self>,
    ) {
        self.notes.rich_editor.toggle_list_kind(kind);
        context.notify();
    }

    /// 切换富文本代码块。
    pub(in crate::app) fn toggle_note_rich_text_code_block(&mut self, context: &mut Context<Self>) {
        self.notes.rich_editor.toggle_code_block();
        context.notify();
    }

    /// 设置当前代码块语言。
    pub(in crate::app) fn apply_note_rich_text_code_language(
        &mut self,
        language: &'static str,
        context: &mut Context<Self>,
    ) {
        self.notes.rich_editor.code_language_menu_open = false;
        self.notes.rich_editor.code_language_menu_anchor = None;
        self.notes.rich_editor.set_code_language(language);
        context.notify();
    }

    /// 根据只读源码行和横坐标生成文本位置。
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
    pub(in crate::app) fn finish_note_source_selection(&mut self, context: &mut Context<Self>) {
        if self.notes.source_selection_drag_anchor.take().is_some() {
            context.notify();
        }
    }

    /// 返回当前笔记源码选中文本。
    #[allow(dead_code)]
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
        self.copy_selected_note_rich_text(context)
    }

    /// 将笔记正文拆成源码阅读器行。
    #[allow(dead_code)]
    pub(in crate::app) fn note_source_lines(content: &str) -> Vec<&str> {
        if content.is_empty() {
            Vec::new()
        } else {
            content.split('\n').collect()
        }
    }

    /// 返回某一行被选择覆盖的 UTF-8 字节范围。
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证目录重命名会同步重写子路径前缀。
    ///
    /// 业务意图：
    /// - 物理笔记 ID 来自相对路径，父目录改名会改变所有子笔记 ID；当前打开笔记和选中节点不能继续保留旧路径。
    #[test]
    fn 目录重命名会重写子笔记路径前缀() {
        let mut note_id = "旧目录/子目录/笔记.md".to_string();
        assert!(MainView::rewrite_note_path_value_prefix(
            &mut note_id,
            "旧目录",
            "新目录"
        ));
        assert_eq!(note_id, "新目录/子目录/笔记.md");

        let mut directory_id = Some("旧目录/子目录".to_string());
        assert!(MainView::rewrite_optional_note_path_prefix(
            &mut directory_id,
            "旧目录",
            "新目录"
        ));
        assert_eq!(directory_id.as_deref(), Some("新目录/子目录"));

        let mut unrelated = "其它目录/笔记.md".to_string();
        assert!(!MainView::rewrite_note_path_value_prefix(
            &mut unrelated,
            "旧目录",
            "新目录"
        ));
        assert_eq!(unrelated, "其它目录/笔记.md");
    }
}
