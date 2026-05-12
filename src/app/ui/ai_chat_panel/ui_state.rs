// AI 对话 GPUI 面板状态和纯状态辅助函数。
//
// 业务意图：
// - 该文件只承载会话、消息、滚动、输入、流式任务等 AI 对话面板 UI 状态，以及不触碰 SQLite/网络的纯逻辑。
// - 类型可见性限制在 app 模块内，避免把第一版 AI 对话内部状态暴露成 crate 级 API。

use std::{cell::RefCell, collections::HashMap, ops::Range, sync::mpsc};

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

/// AI 对话历史数据加载状态。
///
/// 业务意图：
/// - AI 页第一次进入时要先完成页面切换，再在后台读取 SQLite 历史数据，避免数据库打开、schema 初始化和历史消息读取阻塞导航点击。
/// - 状态机明确区分尚未加载、加载中和已加载，避免用户反复点击导航时重复启动后台任务。
///
/// 边界条件：
/// - `Loading` 内保存后台线程回传结果的接收器，只能在 UI 线程轮询读取，不能在渲染阶段阻塞等待。
pub(in crate::app) enum AiChatLoadState {
    /// 尚未请求加载历史数据。
    NotStarted,
    /// 后台任务正在读取或初始化 AI 对话数据库。
    Loading {
        /// 后台任务回传的加载结果。
        receiver: mpsc::Receiver<AiChatLoadResult>,
    },
    /// 历史数据已经完成加载；即使加载失败，也会进入该状态并通过 `database_error` 展示错误。
    Loaded,
}

impl AiChatLoadState {
    /// 判断 AI 历史数据是否已经进入后台加载中。
    ///
    /// 业务意图：
    /// - 渲染层需要据此显示加载动画，交互层需要据此禁用依赖会话 ID 的操作。
    pub(in crate::app) fn is_loading(&self) -> bool {
        matches!(self, Self::Loading { .. })
    }

    /// 判断 AI 历史数据是否已经完成首次加载。
    ///
    /// 边界条件：
    /// - 加载失败也算完成，因为错误会展示在页面中，后续新建或发送仍由数据库路径检查兜底。
    pub(in crate::app) fn is_loaded(&self) -> bool {
        matches!(self, Self::Loaded)
    }
}

/// AI 对话后台加载结果。
///
/// 业务意图：
/// - 后台线程不能直接修改 GPUI 状态，因此把会话列表、当前会话和消息列表打包回 UI 线程统一应用。
pub(in crate::app) struct AiChatLoadResult {
    /// AI 对话会话列表。
    pub(in crate::app) conversations: Vec<AiChatConversation>,
    /// 当前激活的 AI 会话 ID。
    pub(in crate::app) active_conversation_id: Option<String>,
    /// 当前激活会话的消息列表。
    pub(in crate::app) messages: Vec<AiChatMessage>,
    /// 数据库初始化或读取错误。
    pub(in crate::app) database_error: Option<String>,
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
    /// AI 历史数据加载状态。
    ///
    /// 业务意图：
    /// - 主窗口启动时不再同步读取 `ai-chat.db`，第一次进入 AI 页后先渲染页面骨架，再通过该状态驱动后台加载和加载动画。
    pub(in crate::app) load_state: AiChatLoadState,
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
    /// 助手 Markdown 渲染缓存。
    ///
    /// 业务意图：
    /// - AI 回复可能包含较长 Markdown 和代码块语法高亮；缓存解析结果可以避免滚动虚拟列表时反复解析同一条历史消息。
    /// - 缓存只保存展示结构，不写入数据库；键按消息 ID 管理，内容哈希或主题变化时会自动替换。
    pub(in crate::app) markdown_cache: RefCell<HashMap<String, AppMarkdownCacheEntry>>,
}

impl AiChatWorkspaceState {
    /// 创建尚未加载历史数据的 AI 对话工作区状态。
    ///
    /// 业务意图：
    /// - 主窗口默认启动在日志分析页，AI 历史数据库不应在启动或首次导航点击路径上同步读取。
    /// - 输入框焦点等轻量 UI 资源仍在这里初始化，保证页面骨架可以立刻渲染。
    pub(in crate::app) fn new_unloaded(context: &mut Context<MainView>) -> Self {
        Self {
            load_state: AiChatLoadState::NotStarted,
            conversation_list_state: ListState::new(
                0,
                ListAlignment::Top,
                px(AI_CHAT_VIRTUAL_LIST_OVERDRAW),
            ),
            message_list_state: ai_chat_message_list_state(0),
            conversations: Vec::new(),
            active_conversation_id: None,
            messages: Vec::new(),
            scrollbar_drag: None,
            database_error: None,
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
            markdown_cache: RefCell::new(HashMap::new()),
        }
    }
}

/// 在后台读取或初始化 AI 对话历史数据。
///
/// 业务意图：
/// - 该函数不依赖 GPUI 上下文，便于放到后台执行器中运行，避免 SQLite I/O 和 schema 初始化阻塞页面切换。
/// - 空数据库仍按旧行为创建默认会话，保证加载完成后用户可以直接输入并发送。
pub(in crate::app) fn load_ai_chat_initial_data(
    default_model_profile_id: Option<String>,
) -> AiChatLoadResult {
    let Some(path) = ai_chat_database_path() else {
        return AiChatLoadResult {
            conversations: Vec::new(),
            active_conversation_id: None,
            messages: Vec::new(),
            database_error: Some(
                "当前平台没有可用的应用配置目录，无法保存 AI 对话历史".to_string(),
            ),
        };
    };

    match load_ai_chat_conversations(&path) {
        Ok(conversations) => {
            if conversations.is_empty() {
                let conversation = new_ai_chat_conversation(default_model_profile_id);
                return match insert_ai_chat_conversation(&path, &conversation) {
                    Ok(()) => AiChatLoadResult {
                        active_conversation_id: Some(conversation.id.clone()),
                        conversations: vec![conversation],
                        messages: Vec::new(),
                        database_error: None,
                    },
                    Err(error) => AiChatLoadResult {
                        conversations: Vec::new(),
                        active_conversation_id: None,
                        messages: Vec::new(),
                        database_error: Some(error),
                    },
                };
            }

            let active_conversation_id = conversations
                .first()
                .map(|conversation| conversation.id.clone());
            let messages = active_conversation_id
                .as_deref()
                .map(|conversation_id| load_ai_chat_messages(&path, conversation_id))
                .transpose();
            match messages {
                Ok(messages) => AiChatLoadResult {
                    conversations,
                    active_conversation_id,
                    messages: messages.unwrap_or_default(),
                    database_error: None,
                },
                Err(error) => AiChatLoadResult {
                    conversations,
                    active_conversation_id,
                    messages: Vec::new(),
                    database_error: Some(error),
                },
            }
        }
        Err(error) => AiChatLoadResult {
            conversations: Vec::new(),
            active_conversation_id: None,
            messages: Vec::new(),
            database_error: Some(error),
        },
    }
}

/// 创建 AI 消息列表状态。
///
/// 业务意图：
/// - 消息列表使用可变高度气泡，小会话完整测量能让滚动条比例从首帧开始准确。
/// - 历史消息较多时，完整测量会在第一次进入 AI 页同步解析 Markdown、初始化代码高亮并排版所有气泡，
///   直接造成 1-2 秒主线程卡顿，因此大列表改为 GPUI 默认的可见区域渐进测量。
///
/// 边界条件：
/// - 大列表的滚动条总高度会随着用户向上滚动逐步校准，这是用首帧响应速度换取的明确折中。
pub(in crate::app) fn ai_chat_message_list_state(message_count: usize) -> ListState {
    let list_state = ListState::new(
        message_count,
        ListAlignment::Bottom,
        px(AI_CHAT_VIRTUAL_LIST_OVERDRAW),
    );
    if ai_chat_should_measure_all_messages(message_count) {
        list_state.measure_all()
    } else {
        list_state
    }
}

/// 判断 AI 消息列表首帧是否允许完整测量全部历史消息。
///
/// 业务意图：
/// - 小会话保留完整测量，保证滚动条高度和底部对齐从首帧开始稳定。
/// - 大会话必须跳过完整测量，否则第一次进入 AI 页会同步解析和排版所有历史消息，造成明显卡顿。
pub(in crate::app) fn ai_chat_should_measure_all_messages(message_count: usize) -> bool {
    message_count <= AI_CHAT_MESSAGE_MEASURE_ALL_THRESHOLD
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

/// 判断 AI 流式消息是否需要显式触发虚拟列表行高失效。
///
/// 业务意图：
/// - 可见消息行会由 GPUI 在当前帧重新布局；离屏消息只需要按批次失效，避免每个 SSE 增量都让滚动条重新计算。
/// - 完成、停止或失败时必须允许强制失效一次，让终态文案和最终正文高度在用户再次滚动到该消息前保持一致。
pub(in crate::app) fn ai_chat_stream_row_invalidation_due(
    last_invalidated_len: usize,
    current_len: usize,
    force: bool,
) -> bool {
    force
        || current_len.saturating_sub(last_invalidated_len)
            >= AI_CHAT_STREAM_ROW_INVALIDATE_BYTE_THRESHOLD
}
