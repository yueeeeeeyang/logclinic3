// 笔记 GPUI 壳层尺寸常量。
//
// 业务意图：
// - 常量集中放置，保证渲染、状态和测试使用同一组 UI 数值，避免笔记页面布局约束散落在多个文件。
// - 所有尺寸值使用 GPUI 逻辑像素，由框架处理 macOS 和 Windows 的缩放差异。

/// 笔记页左侧树宽度。
pub(in crate::app) const NOTES_TREE_WIDTH: f32 = 280.0;

/// 笔记树默认展开深度。
pub(in crate::app) const NOTES_TREE_DEFAULT_EXPANDED_DEPTH: usize = 2;

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

/// 代码块横向内边距。
pub(in crate::app) const NOTES_CODE_BLOCK_HORIZONTAL_PADDING: f32 = 12.0;

/// 代码块纵向内边距。
pub(in crate::app) const NOTES_CODE_BLOCK_VERTICAL_PADDING: f32 = 10.0;

/// 代码块语言标签所在的顶部区域高度。
pub(in crate::app) const NOTES_CODE_BLOCK_HEADER_HEIGHT: f32 = 24.0;

/// 代码块每行高度。
pub(in crate::app) const NOTES_CODE_BLOCK_LINE_HEIGHT: f32 = 21.0;

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
