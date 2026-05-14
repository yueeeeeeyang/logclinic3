// AI 对话业务域常量。
//
// 业务意图：
// - 这里只保存数据库、请求和标题生成等纯业务常量，避免顶层 AI 域携带 GPUI 布局细节。
// - UI 尺寸常量继续留在 `app/ai_chat/constants.rs`，保证应用壳层只处理渲染约束。

use std::sync::atomic::AtomicU64;

/// AI 对话历史数据库文件名。
///
/// 业务意图：
/// - AI 对话需要保存多会话和完整消息历史，使用单个 SQLite 文件可以在后续扩展搜索、重命名和导出时避免反复迁移零散 JSON。
/// - 文件仍放在现有应用配置目录下，沿用 macOS/Windows 已确认的配置目录策略。
pub(crate) const AI_CHAT_DATABASE_FILE_NAME: &str = "ai-chat.db";

/// AI 对话数据库 schema 版本。
///
/// 业务意图：
/// - SQLite `PRAGMA user_version` 用于迁移判断；版本 2 增加助手推理内容字段，用于区分“思考过程”和正式回复。
pub(crate) const AI_CHAT_DATABASE_SCHEMA_VERSION: i64 = 2;

/// AI 对话实体 ID 的进程内单调序号。
///
/// 业务意图：
/// - 会话和消息 ID 需要在本地 SQLite 主键中稳定唯一；仅依赖系统时间会受到 Windows、虚拟机或低精度时钟影响。
/// - 原子序号允许后台和 UI 线程同时生成 ID 时仍保持唯一，`Relaxed` 足够满足“不重复”的原子递增语义。
pub(crate) static AI_CHAT_ENTITY_ID_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// AI 对话请求的超时时间。
///
/// 业务意图：
/// - 流式生成可能持续较久，但连接建立、首包和后续读取仍不能无限等待；120 秒兼顾本地大模型和远程服务异常场景。
pub(crate) const AI_CHAT_REQUEST_TIMEOUT_SECONDS: u64 = 120;

/// AI 对话消息区默认提示中展示的会话标题截断长度。
///
/// 业务意图：
/// - 标题生成属于会话领域规则，不依赖 GPUI 布局；左侧列表只消费生成后的标题文本。
pub(crate) const AI_CHAT_TITLE_MAX_CHARS: usize = 40;
