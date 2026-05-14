// AI 对话业务领域类型和纯状态辅助函数。
//
// 业务意图：
// - 会话、消息、流式任务和标题生成同时服务 SQLite、OpenAI 兼容请求和 GPUI 展示，应独立于 UI 壳层维护。
// - 这里不持有窗口、焦点、排版或滚动条状态，确保顶层 `ai_chat` 不反向依赖 `app`。

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use crate::config::ModelProfile;

use super::*;

/// AI 对话消息角色。
///
/// 业务意图：
/// - OpenAI Chat Completions 只接受 `user`、`assistant` 等固定角色；内部使用枚举避免数据库或 UI 拼错字符串。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AiChatMessageRole {
    /// 用户输入的问题或指令。
    User,
    /// 模型生成的回复。
    Assistant,
}

impl AiChatMessageRole {
    /// 返回写入数据库和请求体的稳定角色字符串。
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    /// 从数据库字符串恢复消息角色。
    ///
    /// 错误处理：
    /// - 数据库可能被用户或旧版本手工修改，未知角色直接返回中文错误，避免错误消息混入上下文发送给模型。
    pub(crate) fn from_str(raw: &str) -> Result<Self, String> {
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
pub(crate) enum AiChatMessageStatus {
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
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Streaming => "streaming",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    /// 从数据库字符串恢复消息状态。
    pub(crate) fn from_str(raw: &str) -> Result<Self, String> {
        match raw {
            "complete" => Ok(Self::Complete),
            "streaming" => Ok(Self::Streaming),
            "stopped" => Ok(Self::Stopped),
            "failed" => Ok(Self::Failed),
            other => Err(format!("AI 对话数据库包含未知消息状态：{other}")),
        }
    }
}

/// AI 对话思考强度。
///
/// 业务意图：
/// - 该值只在深度思考开启时随请求发送给模型服务，限制为当前服务端支持的 `high` 和 `max`，避免 UI 传出未知字符串。
/// - 当前不持久化到配置文件，防止不同模型或兼容服务对思考强度支持不一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AiChatReasoningEffort {
    /// 较高思考强度，作为默认值，兼顾响应质量和耗时。
    High,
    /// 最大思考强度，适合复杂排障问题，可能带来更长响应时间。
    Max,
}

impl AiChatReasoningEffort {
    /// 转换为模型服务请求体使用的协议字符串。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Max => "max",
        }
    }

    /// 转换为界面展示的中文标签。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::High => "高",
            Self::Max => "最大",
        }
    }
}

/// AI 对话会话摘要。
///
/// 业务意图：
/// - 左侧会话列表只需要标题、模型配置引用和时间排序，不加载所有消息正文，避免历史较多时切换页面成本过高。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiChatConversation {
    /// 会话稳定 ID，作为消息外键和 UI 选中状态。
    pub(crate) id: String,
    /// 用户可见标题；第一版由首条用户消息自动生成。
    pub(crate) title: String,
    /// 当前会话使用的模型配置 ID；配置被删除时保留原值并在 UI 中提示用户重新选择。
    pub(crate) model_profile_id: Option<String>,
    /// 创建时间，Unix epoch 毫秒。
    pub(crate) created_at_ms: i64,
    /// 最近更新时间，Unix epoch 毫秒，用于左侧列表排序。
    pub(crate) updated_at_ms: i64,
}

/// AI 对话消息。
///
/// 业务意图：
/// - 消息同时服务 UI 展示、SQLite 持久化和下一次 Chat Completions 请求上下文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiChatMessage {
    /// 消息稳定 ID。
    pub(crate) id: String,
    /// 所属会话 ID。
    pub(crate) conversation_id: String,
    /// 用户或助手角色。
    pub(crate) role: AiChatMessageRole,
    /// 消息正文；失败消息可能为空，错误原因放在 `error_message`。
    pub(crate) content: String,
    /// 模型服务显式返回的推理内容。
    ///
    /// 业务意图：
    /// - 该字段只保存 OpenAI 兼容响应中显式提供的 reasoning 字段，不存放本客户端或提示词伪造的内部思维。
    /// - 后续请求上下文只发送正式 `content`，不会把历史推理内容回传给模型。
    pub(crate) reasoning_content: String,
    /// 消息生命周期状态。
    pub(crate) status: AiChatMessageStatus,
    /// 失败时的用户可见中文错误。
    pub(crate) error_message: Option<String>,
    /// 会话内单调递增顺序号，保证跨平台和跨重启排序稳定。
    pub(crate) sequence: i64,
    /// 创建时间，Unix epoch 毫秒。
    pub(crate) created_at_ms: i64,
    /// 最近更新时间，Unix epoch 毫秒。
    pub(crate) updated_at_ms: i64,
}

/// AI 对话流式后台事件。
///
/// 业务意图：
/// - 后台线程不能直接修改 GPUI 状态，只能把增量内容、结束和错误事件发给前台任务统一处理。
pub(crate) enum AiChatStreamEvent {
    /// 收到一段助手回复增量。
    Delta(String),
    /// 收到一段模型服务显式返回的推理增量。
    ReasoningDelta(String),
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
pub(crate) struct AiChatStreamingTask {
    /// 当前任务 ID。
    pub(crate) job_id: usize,
    /// 任务所属会话 ID。
    pub(crate) conversation_id: String,
    /// 正在写入的助手消息 ID。
    pub(crate) assistant_message_id: String,
    /// 后台线程发来的流式事件。
    pub(crate) receiver: Receiver<AiChatStreamEvent>,
    /// 用户点击停止时置位。
    pub(crate) cancel: Arc<AtomicBool>,
    /// 最近一次持久化到 SQLite 的正文长度，用于减少流式过程中的写库频率。
    pub(crate) last_persisted_len: usize,
    /// 最近一次虚拟列表行高已同步或显式失效时的正文长度。
    ///
    /// UI 约束：
    /// - 流式输出可能每几十毫秒收到一个增量，如果每次都重置虚拟列表测量高度，滚动条会因为临时 0 高度持续闪烁。
    /// - 该字段只记录当前任务内的 UI 行高同步进度，不写入数据库，也不影响请求上下文。
    pub(crate) last_list_invalidated_len: usize,
}

/// 返回当前 Unix epoch 毫秒。
///
/// 边界条件：
/// - 系统时间异常早于 epoch 时回退为 0，避免数据库时间字段写入负数后影响排序。
pub(crate) fn current_unix_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// 生成 AI 对话实体 ID。
///
/// 业务意图：
/// - 会话和消息 ID 只需要在本地数据库内稳定唯一；时间戳便于人工排查数据库内容。
/// - 追加进程内原子序号，避免低精度系统时钟在连续创建用户消息和助手消息时产生相同主键。
pub(crate) fn new_ai_chat_entity_id(prefix: &str) -> String {
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
pub(crate) fn finish_ai_chat_assistant_message_in_list(
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
pub(crate) fn ai_chat_default_model_profile_id(
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
pub(crate) fn new_ai_chat_conversation(model_profile_id: Option<String>) -> AiChatConversation {
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
pub(crate) fn ai_chat_title_from_user_message(content: &str) -> String {
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
