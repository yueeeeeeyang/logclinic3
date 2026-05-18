// 笔记 GPUI 面板状态和纯状态辅助函数。
//
// 业务意图：
// - 该文件只承载笔记树、当前笔记、只读选区、编辑草稿、确认弹窗和输入布局等 UI 状态。
// - 类型可见性限制在 app 模块内，避免把第一版笔记内部状态暴露成 crate 级 API。

use std::{
    collections::HashSet,
    ops::Range,
    sync::{Arc, atomic::AtomicBool, mpsc},
};

use super::*;

/// 当前选中的笔记树节点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct NotesTreeSelection {
    /// 节点 ID。
    pub(in crate::app) id: String,
    /// 节点类型。
    pub(in crate::app) kind: NoteTreeRowKind,
}

/// 笔记文本位置。
///
/// 业务意图：
/// - 只读源码阅读器不是系统文本控件，因此需要轻量位置类型保存跨行选择范围。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::app) struct NoteTextPosition {
    /// 0 基行号。
    pub(in crate::app) line_index: usize,
    /// 0 基字符列。
    pub(in crate::app) column: usize,
}

/// 笔记源码只读选择范围。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct NoteTextSelection {
    /// 鼠标按下锚点。
    pub(in crate::app) anchor: NoteTextPosition,
    /// 当前拖拽焦点。
    pub(in crate::app) focus: NoteTextPosition,
}

impl NoteTextSelection {
    /// 返回按文档顺序排列后的端点。
    #[allow(dead_code)]
    pub(in crate::app) fn normalized(&self) -> (NoteTextPosition, NoteTextPosition) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// 判断选择是否为空。
    #[allow(dead_code)]
    pub(in crate::app) fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }
}

/// 未保存修改确认动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum NotesUnsavedChoice {
    /// 保存当前草稿后继续原动作。
    Save,
    /// 放弃当前草稿后继续原动作。
    Discard,
    /// 取消原动作并留在当前笔记。
    Cancel,
}

/// 触发未保存确认的后续动作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum NotesPendingAction {
    /// 切换到另一个树节点。
    Select(NotesTreeSelection),
    /// 切换到主导航其它功能页。
    SwitchFeature(MainFeature),
    /// 创建目录；`None` 表示根目录，`Some` 表示指定目录下的子目录。
    CreateDirectory(Option<String>),
    /// 创建笔记；`None` 表示根层笔记，`Some` 表示指定目录下的笔记。
    CreateNote(Option<String>),
    /// 删除指定树节点。
    Delete(NotesTreeSelection),
    /// 重命名指定树节点。
    Rename(NotesTreeSelection),
    /// 刷新物理笔记目录。
    Refresh,
}

/// 未保存修改确认弹窗状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct NotesUnsavedDialog {
    /// 用户确认后要继续执行的动作。
    pub(in crate::app) action: NotesPendingAction,
}

/// 删除确认弹窗状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct NotesDeleteConfirmDialog {
    /// 待删除节点。
    pub(in crate::app) target: NotesTreeSelection,
    /// 展示标题。
    pub(in crate::app) title: String,
}

/// 重命名弹窗状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct NotesRenameDialog {
    /// 待重命名节点。
    pub(in crate::app) target: NotesTreeSelection,
    /// 打开弹窗时的原始标题；用于判断用户是否真正修改，避免无意义写库。
    pub(in crate::app) original_title: String,
}

/// 笔记树右键菜单状态。
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct NotesTreeContextMenu {
    /// 右键落点对应的笔记树节点。
    pub(in crate::app) target: NotesTreeSelection,
    /// 菜单左上角相对笔记树面板的横坐标。
    pub(in crate::app) x: f32,
    /// 菜单左上角相对笔记树面板的纵坐标。
    pub(in crate::app) y: f32,
}

/// 笔记树宽度拖拽状态。
///
/// 业务意图：
/// - 用户按住笔记树右侧边界拖动时，需要记录按下时的窗口横坐标和起始宽度，确保宽度变化与鼠标位移线性一致。
/// - 状态只在当前主窗口生命周期内有效，不写入磁盘，避免临时排版调整影响下一次打开应用。
#[derive(Clone, Copy, Debug)]
pub(in crate::app) struct NotesTreeResizeDrag {
    /// 鼠标按下时的窗口横坐标。
    pub(in crate::app) start_x: Pixels,
    /// 鼠标按下时的笔记树宽度。
    pub(in crate::app) start_width: f32,
}

/// 笔记 AI 临时消息角色。
///
/// 业务意图：
/// - 笔记 AI 侧边栏只保存当前应用会话内的临时对话，不写入全局 AI 对话数据库。
/// - 这里仍显式区分用户和助手，便于构造 OpenAI 兼容上下文并在 UI 中使用不同样式展示。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum NotesAiMessageRole {
    /// 用户输入的生成或改写要求。
    User,
    /// 模型返回的 Markdown 回复。
    Assistant,
}

impl NotesAiMessageRole {
    /// 返回 OpenAI 兼容 Chat Completions 使用的协议角色。
    pub(in crate::app) fn as_chat_role(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// 发送给笔记 AI 的当前文档上下文。
///
/// 业务意图：
/// - AI 生成必须基于用户当前正在编辑的草稿，而不是上一次保存到磁盘的 Markdown 文件。
/// - 该结构只在发起请求时临时构造，不持久化，避免把笔记全文复制到其它长期存储中。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct NotesAiDocumentContext {
    /// 当前编辑器标题草稿。
    pub(in crate::app) title: String,
    /// 当前富文本编辑器导出的 Markdown 草稿。
    pub(in crate::app) markdown: String,
}

/// 笔记 AI 侧边栏中的一条临时消息。
///
/// 边界条件：
/// - `content` 为空且状态为 `Streaming` 时表示助手回复占位，UI 应展示生成中状态。
/// - `reasoning_content` 只保存模型服务显式返回的推理字段，不参与插入当前笔记。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct NotesAiMessage {
    /// 当前侧边栏会话内的消息 ID。
    pub(in crate::app) id: String,
    /// 消息角色。
    pub(in crate::app) role: NotesAiMessageRole,
    /// 用户输入或助手正式回复正文。
    pub(in crate::app) content: String,
    /// 模型服务显式返回的推理增量。
    pub(in crate::app) reasoning_content: String,
    /// 消息生命周期状态。
    pub(in crate::app) status: AiChatMessageStatus,
    /// 请求失败时的中文错误。
    pub(in crate::app) error_message: Option<String>,
}

/// 笔记 AI 输入区中的单行排版缓存。
///
/// 业务意图：
/// - 笔记 AI 输入框需要支持中文 IME、鼠标拖选和候选窗口定位，因此像全局 AI 输入区一样保存真实字形布局。
pub(in crate::app) struct NotesAiInputLineLayout {
    /// 当前可视行对应的原始文本 UTF-8 字节范围。
    pub(in crate::app) byte_range: Range<usize>,
    /// 当前行的 GPUI 字形布局。
    pub(in crate::app) line: ShapedLine,
    /// 当前行在窗口中的绘制边界。
    pub(in crate::app) bounds: Bounds<Pixels>,
}

/// 笔记 AI 正在运行的流式任务。
///
/// 业务意图：
/// - 后台 HTTP/SSE 读取不能直接修改 GPUI 状态；任务通过通道把增量事件交回主线程轮询。
/// - 取消标记用于用户点击停止、切换笔记或关闭侧边栏时尽快让后台请求退出。
pub(in crate::app) struct NotesAiStreamingTask {
    /// 当前任务 ID，用于未来丢弃过期事件。
    pub(in crate::app) job_id: usize,
    /// 正在写入的助手消息 ID。
    pub(in crate::app) assistant_message_id: String,
    /// 后台线程发来的流式事件。
    pub(in crate::app) receiver: mpsc::Receiver<AiChatStreamEvent>,
    /// 停止生成时置位。
    pub(in crate::app) cancel: Arc<AtomicBool>,
}

/// 笔记 AI 侧边栏状态。
///
/// 业务意图：
/// - 该状态只服务当前编辑器会话，不持久化，不写入 `ai-chat.db`，避免把用户笔记全文作为历史上下文长期保存。
/// - 每次发送都会重新读取当前编辑器草稿导出的 Markdown，确保模型看到的是用户当前未保存内容。
pub(in crate::app) struct NotesAiAssistantState {
    /// 侧边栏是否打开。
    pub(in crate::app) is_open: bool,
    /// 当前临时对话消息列表。
    pub(in crate::app) messages: Vec<NotesAiMessage>,
    /// 当前选中的模型配置 ID；为空时发送前回退到默认模型。
    pub(in crate::app) selected_model_profile_id: Option<String>,
    /// 模型选择菜单是否展开。
    pub(in crate::app) model_menu_open: bool,
    /// 输入框文本。
    pub(in crate::app) input_text: String,
    /// 输入框选择范围。
    pub(in crate::app) input_selection_range: Range<usize>,
    /// 输入法组合文本范围。
    pub(in crate::app) input_marked_range: Option<Range<usize>>,
    /// 输入框焦点句柄。
    pub(in crate::app) input_focus: gpui::FocusHandle,
    /// 输入框最近一次绘制的逐行布局。
    pub(in crate::app) input_last_layouts: Vec<NotesAiInputLineLayout>,
    /// 输入框最近一次整体绘制边界。
    pub(in crate::app) input_last_bounds: Option<Bounds<Pixels>>,
    /// 输入框拖拽选择锚点。
    pub(in crate::app) input_selection_drag: Option<usize>,
    /// 当前正在进行的流式任务。
    pub(in crate::app) streaming_task: Option<NotesAiStreamingTask>,
    /// 下一个流式任务 ID。
    pub(in crate::app) next_job_id: usize,
    /// 侧边栏顶部展示的错误。
    pub(in crate::app) error_message: Option<String>,
}

impl NotesAiAssistantState {
    /// 创建笔记 AI 初始状态。
    ///
    /// 边界条件：
    /// - 焦点句柄必须在 `Context<MainView>` 中创建，确保独立的自绘输入元素可以接入 GPUI 平台输入协议。
    pub(in crate::app) fn new(context: &mut Context<MainView>) -> Self {
        Self {
            is_open: false,
            messages: Vec::new(),
            selected_model_profile_id: None,
            model_menu_open: false,
            input_text: String::new(),
            input_selection_range: 0..0,
            input_marked_range: None,
            input_focus: context.focus_handle(),
            input_last_layouts: Vec::new(),
            input_last_bounds: None,
            input_selection_drag: None,
            streaming_task: None,
            next_job_id: 1,
            error_message: None,
        }
    }

    /// 清空当前临时会话和输入状态。
    ///
    /// 业务意图：
    /// - 切换笔记、退出编辑或删除当前笔记时必须丢弃旧上下文，避免后续请求把另一篇笔记的内容带给模型。
    pub(in crate::app) fn reset_session(&mut self) {
        if let Some(task) = self.streaming_task.take() {
            task.cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.messages.clear();
        self.model_menu_open = false;
        self.input_text.clear();
        self.input_selection_range = 0..0;
        self.input_marked_range = None;
        self.input_last_layouts.clear();
        self.input_last_bounds = None;
        self.input_selection_drag = None;
        self.error_message = None;
    }
}

/// 构造笔记 AI 的 OpenAI 兼容消息列表。
///
/// 业务意图：
/// - 每次发送前都显式注入当前文档标题和 Markdown 全文，保证模型按照最新草稿生成内容。
/// - 临时历史只使用已完成、已停止且有正文的消息；失败或正在生成的助手消息不应作为上下文再次发送。
pub(in crate::app) fn build_notes_ai_request_messages(
    document: &NotesAiDocumentContext,
    history: &[NotesAiMessage],
    prompt: &str,
) -> Vec<OpenAiCompatibleChatMessage> {
    let mut messages = vec![
        OpenAiCompatibleChatMessage::new(
            "system",
            "你是 LogClinic 的笔记编辑助手。你只能根据用户给出的当前笔记内容和要求生成可直接插入 Markdown 笔记的内容。回复正文使用 Markdown，不要输出与插入内容无关的解释。",
        ),
        OpenAiCompatibleChatMessage::new(
            "user",
            format!(
                "当前笔记标题：\n{}\n\n当前笔记 Markdown 全文：\n```markdown\n{}\n```",
                document.title, document.markdown
            ),
        ),
    ];
    messages.extend(history.iter().filter_map(|message| {
        if message.content.trim().is_empty()
            || matches!(
                message.status,
                AiChatMessageStatus::Failed | AiChatMessageStatus::Streaming
            )
        {
            return None;
        }
        Some(OpenAiCompatibleChatMessage::new(
            message.role.as_chat_role(),
            message.content.clone(),
        ))
    }));
    messages.push(OpenAiCompatibleChatMessage::new(
        "user",
        prompt.trim().to_string(),
    ));
    messages
}

/// 笔记树右键菜单命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum NotesTreeContextMenuAction {
    /// 在目录下创建子目录。
    NewDirectory,
    /// 在目录下创建笔记。
    NewNote,
    /// 重命名目录或笔记。
    Rename,
    /// 删除目录或笔记。
    Delete,
    /// 调用插件贡献的笔记树右键菜单命令。
    ///
    /// 业务意图：
    /// - 插件菜单项来自 JSON manifest，运行时才能确定插件 ID、菜单 ID 和命令 ID。
    /// - v1 只向插件发送右键节点元数据，不开放笔记正文读写，避免第三方插件越过当前笔记权限边界。
    Plugin {
        /// 插件 ID。
        plugin_id: String,
        /// manifest 中的菜单贡献点 ID。
        menu_id: String,
        /// 传给插件进程的命令 ID。
        command_id: String,
    },
}

/// 笔记页面完整工作区状态。
pub(in crate::app) struct NotesWorkspaceState {
    /// 完整笔记树行。
    pub(in crate::app) tree_rows: Vec<NoteTreeRow>,
    /// 当前按展开状态可见的笔记树行。
    pub(in crate::app) visible_rows: Vec<NoteTreeRow>,
    /// 展开的目录 ID 集合。
    pub(in crate::app) expanded_directory_ids: HashSet<String>,
    /// 当前选中节点。
    pub(in crate::app) selected: Option<NotesTreeSelection>,
    /// 当前右侧打开的笔记。
    pub(in crate::app) active_note: Option<Note>,
    /// 笔记树当前宽度。
    ///
    /// 业务意图：
    /// - 默认值和 AI 对话左侧栏一致，拖拽后在当前会话内即时生效。
    /// - 宽度不持久化，避免用户一次临时查看长目录名后影响后续启动的标准布局。
    pub(in crate::app) tree_width: f32,
    /// 笔记树宽度拖拽状态。
    ///
    /// 边界条件：
    /// - `None` 表示普通鼠标移动不会改变布局；只有从右侧拖拽命中区按下后才进入拖拽模式。
    pub(in crate::app) tree_resize_drag: Option<NotesTreeResizeDrag>,
    /// 笔记树虚拟列表滚动句柄。
    pub(in crate::app) tree_scroll_handle: UniformListScrollHandle,
    /// 笔记存储错误。
    pub(in crate::app) database_error: Option<String>,
    /// 只读源码选择范围。
    pub(in crate::app) source_selection: Option<NoteTextSelection>,
    /// 只读源码拖拽锚点。
    pub(in crate::app) source_selection_drag_anchor: Option<NoteTextPosition>,
    /// 当前是否处于编辑状态。
    pub(in crate::app) is_editing: bool,
    /// 富文本正文编辑器状态。
    ///
    /// 业务意图：
    /// - 该状态保存正文文档、线性选区、IME 组合区、待输入样式、撤销重做和排版缓存。
    /// - 数据库仍只在保存按钮或未保存确认“保存”路径写入，避免编辑器输入过程产生隐式持久化。
    pub(in crate::app) rich_editor: RichTextEditorState,
    /// 编辑器标题草稿；第一版标题保存时同步重命名笔记。
    pub(in crate::app) editor_title: String,
    /// 标题输入框选择范围。
    pub(in crate::app) title_selection_range: Range<usize>,
    /// 标题输入框输入法组合文本范围。
    pub(in crate::app) title_marked_range: Option<Range<usize>>,
    /// 标题输入框焦点句柄。
    pub(in crate::app) title_focus: gpui::FocusHandle,
    /// 标题输入框最近一次单行排版结果。
    pub(in crate::app) title_last_layout: Option<ShapedLine>,
    /// 标题输入框最近一次绘制边界。
    pub(in crate::app) title_last_bounds: Option<Bounds<Pixels>>,
    /// 未保存修改确认弹窗。
    pub(in crate::app) unsaved_dialog: Option<NotesUnsavedDialog>,
    /// 重命名弹窗。
    pub(in crate::app) rename_dialog: Option<NotesRenameDialog>,
    /// 笔记树右键菜单。
    pub(in crate::app) tree_context_menu: Option<NotesTreeContextMenu>,
    /// 笔记树顶部新增菜单是否打开。
    ///
    /// 业务意图：
    /// - 左侧工具栏只保留一个“新增”入口，点击后再选择新建目录或新建笔记，降低工具栏图标密度。
    /// - 菜单是临时 UI 状态，不能影响树选择、编辑草稿和持久化数据。
    pub(in crate::app) tree_create_menu_open: bool,
    /// 删除确认弹窗。
    pub(in crate::app) delete_confirm_dialog: Option<NotesDeleteConfirmDialog>,
    /// 笔记编辑器内嵌 AI 侧边栏状态。
    ///
    /// 业务意图：
    /// - AI 侧边栏绑定当前编辑中的笔记，发送前读取当前草稿 Markdown，回复由用户显式插入当前编辑器。
    /// - 状态不持久化，也不写入全局 AI 对话历史，避免扩大笔记内容的长期存储范围。
    pub(in crate::app) ai: NotesAiAssistantState,
}

impl NotesWorkspaceState {
    /// 从物理 Markdown 目录创建笔记工作区状态。
    pub(in crate::app) fn load_or_initialize(context: &mut Context<MainView>) -> Self {
        let (tree_rows, database_error) = if let Some(path) = notes_root_dir() {
            match load_note_tree(&path) {
                Ok(rows) => (rows, None),
                Err(error) => (Vec::new(), Some(error)),
            }
        } else {
            (
                Vec::new(),
                Some("当前平台没有可用的应用配置目录，无法保存笔记文件".to_string()),
            )
        };
        let mut state = Self {
            tree_rows,
            visible_rows: Vec::new(),
            expanded_directory_ids: HashSet::new(),
            selected: None,
            active_note: None,
            tree_width: NOTES_TREE_DEFAULT_WIDTH,
            tree_resize_drag: None,
            tree_scroll_handle: UniformListScrollHandle::new(),
            database_error,
            source_selection: None,
            source_selection_drag_anchor: None,
            is_editing: false,
            rich_editor: RichTextEditorState::new(context),
            editor_title: String::new(),
            title_selection_range: 0..0,
            title_marked_range: None,
            title_focus: context.focus_handle(),
            title_last_layout: None,
            title_last_bounds: None,
            unsaved_dialog: None,
            rename_dialog: None,
            tree_context_menu: None,
            tree_create_menu_open: false,
            delete_confirm_dialog: None,
            ai: NotesAiAssistantState::new(context),
        };
        state.initialize_expanded_directories();
        state.rebuild_visible_rows();
        state
    }

    /// 初始化默认展开目录。
    fn initialize_expanded_directories(&mut self) {
        self.expanded_directory_ids = self
            .tree_rows
            .iter()
            .filter(|row| {
                row.kind == NoteTreeRowKind::Directory
                    && row.has_children
                    && row.depth < NOTES_TREE_DEFAULT_EXPANDED_DEPTH
            })
            .map(|row| row.id.clone())
            .collect();
    }

    /// 按展开状态重建可见行。
    pub(in crate::app) fn rebuild_visible_rows(&mut self) {
        self.visible_rows.clear();
        let mut collapsed_depth: Option<usize> = None;
        for row in &self.tree_rows {
            if let Some(depth) = collapsed_depth {
                if row.depth > depth {
                    continue;
                }
                collapsed_depth = None;
            }
            self.visible_rows.push(row.clone());
            if row.kind == NoteTreeRowKind::Directory
                && row.has_children
                && !self.expanded_directory_ids.contains(&row.id)
            {
                collapsed_depth = Some(row.depth);
            }
        }
    }

    /// 判断当前编辑草稿是否存在未保存修改。
    pub(in crate::app) fn has_unsaved_changes(&self) -> bool {
        let Some(note) = &self.active_note else {
            return false;
        };
        let serialized = self.rich_editor.markdown_content();
        Self::note_editor_has_unsaved_changes(
            self.is_editing,
            &self.editor_title,
            &serialized,
            note,
        )
    }

    /// 判断给定笔记草稿是否需要保存。
    ///
    /// 业务意图：
    /// - 物理文件存储以 Markdown 为真实内容；编辑器内部富文本需要先导出 Markdown，再与文件原文比较。
    /// - 旧富文本或纯文本只会来自迁移前数据，进入编辑态后保存会规范化为 Markdown。
    /// - 抽成纯函数便于测试，不需要启动真实 GPUI 窗口或构造输入控件实体。
    pub(in crate::app) fn note_editor_has_unsaved_changes(
        is_editing: bool,
        editor_title: &str,
        serialized_markdown: &str,
        note: &Note,
    ) -> bool {
        is_editing
            && (serialized_markdown != note.content
                || editor_title != note.title
                || note.content_format != NoteContentFormat::Markdown)
    }
}
