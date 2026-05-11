// AI 对话功能域的领域状态和纯状态辅助函数。
//
// 业务意图：
// - 该文件只承载会话、消息、滚动、输入、流式任务等 AI 对话内部模型，以及不触碰 UI/SQLite/网络的纯逻辑。
// - 类型可见性限制在 app 模块内，避免把第一版 AI 对话内部状态暴露成 crate 级 API。

use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use super::*;

/// AI 对话消息角色。
///
/// 业务意图：
/// - OpenAI Chat Completions 只接受 `user`、`assistant` 等固定角色；内部使用枚举避免数据库或 UI 拼错字符串。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum AiChatMessageRole {
    /// 用户输入的问题或指令。
    User,
    /// 模型生成的回复。
    Assistant,
}

impl AiChatMessageRole {
    /// 返回写入数据库和请求体的稳定角色字符串。
    pub(in crate::app) fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    /// 从数据库字符串恢复消息角色。
    ///
    /// 错误处理：
    /// - 数据库可能被用户或旧版本手工修改，未知角色直接返回中文错误，避免错误消息混入上下文发送给模型。
    pub(in crate::app) fn from_str(raw: &str) -> Result<Self, String> {
        match raw {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            other => Err(format!("AI 对话数据库包含未知消息角色：{other}")),
        }
    }
}

/// AI 对话消息状态。
///
/// 业务意图：
/// - 流式生成期间、完成、停止和失败会影响 UI 文案和下一次请求上下文，必须明确区分。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum AiChatMessageStatus {
    /// 消息已经完整可用。
    Complete,
    /// 助手消息正在流式生成。
    Streaming,
    /// 用户主动停止生成，内容可能只是部分回复。
    Stopped,
    /// 请求失败，`error_message` 保存用户可见原因。
    Failed,
}

impl AiChatMessageStatus {
    /// 返回写入数据库的稳定状态字符串。
    pub(in crate::app) fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Streaming => "streaming",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    /// 从数据库字符串恢复消息状态。
    pub(in crate::app) fn from_str(raw: &str) -> Result<Self, String> {
        match raw {
            "complete" => Ok(Self::Complete),
            "streaming" => Ok(Self::Streaming),
            "stopped" => Ok(Self::Stopped),
            "failed" => Ok(Self::Failed),
            other => Err(format!("AI 对话数据库包含未知消息状态：{other}")),
        }
    }
}

/// AI 对话会话摘要。
///
/// 业务意图：
/// - 左侧会话列表只需要标题、模型配置引用和时间排序，不加载所有消息正文，避免历史较多时切换页面成本过高。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct AiChatConversation {
    /// 会话稳定 ID，作为消息外键和 UI 选中状态。
    pub(in crate::app) id: String,
    /// 用户可见标题；第一版由首条用户消息自动生成。
    pub(in crate::app) title: String,
    /// 当前会话使用的模型配置 ID；配置被删除时保留原值并在 UI 中提示用户重新选择。
    pub(in crate::app) model_profile_id: Option<String>,
    /// 创建时间，Unix epoch 毫秒。
    pub(in crate::app) created_at_ms: i64,
    /// 最近更新时间，Unix epoch 毫秒，用于左侧列表排序。
    pub(in crate::app) updated_at_ms: i64,
}

/// AI 对话消息。
///
/// 业务意图：
/// - 消息同时服务 UI 展示、SQLite 持久化和下一次 Chat Completions 请求上下文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct AiChatMessage {
    /// 消息稳定 ID。
    pub(in crate::app) id: String,
    /// 所属会话 ID。
    pub(in crate::app) conversation_id: String,
    /// 用户或助手角色。
    pub(in crate::app) role: AiChatMessageRole,
    /// 消息正文；失败消息可能为空，错误原因放在 `error_message`。
    pub(in crate::app) content: String,
    /// 消息生命周期状态。
    pub(in crate::app) status: AiChatMessageStatus,
    /// 失败时的用户可见中文错误。
    pub(in crate::app) error_message: Option<String>,
    /// 会话内单调递增顺序号，保证跨平台和跨重启排序稳定。
    pub(in crate::app) sequence: i64,
    /// 创建时间，Unix epoch 毫秒。
    pub(in crate::app) created_at_ms: i64,
    /// 最近更新时间，Unix epoch 毫秒。
    pub(in crate::app) updated_at_ms: i64,
}

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

/// AI 对话流式后台事件。
///
/// 业务意图：
/// - 后台线程不能直接修改 GPUI 状态，只能把增量内容、结束和错误事件发给前台任务统一处理。
pub(in crate::app) enum AiChatStreamEvent {
    /// 收到一段助手回复增量。
    Delta(String),
    /// 服务端发送 `[DONE]` 或响应读取到 EOF。
    Done,
    /// 用户请求停止后后台循环退出。
    Stopped,
    /// 网络、HTTP 或 SSE 解析失败。
    Error(String),
}

/// AI 对话正在运行的流式任务。
///
/// 业务意图：
/// - 任务 ID 用于丢弃旧任务事件；停止标记用于让后台读取循环尽快退出；接收器由前台轮询并批量更新 UI。
pub(in crate::app) struct AiChatStreamingTask {
    /// 当前任务 ID。
    pub(in crate::app) job_id: usize,
    /// 任务所属会话 ID。
    pub(in crate::app) conversation_id: String,
    /// 正在写入的助手消息 ID。
    pub(in crate::app) assistant_message_id: String,
    /// 后台线程发来的流式事件。
    pub(in crate::app) receiver: Receiver<AiChatStreamEvent>,
    /// 用户点击停止时置位。
    pub(in crate::app) cancel: Arc<AtomicBool>,
    /// 最近一次持久化到 SQLite 的正文长度，用于减少流式过程中的写库频率。
    pub(in crate::app) last_persisted_len: usize,
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

/// 返回当前 Unix epoch 毫秒。
///
/// 边界条件：
/// - 系统时间异常早于 epoch 时回退为 0，避免数据库时间字段写入负数后影响排序。
pub(in crate::app) fn current_unix_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
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

/// 生成 AI 对话实体 ID。
///
/// 业务意图：
/// - 会话和消息 ID 只需要在本地数据库内稳定唯一；时间戳便于人工排查数据库内容。
/// - 追加进程内原子序号，避免低精度系统时钟在连续创建用户消息和助手消息时产生相同主键。
pub(in crate::app) fn new_ai_chat_entity_id(prefix: &str) -> String {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = AI_CHAT_ENTITY_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{seed}-{sequence}")
}

/// 在内存消息列表中完成一条助手消息，并返回需要落库的消息副本。
///
/// 业务意图：
/// - 正常完成、用户停止、切换会话时的强制停止都必须用同一套状态流转，避免某个分支只取消后台任务但数据库仍停留在 `streaming`。
/// - 返回 clone 是为了先结束可变借用，再执行 SQLite 写入，避免 UI 状态更新和持久化逻辑互相牵扯。
pub(in crate::app) fn finish_ai_chat_assistant_message_in_list(
    messages: &mut [AiChatMessage],
    message_id: &str,
    status: AiChatMessageStatus,
    error_message: Option<String>,
) -> Option<AiChatMessage> {
    let message = messages
        .iter_mut()
        .find(|message| message.id == message_id)?;
    message.status = status;
    message.error_message = error_message;
    message.updated_at_ms = current_unix_time_millis();
    Some(message.clone())
}

/// 返回 AI 对话默认模型配置 ID。
///
/// 业务意图：
/// - 新会话优先使用设置页标记的默认模型；没有默认时使用第一条配置，保证用户只配置一个模型即可直接开始对话。
pub(in crate::app) fn ai_chat_default_model_profile_id(
    profiles: &[ModelProfile],
    default_profile_id: Option<&str>,
) -> Option<String> {
    default_profile_id
        .and_then(|default_id| {
            profiles
                .iter()
                .find(|profile| profile.id == default_id)
                .map(|profile| profile.id.clone())
        })
        .or_else(|| profiles.first().map(|profile| profile.id.clone()))
}

/// 创建空白 AI 对话会话模型。
pub(in crate::app) fn new_ai_chat_conversation(
    model_profile_id: Option<String>,
) -> AiChatConversation {
    let now = current_unix_time_millis();
    AiChatConversation {
        id: new_ai_chat_entity_id("ai-conversation"),
        title: "新对话".to_string(),
        model_profile_id,
        created_at_ms: now,
        updated_at_ms: now,
    }
}

/// 根据首条用户消息生成会话标题。
///
/// 边界条件：
/// - 标题最多取 40 个字符，并把换行折叠为空格，避免左侧列表因为长提示词或多行文本被撑开。
pub(in crate::app) fn ai_chat_title_from_user_message(content: &str) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return "新对话".to_string();
    }
    let mut title = normalized
        .chars()
        .take(AI_CHAT_TITLE_MAX_CHARS)
        .collect::<String>();
    if normalized.chars().count() > AI_CHAT_TITLE_MAX_CHARS {
        title.push_str("...");
    }
    title
}
