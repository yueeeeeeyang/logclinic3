// AI 对话 GPUI 面板状态和纯状态辅助函数。
//
// 业务意图：
// - 该文件只承载会话、消息、滚动、输入、流式任务等 AI 对话面板 UI 状态，以及不触碰 SQLite/网络的纯逻辑。
// - 类型可见性限制在 app 模块内，避免把第一版 AI 对话内部状态暴露成 crate 级 API。

use std::ops::Range;

use super::*;

/// AI 对话页面中的可滚动区域。
///
/// 业务意图：
/// - 左侧历史栏和右侧消息流都使用虚拟列表和自绘滚动条；该枚举让拖动生命周期可以复用同一套计算逻辑。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum AiChatScrollArea {
    /// 左侧历史会话列表。
    Conversations,
    /// 右侧当前会话消息流。
    Messages,
}

/// AI 对话自绘滚动条拖动状态。
///
/// 业务意图：
/// - 用户拖动滚动条时需要保存鼠标在滑块内部的相对位置，避免按下瞬间滑块跳到鼠标中心。
#[derive(Clone, Copy, Debug)]
pub(in crate::app) struct AiChatScrollbarDrag {
    /// 正在拖动的滚动区域。
    pub(in crate::app) area: AiChatScrollArea,
    /// 鼠标按下点到滑块顶部的距离。
    pub(in crate::app) cursor_offset: Pixels,
}

/// AI 对话输入区高度拖拽状态。
///
/// 业务意图：
/// - 用户需要根据提示词长度临时放大或缩小输入区；拖动时记录起点和起始高度，保证高度变化与鼠标位移线性一致。
/// - 高度只保存在当前主视图状态中，不写入配置文件，避免一个临时输入场景影响后续启动默认布局。
#[derive(Clone, Copy, Debug)]
pub(in crate::app) struct AiChatInputResizeDrag {
    /// 鼠标按下时的窗口纵坐标。
    pub(in crate::app) start_y: Pixels,
    /// 鼠标按下时的输入区高度。
    pub(in crate::app) start_height: f32,
}

/// AI 对话输入区中的单行排版缓存。
///
/// 业务意图：
/// - 多行输入需要支持鼠标点击、拖拽选择和 IME 候选窗口定位，必须保存每一行的真实字形布局。
pub(in crate::app) struct AiChatInputLineLayout {
    /// 当前可视行对应的原始文本 UTF-8 字节范围。
    pub(in crate::app) byte_range: Range<usize>,
    /// 当前行的 GPUI 字形布局。
    pub(in crate::app) line: ShapedLine,
    /// 当前行在窗口中的绘制边界。
    pub(in crate::app) bounds: Bounds<Pixels>,
}

/// AI 对话页面完整工作区状态。
///
/// 业务意图：
/// - AI 对话状态原本散落在 `MainView` 顶层字段中，导致根视图构造函数同时理解 SQLite、虚拟列表、输入框和流式任务。
/// - 该容器把会话、消息、输入、多列表滚动、数据库错误和后台任务集中到 AI 面板 UI 状态，主视图只负责组合工作区。
///
/// 边界条件：
/// - 构造函数会读取原有 AI 对话数据库并在空库时创建默认会话，数据库路径、错误文案和默认模型选择保持不变。
/// - 所有字段仍限制在 `app` 内部访问，不新增公开 API，也不改变用户可见配置或持久化 schema。
pub(in crate::app) struct AiChatWorkspaceState {
    /// AI 对话会话列表。
    pub(in crate::app) conversations: Vec<AiChatConversation>,
    /// 当前 AI 对话会话 ID。
    pub(in crate::app) active_conversation_id: Option<String>,
    /// 当前 AI 对话消息列表。
    pub(in crate::app) messages: Vec<AiChatMessage>,
    /// AI 对话左侧历史会话虚拟列表状态。
    pub(in crate::app) conversation_list_state: ListState,
    /// AI 对话右侧消息流虚拟列表状态。
    pub(in crate::app) message_list_state: ListState,
    /// AI 对话列表滚动条拖动状态。
    pub(in crate::app) scrollbar_drag: Option<AiChatScrollbarDrag>,
    /// AI 对话数据库错误。
    pub(in crate::app) database_error: Option<String>,
    /// AI 对话模型选择下拉菜单是否展开。
    pub(in crate::app) model_menu_open: bool,
    /// AI 对话输入框文本。
    pub(in crate::app) input_text: String,
    /// AI 对话输入框选择范围。
    pub(in crate::app) input_selection_range: Range<usize>,
    /// AI 对话输入框输入法组合文本范围。
    pub(in crate::app) input_marked_range: Option<Range<usize>>,
    /// AI 对话输入框焦点句柄。
    pub(in crate::app) input_focus: gpui::FocusHandle,
    /// AI 对话输入框最近一次绘制的逐行布局。
    pub(in crate::app) input_last_layouts: Vec<AiChatInputLineLayout>,
    /// AI 对话输入框最近一次整体绘制边界。
    pub(in crate::app) input_last_bounds: Option<Bounds<Pixels>>,
    /// AI 对话输入框拖拽选择锚点。
    pub(in crate::app) input_selection_drag: Option<usize>,
    /// AI 对话输入区当前高度。
    pub(in crate::app) input_height: f32,
    /// AI 对话输入区高度拖拽状态。
    pub(in crate::app) input_resize_drag: Option<AiChatInputResizeDrag>,
    /// 当前正在进行的 AI 流式任务。
    pub(in crate::app) streaming_task: Option<AiChatStreamingTask>,
    /// 下一个 AI 流式任务 ID。
    pub(in crate::app) next_job_id: usize,
}

impl AiChatWorkspaceState {
    /// 从持久化数据库和 UI 上下文创建 AI 对话工作区状态。
    ///
    /// 业务意图：
    /// - 主视图构造只需要传入默认模型配置 ID，AI 面板状态自己处理会话加载、空库初始化和输入框焦点创建。
    /// - 数据库不可用时只记录 AI 页错误，不影响日志查看主流程启动。
    pub(in crate::app) fn load_or_initialize(
        context: &mut Context<MainView>,
        default_model_profile_id: Option<String>,
    ) -> Self {
        let (conversations, active_conversation_id, messages, database_error) = if let Some(path) =
            ai_chat_database_path()
        {
            match load_ai_chat_conversations(&path) {
                Ok(mut conversations) => {
                    if conversations.is_empty() {
                        let conversation =
                            new_ai_chat_conversation(default_model_profile_id.clone());
                        match insert_ai_chat_conversation(&path, &conversation) {
                            Ok(()) => {
                                let active_id = Some(conversation.id.clone());
                                conversations.push(conversation);
                                (conversations, active_id, Vec::new(), None)
                            }
                            Err(error) => (Vec::new(), None, Vec::new(), Some(error)),
                        }
                    } else {
                        let active_id = conversations
                            .first()
                            .map(|conversation| conversation.id.clone());
                        let messages = active_id
                            .as_deref()
                            .map(|conversation_id| load_ai_chat_messages(&path, conversation_id))
                            .transpose();
                        match messages {
                            Ok(messages) => {
                                (conversations, active_id, messages.unwrap_or_default(), None)
                            }
                            Err(error) => (conversations, active_id, Vec::new(), Some(error)),
                        }
                    }
                }
                Err(error) => (Vec::new(), None, Vec::new(), Some(error)),
            }
        } else {
            (
                Vec::new(),
                None,
                Vec::new(),
                Some("当前平台没有可用的应用配置目录，无法保存 AI 对话历史".to_string()),
            )
        };

        Self {
            conversation_list_state: ListState::new(
                conversations.len(),
                ListAlignment::Top,
                px(AI_CHAT_VIRTUAL_LIST_OVERDRAW),
            ),
            message_list_state: ListState::new(
                messages.len(),
                ListAlignment::Bottom,
                px(AI_CHAT_VIRTUAL_LIST_OVERDRAW),
            ),
            conversations,
            active_conversation_id,
            messages,
            scrollbar_drag: None,
            database_error,
            model_menu_open: false,
            input_text: String::new(),
            input_selection_range: 0..0,
            input_marked_range: None,
            input_focus: context.focus_handle(),
            input_last_layouts: Vec::new(),
            input_last_bounds: None,
            input_selection_drag: None,
            input_height: AI_CHAT_INPUT_DEFAULT_HEIGHT,
            input_resize_drag: None,
            streaming_task: None,
            next_job_id: 1,
        }
    }
}

/// AI 对话输入区绘制状态。
pub(in crate::app) struct AiChatInputPrepaint {
    /// 当前帧需要绘制的所有文本行。
    pub(in crate::app) lines: Vec<AiChatInputPaintLine>,
    /// 当前选择范围对应的高亮矩形。
    pub(in crate::app) selections: Vec<PaintQuad>,
    /// 当前光标矩形。
    pub(in crate::app) cursor: Option<PaintQuad>,
}

/// AI 对话输入区单行绘制数据。
pub(in crate::app) struct AiChatInputPaintLine {
    /// 当前行对应的原始文本范围。
    pub(in crate::app) byte_range: Range<usize>,
    /// 当前行边界。
    pub(in crate::app) bounds: Bounds<Pixels>,
    /// 已排版的文本行。
    pub(in crate::app) line: ShapedLine,
}

/// 约束 AI 对话输入区拖拽后的高度。
///
/// 业务意图：
/// - 输入区高度要允许用户临时放大长提示词编辑空间，但不能遮挡发送控件或吞掉主要消息阅读区域。
///
/// 边界条件：
/// - 视口高度异常、极小或非有限时回退到绝对上限，避免平台窗口系统返回临时无效尺寸导致高度计算出 NaN。
pub(in crate::app) fn clamp_ai_chat_input_height(
    requested_height: f32,
    viewport_height: f32,
) -> f32 {
    let viewport_cap = if viewport_height.is_finite() && viewport_height > 0.0 {
        (viewport_height * AI_CHAT_INPUT_MAX_VIEWPORT_RATIO).max(AI_CHAT_INPUT_MIN_HEIGHT)
    } else {
        AI_CHAT_INPUT_MAX_HEIGHT
    };
    let max_height = AI_CHAT_INPUT_MAX_HEIGHT
        .min(viewport_cap)
        .max(AI_CHAT_INPUT_MIN_HEIGHT);
    requested_height.clamp(AI_CHAT_INPUT_MIN_HEIGHT, max_height)
}
