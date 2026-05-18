// 笔记 GPUI 壳层尺寸常量。
//
// 业务意图：
// - 常量集中放置，保证渲染、状态和测试使用同一组 UI 数值，避免笔记页面布局约束散落在多个文件。
// - 所有尺寸值使用 GPUI 逻辑像素，由框架处理 macOS 和 Windows 的缩放差异。

use crate::app::ui::AI_CHAT_CONVERSATION_LIST_WIDTH;

/// 笔记页左侧树默认宽度。
///
/// UI 约束：
/// - 默认值和 AI 对话左侧会话栏保持一致，让应用内“左侧资源列表”在不同功能页之间有稳定的视觉宽度。
/// - 用户拖拽后的宽度只保存在当前会话状态中，不写入配置，避免临时调整影响后续启动默认布局。
pub(in crate::app) const NOTES_TREE_DEFAULT_WIDTH: f32 = AI_CHAT_CONVERSATION_LIST_WIDTH;

/// 笔记页左侧树允许拖拽到的最小宽度。
///
/// 边界条件：
/// - 该宽度需要容纳展开箭头、文件图标和短标题，避免拖得过窄后树行只剩不可识别的图标。
pub(in crate::app) const NOTES_TREE_MIN_WIDTH: f32 = 220.0;

/// 笔记页左侧树允许拖拽到的最大宽度。
///
/// 边界条件：
/// - 固定最大值避免树面板吞掉 A4 编辑纸张的主要阅读区域；极窄窗口下仍会再受工作区最小宽度约束。
pub(in crate::app) const NOTES_TREE_MAX_WIDTH: f32 = 420.0;

/// 笔记工作区拖拽时需要保留的最小宽度。
///
/// UI 约束：
/// - 右侧需要容纳顶部操作区和 A4 纸张滚动容器；窗口较窄时优先保证工作区仍可操作。
pub(in crate::app) const NOTES_WORKSPACE_MIN_WIDTH: f32 = 420.0;

/// 笔记树默认展开深度。
pub(in crate::app) const NOTES_TREE_DEFAULT_EXPANDED_DEPTH: usize = 2;

/// 笔记树行文字字号。
///
/// UI 约束：
/// - 笔记左侧树通常展示较多真实目录和 Markdown 文件名，13px 比日志树默认字号更紧凑，能在同样宽度下保留更多路径信息。
/// - 该值只影响笔记树行，不复用日志树字号，避免后续调整日志目录树时连带改变笔记页密度。
pub(in crate::app) const NOTES_TREE_FONT_SIZE: f32 = 13.0;

/// 笔记源码阅读和编辑区行高。
pub(in crate::app) const NOTES_TEXT_LINE_HEIGHT: f32 = 21.0;

/// 笔记源码阅读和编辑区字号。
#[allow(dead_code)]
pub(in crate::app) const NOTES_TEXT_FONT_SIZE: f32 = 14.0;

/// 源码阅读器首列缩进。
#[allow(dead_code)]
pub(in crate::app) const NOTES_SOURCE_HORIZONTAL_PADDING: f32 = 18.0;

/// 源码阅读器字符命中估算宽度。
///
/// 业务意图：
/// - 第一版阅读器不做完整编辑器级字形命中缓存；普通复制场景先用固定宽度近似命中，编辑器输入仍通过 GPUI shaping 精确处理。
#[allow(dead_code)]
pub(in crate::app) const NOTES_SOURCE_CHAR_WIDTH: f32 = 8.0;

/// 笔记编辑器默认内边距。
pub(in crate::app) const NOTES_EDITOR_PADDING: f32 = 16.0;

/// 笔记富文本 A4 纸张宽度。
///
/// 业务意图：
/// - 编辑态和预览态都需要呈现接近 A4 纸张的固定版心，让用户书写和查看时有一致的页面边界。
/// - 794x1123 是 96 DPI 下 A4 纸 8.27x11.69 英寸的常见逻辑像素近似值，和 GPUI 逻辑像素在 macOS/Windows 缩放下保持稳定。
pub(in crate::app) const NOTES_A4_PAGE_WIDTH: f32 = 794.0;

/// 笔记富文本 A4 纸张高度。
///
/// 边界条件：
/// - 该高度只是视觉最小高度，不做分页截断；正文超过一页时纸张会随内容继续向下增长，避免丢失可编辑区域。
pub(in crate::app) const NOTES_A4_PAGE_HEIGHT: f32 = 1123.0;

/// A4 纸张与滚动视口左右边缘的最小留白。
pub(in crate::app) const NOTES_A4_PAGE_GUTTER: f32 = 32.0;

/// A4 纸张与工具栏之间、以及底部滚动边缘之间的留白。
pub(in crate::app) const NOTES_A4_PAGE_VERTICAL_MARGIN: f32 = 28.0;

/// 富文本正文横向内边距。
pub(in crate::app) const NOTES_RICH_TEXT_HORIZONTAL_PADDING: f32 = 18.0;

/// 富文本正文纵向内边距。
pub(in crate::app) const NOTES_RICH_TEXT_VERTICAL_PADDING: f32 = 14.0;

/// 富文本工具栏高度。
pub(in crate::app) const NOTES_RICH_TEXT_TOOLBAR_HEIGHT: f32 = 40.0;

/// 笔记 AI 侧边栏宽度。
///
/// UI 约束：
/// - 侧边栏只服务当前编辑中的笔记，宽度需要容纳模型选择、对话预览和输入框，同时不能把 A4 编辑区挤到不可用。
/// - 该宽度只在笔记编辑态使用，不写入配置文件，避免临时 AI 辅助布局影响后续普通笔记阅读。
pub(in crate::app) const NOTES_AI_ASSISTANT_WIDTH: f32 = 360.0;

/// 笔记 AI 输入框默认高度。
///
/// 业务意图：
/// - 用户通常输入一句生成或改写要求，默认高度给两到三行文本空间；更长输入仍可通过内部滚动完整编辑。
pub(in crate::app) const NOTES_AI_INPUT_HEIGHT: f32 = 116.0;

/// 笔记 AI 输入框行高。
///
/// 跨平台约束：
/// - macOS 和 Windows 的默认 UI 字体字形高度不同，固定逻辑像素行高可以避免 IME 光标和多行命中在不同平台明显漂移。
pub(in crate::app) const NOTES_AI_INPUT_LINE_HEIGHT: f32 = 20.0;

/// 笔记 AI 输入框底部浮层预留高度。
///
/// UI 约束：
/// - 发送按钮位于输入框底部浮层，正文滚动区需要保留空间，避免最后一行文字被按钮遮挡。
pub(in crate::app) const NOTES_AI_INPUT_BOTTOM_PADDING: f32 = 44.0;

/// 代码块横向内边距。
pub(in crate::app) const NOTES_CODE_BLOCK_HORIZONTAL_PADDING: f32 = 16.0;

/// 代码块纵向内边距。
pub(in crate::app) const NOTES_CODE_BLOCK_VERTICAL_PADDING: f32 = 12.0;

/// 代码块语言标签所在的顶部区域高度。
pub(in crate::app) const NOTES_CODE_BLOCK_HEADER_HEIGHT: f32 = 34.0;

/// 代码块每行高度。
pub(in crate::app) const NOTES_CODE_BLOCK_LINE_HEIGHT: f32 = 22.0;

/// 代码块文本字号。
///
/// 业务意图：
/// - 代码块需要比正文稍紧凑，配合卡片内边距展示更多参数和命令；仍保持足够高度避免中英文混排时压线。
pub(in crate::app) const NOTES_CODE_BLOCK_FONT_SIZE: f32 = 13.0;

/// 代码块底部横向滚动条高度。
pub(in crate::app) const NOTES_CODE_BLOCK_SCROLLBAR_HEIGHT: f32 = 8.0;

/// 代码块与相邻正文之间的垂直间距。
pub(in crate::app) const NOTES_CODE_BLOCK_VERTICAL_GAP: f32 = 8.0;

/// 代码块语言下拉支持的常用语言。
///
/// 业务意图：
/// - 第一版不提供自由输入，避免用户输入任意值后高亮表现不可预期；未知旧数据仍由高亮层降级为纯文本。
pub(in crate::app) const NOTES_CODE_BLOCK_LANGUAGES: &[&str] = &[
    "text",
    "rust",
    "java",
    "kotlin",
    "javascript",
    "typescript",
    "json",
    "xml",
    "yaml",
    "toml",
    "sql",
    "shell",
    "python",
];
