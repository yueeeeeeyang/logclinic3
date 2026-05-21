// AI 对话 GPUI 壳层尺寸常量。
//
// 业务意图：
// - 常量集中放置，保证渲染、状态和测试使用同一组 UI 数值，重构不改变任何页面布局行为。
// - 所有尺寸值继续使用 GPUI 逻辑像素，由框架处理 macOS 和 Windows 的缩放差异。

use super::TOOLBAR_HEIGHT;

/// AI 对话页左侧会话列表默认宽度。
///
/// UI 约束：
/// - 默认宽度和笔记页左侧树保持一致，让应用内资源列表区域在功能切换时具有稳定视觉节奏。
/// - 用户拖拽后的宽度只保存在当前会话状态中，不写入配置，避免一次临时查看长标题影响后续启动。
pub(in crate::app) const AI_CHAT_CONVERSATION_LIST_WIDTH: f32 = 260.0;

/// AI 对话页左侧会话列表允许拖拽到的最小宽度。
///
/// 边界条件：
/// - 该宽度需要容纳会话标题、滚动条和顶部新增按钮，避免拖得过窄后列表失去可识别性。
pub(in crate::app) const AI_CHAT_CONVERSATION_LIST_MIN_WIDTH: f32 = 220.0;

/// AI 对话页左侧会话列表允许拖拽到的最大宽度。
///
/// 边界条件：
/// - 固定最大值避免历史会话栏吞掉右侧消息阅读区；极窄窗口下还会再受工作区最小宽度约束。
pub(in crate::app) const AI_CHAT_CONVERSATION_LIST_MAX_WIDTH: f32 = 420.0;

/// AI 对话右侧消息工作区拖拽时需要保留的最小宽度。
///
/// UI 约束：
/// - 右侧需要容纳消息流、输入框和模型选择控件，窗口较窄时优先保证对话仍可继续使用。
pub(in crate::app) const AI_CHAT_WORKSPACE_MIN_WIDTH: f32 = 420.0;

/// AI 对话左右两侧顶部栏的统一高度。
///
/// UI 约束：
/// - 历史会话栏和右侧对话窗口共享同一条顶部分隔线，必须使用同一高度避免横线错位。
/// - 顶部栏高度和日志分析页操作栏保持一致，保证主功能页切换时 header 高度稳定。
pub(in crate::app) const AI_CHAT_TOP_BAR_HEIGHT: f32 = TOOLBAR_HEIGHT;

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

/// AI 对话消息首帧允许完整测量的最大条数。
///
/// 性能约束：
/// - 小会话完整测量可以保持滚动条比例稳定，成本也很低。
/// - 大会话如果首次进入页面就测量所有历史消息，会同步触发 Markdown 解析、代码高亮和文本排版，造成明显卡顿。
/// - 超过该阈值后只测量可见消息和少量 overdraw，向上滚动时再渐进测量历史消息。
pub(in crate::app) const AI_CHAT_MESSAGE_MEASURE_ALL_THRESHOLD: usize = 40;

/// AI 流式消息离屏时触发虚拟列表行高失效的正文增长阈值。
///
/// 业务意图：
/// - 可见消息行会在 GPUI `list` 的正常布局阶段重新测量，不需要显式失效。
/// - 离屏消息如果完全不失效，用户滚动到正在增长的助手回复时会一次性修正缓存高度，导致滚动条跳变。
/// - 每个 SSE 增量都失效又会把行高临时归零，造成滚动条持续闪烁；因此用批量阈值折中正确性和视觉稳定性。
pub(in crate::app) const AI_CHAT_STREAM_ROW_INVALIDATE_BYTE_THRESHOLD: usize = 2048;

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
