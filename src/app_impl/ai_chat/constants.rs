// AI 对话功能域的尺寸、数据库和请求常量。
//
// 业务意图：
// - 常量集中放置，保证渲染、状态、存储和测试使用同一组数值，重构不改变任何 UI 或协议行为。
// - 所有尺寸值继续使用 GPUI 逻辑像素，由框架处理 macOS 和 Windows 的缩放差异。

use std::sync::atomic::AtomicU64;

/// AI 对话历史数据库文件名。
///
/// 业务意图：
/// - AI 对话需要保存多会话和完整消息历史，使用单个 SQLite 文件可以在后续扩展搜索、重命名和导出时避免反复迁移零散 JSON。
/// - 文件仍放在现有应用配置目录下，沿用 macOS/Windows 已确认的配置目录策略。
pub(in crate::app) const AI_CHAT_DATABASE_FILE_NAME: &str = "ai-chat.db";

/// AI 对话数据库首版 schema 版本。
///
/// 业务意图：
/// - SQLite `PRAGMA user_version` 用于后续迁移判断；首版固定为 1，避免未来新增列时无法区分历史数据库。
pub(in crate::app) const AI_CHAT_DATABASE_SCHEMA_VERSION: i64 = 1;

/// AI 对话实体 ID 的进程内单调序号。
///
/// 业务意图：
/// - 会话和消息 ID 需要在本地 SQLite 主键中稳定唯一；仅依赖系统时间会受到 Windows、虚拟机或低精度时钟影响。
/// - 原子序号允许后台和 UI 线程同时生成 ID 时仍保持唯一，`Relaxed` 足够满足“不重复”的原子递增语义。
pub(in crate::app) static AI_CHAT_ENTITY_ID_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// AI 对话请求的超时时间。
///
/// 业务意图：
/// - 流式生成可能持续较久，但连接建立、首包和后续读取仍不能无限等待；120 秒兼顾本地大模型和远程服务异常场景。
pub(in crate::app) const AI_CHAT_REQUEST_TIMEOUT_SECONDS: u64 = 120;

/// AI 对话页左侧会话列表宽度。
///
/// UI 约束：
/// - 固定宽度让消息区和输入区在窗口缩放时保持稳定，避免会话标题变化导致聊天正文横向跳动。
pub(in crate::app) const AI_CHAT_CONVERSATION_LIST_WIDTH: f32 = 260.0;

/// AI 对话左右两侧顶部栏的统一高度。
///
/// UI 约束：
/// - 历史会话栏和右侧对话窗口共享同一条顶部分隔线，必须使用同一高度避免横线错位。
pub(in crate::app) const AI_CHAT_TOP_BAR_HEIGHT: f32 = 46.0;

/// AI 对话输入区默认高度。
///
/// UI 约束：
/// - 输入文本区需要平铺底部容器，模型选择和发送按钮作为内部浮层显示，因此高度包含文本编辑空间和底部浮层预留空间。
pub(in crate::app) const AI_CHAT_INPUT_DEFAULT_HEIGHT: f32 = 132.0;

/// AI 对话输入区允许拖拽到的最小高度。
///
/// UI 约束：
/// - 输入区底部存在模型选择和发送按钮浮层；最小高度必须保证单行文本和浮层不会互相遮挡。
pub(in crate::app) const AI_CHAT_INPUT_MIN_HEIGHT: f32 = 96.0;

/// AI 对话输入区允许拖拽到的最大绝对高度。
///
/// UI 约束：
/// - 输入区不能无限占用消息流空间，尤其在小屏窗口中必须保留足够消息可视区域。
pub(in crate::app) const AI_CHAT_INPUT_MAX_HEIGHT: f32 = 360.0;

/// AI 对话输入区最大可占窗口高度比例。
///
/// 跨平台约束：
/// - macOS 和 Windows 标题栏高度由系统管理，比例按 GPUI 视口逻辑像素计算，避免固定值在小屏下压缩消息区。
pub(in crate::app) const AI_CHAT_INPUT_MAX_VIEWPORT_RATIO: f32 = 0.45;

/// AI 对话输入区高度拖拽条的命中高度。
///
/// UI 约束：
/// - 命中区略高于视觉线条，保证触控板和高 DPI 鼠标更容易抓住；事件会被消费，避免误点到消息区。
pub(in crate::app) const AI_CHAT_INPUT_RESIZE_HANDLE_HEIGHT: f32 = 8.0;

/// AI 对话输入栏的内边距。
pub(in crate::app) const AI_CHAT_INPUT_BAR_PADDING: f32 = 16.0;

/// AI 对话模型选择器的固定高度。
pub(in crate::app) const AI_CHAT_MODEL_SELECTOR_HEIGHT: f32 = 30.0;

/// AI 对话发送按钮的固定高度。
pub(in crate::app) const AI_CHAT_SEND_BUTTON_HEIGHT: f32 = 34.0;

/// AI 对话输入框内底部浮层的水平内缩。
pub(in crate::app) const AI_CHAT_INPUT_FLOATING_CONTROLS_HORIZONTAL_INSET: f32 = 10.0;

/// AI 对话输入框内底部浮层的底部内缩。
pub(in crate::app) const AI_CHAT_INPUT_FLOATING_CONTROLS_BOTTOM_INSET: f32 = 10.0;

/// AI 对话文本输入内容的底部预留空间。
///
/// UI 约束：
/// - 模型选择器和发送按钮悬浮在输入框底部，文本绘制和滚动区域必须预留空间，避免最后一行被浮层遮挡。
pub(in crate::app) const AI_CHAT_INPUT_CONTENT_BOTTOM_PADDING: f32 =
    AI_CHAT_INPUT_FLOATING_CONTROLS_BOTTOM_INSET + AI_CHAT_SEND_BUTTON_HEIGHT + 12.0;

/// AI 对话模型菜单相对工作区左侧的偏移。
pub(in crate::app) const AI_CHAT_MODEL_MENU_LEFT_OFFSET: f32 =
    AI_CHAT_INPUT_BAR_PADDING + AI_CHAT_INPUT_FLOATING_CONTROLS_HORIZONTAL_INSET;

/// AI 对话模型菜单相对工作区底部的偏移。
///
/// UI 约束：
/// - 模型选择器悬浮在输入框底部，下拉菜单从浮层上方展开，避免遮挡正在输入的底部控件。
pub(in crate::app) const AI_CHAT_MODEL_MENU_BOTTOM_OFFSET: f32 = AI_CHAT_INPUT_BAR_PADDING
    + AI_CHAT_INPUT_FLOATING_CONTROLS_BOTTOM_INSET
    + AI_CHAT_SEND_BUTTON_HEIGHT
    + 4.0;

/// AI 对话虚拟列表的上下预渲染高度。
///
/// 性能约束：
/// - 聊天消息和历史会话可能持续增长，虚拟列表只渲染可视区域及少量缓冲，避免滚动时为所有历史创建 GPUI 元素。
pub(in crate::app) const AI_CHAT_VIRTUAL_LIST_OVERDRAW: f32 = 180.0;

/// AI 对话列表滚动条宽度。
pub(in crate::app) const AI_CHAT_SCROLLBAR_WIDTH: f32 = 6.0;

/// AI 对话列表滚动条最小滑块高度。
pub(in crate::app) const AI_CHAT_SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 32.0;

/// AI 对话列表滚动条距离容器边缘的内缩。
pub(in crate::app) const AI_CHAT_SCROLLBAR_PADDING: f32 = 4.0;

/// AI 对话消息之间的垂直间距。
///
/// UI 约束：
/// - 消息列表使用可变高度虚拟列表，间距必须作为消息行内部 padding 参与测量，不能依赖外部 margin。
pub(in crate::app) const AI_CHAT_MESSAGE_ROW_GAP: f32 = 20.0;

/// AI 对话第一条消息与顶部栏之间的间距。
///
/// UI 约束：
/// - 消息容器本身不再保留上下 padding，避免滚动到顶部/底部时出现遮挡感。
/// - 首条消息的顶部间距必须放在虚拟列表行内部，确保 `ListState` 测量高度时把这段留白计入滚动范围。
pub(in crate::app) const AI_CHAT_FIRST_MESSAGE_TOP_GAP: f32 = 24.0;

/// AI 对话多行输入行高。
pub(in crate::app) const AI_CHAT_INPUT_LINE_HEIGHT: f32 = 20.0;

/// AI 对话消息区默认提示中展示的会话标题截断长度。
pub(in crate::app) const AI_CHAT_TITLE_MAX_CHARS: usize = 40;
