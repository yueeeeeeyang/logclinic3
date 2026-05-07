//! LogClinic 桌面客户端的程序入口。
//!
//! 当前阶段负责创建 GPUI 主窗口、顶部工具栏、可拖动左右分栏、左侧日志目录树和右侧日志 tab 工作区。
//! “加载日志”已经接入真实路径选择和来源扫描，日志文件点击后可以读取正文、自动识别编码并只读展示；
//! 搜索、日志正文只读选择复制、语法高亮和编码切换已接入；实时追踪和设置持久化等尚未定义的业务功能
//! 仍需在明确业务规则和验收标准后再接入。
//!
//! 跨平台约束：
//! - macOS 和 Windows 都需要从同一个入口启动主窗口，因此这里不写平台专属逻辑。
//! - 窗口尺寸使用 GPUI 的逻辑像素表达，由 GPUI 负责映射到具体平台窗口系统。
//! - 当前不持久化窗口位置或尺寸，避免在尚未定义配置目录和权限规则前写入用户文件。

use std::{
    borrow::Cow, collections::HashSet, ops::Range, path::PathBuf, sync::Arc, time::Duration,
};

use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt as _, App, AppContext, Application, Bounds, ClickEvent, ClipboardItem,
    Context, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, FontWeight,
    GlobalElementId, InteractiveElement, IntoElement, KeyBinding, KeyDownEvent, Keystroke,
    LayoutId, ListHorizontalSizingBehavior, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, PathPromptOptions, Pixels, Render, ScrollHandle, ScrollStrategy,
    SharedString, StatefulInteractiveElement, Style, Styled as _, StyledText, TitlebarOptions,
    UTF16Selection, UniformListScrollHandle, Window, WindowBounds, WindowOptions, actions, div,
    point, px, relative, rgb, size, uniform_list,
};
use lucide_icons::{Icon, LUCIDE_FONT_BYTES};

mod highlighting;
mod log_content;
mod log_loader;
mod search;

use highlighting::highlight_line;
use log_content::{
    DecodedLogDocument, EncodingChoice, LogContentError, LogTextEncoding, decode_log_bytes,
    read_log_source_bytes,
};
use log_loader::{
    LoadedLogTree, LogFileSource, LogTreeEntryKind, LogTreeRow as LoadedLogTreeRow,
    load_log_sources,
};
use search::{
    SearchFileError, SearchOptions, SearchProgress, SearchResultItem, SearchScope,
    collect_current_directory_sources, search_lines, source_location_label,
};

actions!(logclinic, [OpenSearchDialog]);

/// 应用主窗口的标题。
///
/// 业务意图：
/// - 第一阶段只验证窗口创建，因此标题使用稳定的产品名 `LogClinic`。
/// - 后续如果需要显示当前打开文件名或工作区状态，应在明确标题格式规则后再扩展。
const MAIN_WINDOW_TITLE: &str = "LogClinic";

/// 主窗口的默认宽度。
///
/// 业务约束：
/// - 用户明确要求默认窗口宽度调整为 1400px，以便日志 tab 和正文区域在首次打开时有更多横向空间。
/// - 这里使用浮点字面量是为了匹配 GPUI `px` 的尺寸 API，避免在调用处反复转换。
const MAIN_WINDOW_WIDTH: f32 = 1400.0;

/// 主窗口的默认高度。
///
/// 边界说明：
/// - 用户要求默认高度从 900 调整为 800，因此这里是窗口启动时的初始高度。
/// - 当前不设置最小尺寸和最大尺寸，因为本阶段只要求默认大小。
/// - 如果后续添加复杂布局，必须再定义小屏幕、缩放比例和窗口尺寸变化的验收标准。
const MAIN_WINDOW_HEIGHT: f32 = 800.0;

/// 顶部工具栏的固定高度。
///
/// 业务意图：
/// - 工具栏用于承载当前已经明确的全局入口：加载日志、智能诊断、设置。
/// - 用户要求工具栏高度和 tab 页签高度一致，因此这里直接复用 `LOG_TAB_BAR_HEIGHT`。
/// - 统一高度可以让顶部全局工具栏和右侧日志 tab 栏的垂直节奏一致，减少首屏跳变感。
///
/// 边界条件：
/// - 当前不实现可换行工具栏；如果后续窗口宽度允许缩小，需要再定义窄宽度下的折叠策略。
const TOOLBAR_HEIGHT: f32 = LOG_TAB_BAR_HEIGHT;

/// 主界面工具栏按钮的水平内边距。
///
/// 实现原因：
/// - 当前按钮是文字按钮，不再使用边框和实体背景，因此水平内边距需要更紧凑。
/// - 统一内边距可以保证三个入口在 macOS 和 Windows 字体渲染差异下仍保持一致点击区域。
const TOOLBAR_BUTTON_HORIZONTAL_PADDING: f32 = 8.0;

/// 主界面工具栏按钮的垂直内边距。
///
/// 边界说明：
/// - 工具栏高度和 tab 栏一致后，垂直内边距仍保持紧凑，避免文字和图标被裁切。
/// - 当前按钮不承载多行文本，也不展示快捷键提示，因此不需要动态高度。
const TOOLBAR_BUTTON_VERTICAL_PADDING: f32 = 4.0;

/// Lucide 图标字体在 GPUI 中使用的字体族名称。
///
/// 业务意图：
/// - `lucide-icons` 通过字体字形暴露图标，必须在渲染图标字符时指定该字体族。
/// - 显式定义字体族名称可以避免图标字符被系统默认字体渲染成方块或错误符号。
///
/// 边界条件：
/// - 字体数据来自 `lucide-icons` 依赖内置的 `LUCIDE_FONT_BYTES`，启动时注册到 GPUI 文本系统。
/// - 如果未来改为 SVG 图标或其它图标库，应同步替换字体注册和图标渲染逻辑。
const LUCIDE_FONT_FAMILY: &str = "lucide";

/// 工具栏按钮图标的固定宽度。
///
/// 业务意图：
/// - 三个 Lucide 图标字形宽度可能不同，固定图标区域可以让按钮文字起点更稳定。
/// - 该宽度只约束图标可视区域，不改变按钮整体点击区域。
///
/// 边界条件：
/// - 当前图标尺寸与工具栏高度配套，避免在 macOS 和 Windows 字体度量差异下发生裁切。
/// - 固定宽度只约束单个图标，不处理多个图标、徽标或加载状态。
const TOOLBAR_BUTTON_ICON_WIDTH: f32 = 16.0;

/// 工具栏按钮图标字号。
///
/// 业务意图：
/// - Lucide 字体图标需要使用接近 16px 的字号才能保持清晰线条和按钮视觉平衡。
/// - 单独定义字号便于后续统一调整工具栏密度。
///
/// 边界条件：
/// - 字号不能超过工具栏高度，否则部分平台的字体上升/下降空间可能导致图标裁切。
const TOOLBAR_BUTTON_ICON_SIZE: f32 = 16.0;

/// 左侧内容区域的默认宽度。
///
/// 业务意图：
/// - 用户明确要求左右两栏布局默认左侧区域宽度为 300px。
/// - 当前不持久化用户拖动后的宽度，避免在配置存储位置和权限未定义前写入用户环境。
const LEFT_PANEL_DEFAULT_WIDTH: f32 = 300.0;

/// 左侧内容区域可拖动到的最小宽度。
///
/// 业务意图：
/// - 左侧区域后续预计承载日志来源、文件列表或过滤条件，过窄会导致基础交互不可用。
/// - 用户未定义最小宽度，因此采用保守的桌面客户端默认值，避免拖动后面板完全消失。
///
/// 边界条件：
/// - 该值只约束当前进程内拖动行为，不代表后续设置持久化或布局配置规则。
const LEFT_PANEL_MIN_WIDTH: f32 = 180.0;

/// 右侧内容区域可保留的最小宽度。
///
/// 业务意图：
/// - 右侧区域后续预计承载日志内容主视图，需要保留足够宽度避免拖动左栏挤占主体区域。
/// - 用户未定义右侧最小宽度，因此当前用 320px 作为临时保护边界。
///
/// 边界条件：
/// - 如果后续右侧加入表格、详情面板或诊断视图，应重新定义最小宽度和窄屏折叠规则。
const RIGHT_PANEL_MIN_WIDTH: f32 = 320.0;

/// 左右内容区域之间的可拖动命中区域宽度。
///
/// 业务意图：
/// - 用户要求分隔条视觉上改成细线，但仍然需要支持拖动改变宽度。
/// - 因此这里保留 6px 透明命中区域，让鼠标更容易命中；命中区通过覆盖在右侧内容上的透明层实现，
///   不再占用额外布局宽度，避免分割线右侧出现白色视觉缝隙。
///
/// 边界条件：
/// - 当前仅支持鼠标拖动，不支持键盘调整、双击重置或触控手势。
/// - 命中区域不参与 flex 布局，拖动边界计算只需要扣除真实可见线条宽度。
const SPLITTER_HIT_WIDTH: f32 = 6.0;

/// 左右内容区域之间分割线的可见宽度。
///
/// 业务意图：
/// - 用户反馈上一版分隔条太粗，因此可见线条收敛为 1px。
/// - 视觉宽度与拖拽命中宽度分离，保证界面轻量的同时不牺牲可用性。
///
/// 边界条件：
/// - 当前线条颜色固定；如果后续加入主题系统，应把颜色纳入主题令牌而不是继续硬编码。
const SPLITTER_VISIBLE_WIDTH: f32 = 1.0;

/// 覆盖在右侧内容上的透明拖动命中宽度。
///
/// 业务意图：
/// - 可见分割线只占 1px，剩余命中区域放到右侧内容之上，让右侧内容视觉上紧贴分割线。
const SPLITTER_OVERLAY_HIT_WIDTH: f32 = SPLITTER_HIT_WIDTH - SPLITTER_VISIBLE_WIDTH;

/// 左侧目录树标题区域高度。
///
/// 业务意图：
/// - 左侧区域用于展示真实加载后的日志目录树，标题区域帮助用户识别当前面板语义。
/// - 当前标题不展示真实绝对路径，避免路径脱敏和窄面板排版规则未定义前暴露过多信息。
const LOG_TREE_HEADER_HEIGHT: f32 = 34.0;

/// 左侧目录树每行固定高度。
///
/// 业务意图：
/// - 固定行高可以让树结构在 macOS 和 Windows 字体渲染差异下保持稳定密度。
/// - 后续接入虚拟列表或大目录时，也可以基于固定行高计算滚动区域。
///
/// 边界条件：
/// - 当前未实现超长目录名的多行展示；长文本统一截断，避免挤压布局。
const LOG_TREE_ROW_HEIGHT: f32 = 28.0;

/// 左侧目录树首次加载后的默认展开深度。
///
/// 业务意图：
/// - 用户要求默认展开 2 级目录，因此深度 0 和深度 1 的可展开节点默认处于展开状态。
/// - 深度 2 的节点默认可见但不继续展开，避免大目录首次渲染时一次性暴露过多文件行。
///
/// 边界条件：
/// - 这里使用加载层提供的 `depth`，只影响初始 UI 展开状态，不裁剪真实扫描结果。
/// - 用户手动展开或收起后只在当前进程内生效，重新加载日志会重新套用默认展开规则。
const LOG_TREE_DEFAULT_EXPANDED_DEPTH: usize = 2;

/// 左侧目录树每一级层级缩进。
///
/// 业务意图：
/// - 树形结构依赖稳定缩进表达父子关系，16px 可以容纳展开箭头和文件夹图标。
/// - 缩进值固定，避免不同节点因为文本长度不同导致层级难以扫描。
const LOG_TREE_ROW_INDENT: f32 = 16.0;

/// 左侧目录树行内容的基础水平内边距。
///
/// 业务意图：
/// - 给树节点与面板边缘之间保留基础留白，避免图标贴边影响可读性。
/// - 缩进会叠加在该基础内边距之上，因此根节点和子节点都保持一致起点规则。
const LOG_TREE_ROW_HORIZONTAL_PADDING: f32 = 10.0;

/// 左侧目录树展开箭头占位宽度。
///
/// 业务意图：
/// - 文件节点没有展开箭头，但仍需要占用同等宽度，保证文件夹和文件名称左边缘对齐。
/// - 该值只影响视觉布局，不代表当前节点支持真实展开或收起。
const LOG_TREE_CHEVRON_WIDTH: f32 = 14.0;

/// 左侧目录树展开箭头字号。
///
/// 边界条件：
/// - 箭头图标必须小于行高，避免在低 DPI 或 Windows 字体度量差异下被裁切。
const LOG_TREE_CHEVRON_SIZE: f32 = 13.0;

/// 左侧目录树文件夹和文件图标占位宽度。
///
/// 业务意图：
/// - 固定图标宽度可以让节点文字起点稳定，提升目录树快速扫描体验。
const LOG_TREE_ITEM_ICON_WIDTH: f32 = 16.0;

/// 左侧目录树文件夹和文件图标字号。
///
/// 边界条件：
/// - 图标字号与行高配套，避免树节点在不同平台字体度量下出现垂直裁切。
const LOG_TREE_ITEM_ICON_SIZE: f32 = 15.0;

/// 左侧目录树滚动条可见宽度。
///
/// 业务意图：
/// - 左侧目录树也使用虚拟列表，文件较多时需要稳定显示滚动位置，避免用户误以为列表只展示当前屏内容。
/// - 宽度和右侧日志正文滚动条保持一致，降低左右区域的视觉差异。
const LOG_TREE_SCROLLBAR_WIDTH: f32 = 6.0;

/// 左侧目录树滚动条最小滑块高度。
///
/// 边界条件：
/// - 大目录中按比例计算出的滑块可能过小，最小高度保证鼠标仍能命中并拖动。
const LOG_TREE_SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 36.0;

/// 左侧目录树滚动条距离面板边缘的内缩。
///
/// 业务意图：
/// - 滚动条贴近右侧但不压住分割线，避免目录树和右侧内容边界混在一起。
const LOG_TREE_SCROLLBAR_PADDING: f32 = 3.0;

/// 右侧日志 tab 栏高度。
///
/// 业务意图：
/// - tab 栏需要容纳多个已打开日志文件的切换入口，同时不抢占日志正文可视高度。
/// - 固定高度便于右侧主体区域继续使用等高虚拟列表渲染日志行。
const LOG_TAB_BAR_HEIGHT: f32 = 34.0;

/// tab 栏两侧滚动箭头按钮宽度。
///
/// 业务意图：
/// - 用户要求打开日志较多时 tab 可以横向滚动，并在两边显示滚动箭头。
/// - 按钮固定宽度可以让 tab 可视区域在滚动前后保持稳定。
const LOG_TAB_SCROLL_BUTTON_WIDTH: f32 = 28.0;

/// tab 栏每次点击滚动箭头时移动的距离。
///
/// 业务意图：
/// - 该值约等于两个中等长度 tab 的宽度，点击次数和定位速度比较平衡。
const LOG_TAB_SCROLL_STEP: f32 = 180.0;

/// tab 内关闭图标按钮宽度。
///
/// 业务意图：
/// - 关闭按钮需要稳定命中区，避免用户只能点到很小的 `X` 字形。
const LOG_TAB_CLOSE_BUTTON_WIDTH: f32 = 18.0;

/// 右侧日志文档工具条高度。
///
/// 业务意图：
/// - 当前工具条只承载编码切换和当前文件状态，后续如果加入搜索或跳转行号也应从这里扩展。
const LOG_DOCUMENT_TOOLBAR_HEIGHT: f32 = 36.0;

/// 右侧日志正文每行固定高度。
///
/// 业务意图：
/// - 日志正文使用 `uniform_list` 进行虚拟渲染，必须保持每行高度一致，才能在大文件滚动时稳定计算可见区间。
const LOG_VIEWER_ROW_HEIGHT: f32 = 22.0;

/// 日志查看器行号列固定宽度。
///
/// 业务意图：
/// - 行号和正文分开渲染，便于用户定位日志上下文，同时避免行号挤压正文起点。
const LOG_VIEWER_LINE_NUMBER_MIN_WIDTH: f32 = 46.0;

/// 日志查看器行号列最大宽度。
///
/// 业务意图：
/// - 大文件行数较多时行号列可以略微变宽，但不能回到上一版过宽导致正文起点过远的问题。
const LOG_VIEWER_LINE_NUMBER_MAX_WIDTH: f32 = 64.0;

/// 日志查看器行号单个数字的估算宽度。
///
/// 业务意图：
/// - GPUI 当前没有在渲染前测量等宽行号文本的必要性，使用稳定估算即可让常见行数下列宽更紧凑。
const LOG_VIEWER_LINE_NUMBER_DIGIT_WIDTH: f32 = 7.0;

/// 日志查看器行号列右侧留白和边框的估算宽度。
///
/// 业务意图：
/// - 行号需要和边框保持一点距离，避免数字贴线影响扫描。
const LOG_VIEWER_LINE_NUMBER_PADDING_WIDTH: f32 = 18.0;

/// 日志正文相对行号列右侧的水平内边距。
///
/// 业务意图：
/// - 行号列固定在左侧，正文需要留出少量间距，避免文本贴着行号分割线显示。
/// - 文本选择命中测试也必须使用同一个偏移，保证鼠标选中的列和实际渲染起点一致。
const LOG_VIEWER_TEXT_LEFT_PADDING: f32 = 8.0;

/// 日志正文等宽字体的近似单字符宽度。
///
/// 业务意图：
/// - 当前日志查看器使用 `uniform_list + StyledText` 自绘，不是系统文本控件，因此 GPUI 不会提供跨行文本选择命中结果。
/// - 为了支持只读选择，需要把鼠标横坐标换算为字符列；内置 JetBrains Mono 在 12px 下单字符宽度约为 7px。
///
/// 边界条件：
/// - 该值服务普通日志、堆栈和配置文件的等宽场景；如果一行包含大量制表符或全角字符，选择列可能与视觉宽度存在轻微偏差。
/// - 复制时仍从真实 UTF-8 字符串按字符列截取，不会按像素反推出文本内容。
const LOG_VIEWER_TEXT_CHAR_WIDTH: f32 = 7.0;

/// 日志正文使用的等宽字体族。
///
/// 业务意图：
/// - 日志通常包含时间戳、线程名和列对齐字段，等宽字体能提升扫描和对比效率。
/// - 用户指定使用 JetBrains Mono，并要求内置到程序中，因此启动时会注册随项目打包的字体字节。
const LOG_VIEWER_FONT_FAMILY: &str = "JetBrains Mono";

/// 内置 JetBrains Mono Regular 字体数据。
///
/// 业务意图：
/// - 日志正文必须使用 JetBrains Mono，不能依赖用户系统是否安装该字体。
/// - `include_bytes!` 会把字体文件编译进可执行产物，macOS 和 Windows 启动时通过同一套 GPUI 字体注册流程加载。
///
/// 边界条件：
/// - 当前只内置 Regular 字重；日志正文高亮仍通过字体系统模拟粗体，后续若要求更高字重质量可追加 Bold 字体文件。
const JETBRAINS_MONO_REGULAR_FONT_BYTES: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

/// 日志正文基础字号。
///
/// 边界条件：
/// - 用户要求默认字号调整为 12px；当前继续沿用 22px 行高，保证单屏可见行数和滚动计算稳定。
/// - 如果后续允许用户自定义字号，行高也必须同步配置，避免虚拟列表等高假设失效。
const LOG_VIEWER_FONT_SIZE: f32 = 12.0;

/// 日志正文滚动条可见宽度。
///
/// 业务意图：
/// - GPUI `uniform_list` 默认只处理滚动交互，不预留可见滚动条；这里自绘轻量滚动条提示当前位置。
const LOG_VIEWER_SCROLLBAR_WIDTH: f32 = 6.0;

/// 日志正文滚动条最小滑块长度。
///
/// 边界条件：
/// - 超长或超宽日志中按比例计算出的滑块可能过小，最小长度保证用户仍能看到并拖动滚动位置。
const LOG_VIEWER_SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 36.0;

/// 日志正文滚动条距离内容边缘的内缩。
///
/// 业务意图：
/// - 让滚动条不贴到窗口边缘和 tab 内容边界，视觉上更轻量。
const LOG_VIEWER_SCROLLBAR_PADDING: f32 = 3.0;

/// 编码下拉框按钮宽度。
///
/// 业务意图：
/// - 编码选择器现在嵌入右侧状态信息中，宽度需要比旧版左侧独立控件更紧凑。
/// - 该宽度仍需容纳最长的 “UTF-8 BOM” 文案和下拉箭头。
const ENCODING_DROPDOWN_BUTTON_WIDTH: f32 = 94.0;

/// 编码下拉框弹层宽度。
///
/// 业务意图：
/// - 所有编码选项文案长度固定，使用稳定宽度避免菜单在不同平台字体下抖动。
const ENCODING_DROPDOWN_WIDTH: f32 = 116.0;

/// 编码下拉按钮高度。
///
/// 业务意图：
/// - 菜单弹层位置需要和按钮底边对齐，因此按钮高度集中成常量，避免渲染处和定位处写两份数字。
const ENCODING_DROPDOWN_BUTTON_HEIGHT: f32 = 24.0;

/// 编码下拉菜单距离按钮底边的垂直间隔。
///
/// 边界条件：
/// - 保留 2px 间隔可以让菜单边框和按钮边框不粘连。
const ENCODING_DROPDOWN_MENU_GAP: f32 = 2.0;

/// 编码下拉框单项高度。
///
/// 边界条件：
/// - 当前每项只包含单行中文或 ASCII 编码名，不需要多行布局。
const ENCODING_DROPDOWN_ITEM_HEIGHT: f32 = 28.0;

/// tab 右键菜单宽度。
///
/// 业务意图：
/// - 三个菜单项文案长度固定，使用稳定宽度可以避免菜单因为字体差异跳动。
const TAB_CONTEXT_MENU_WIDTH: f32 = 132.0;

/// tab 右键菜单单项高度。
///
/// 边界条件：
/// - 菜单项只显示单行中文命令，不承载图标或快捷键。
const TAB_CONTEXT_MENU_ITEM_HEIGHT: f32 = 30.0;

/// 搜索对话框默认宽度。
///
/// 业务意图：
/// - 对话框需要同时容纳查询输入、范围切换和大小写开关，宽度固定可以让拖动和定位行为稳定。
/// - 该对话框是浮层，不参与右侧日志正文布局，因此保持紧凑，避免遮挡过多日志内容。
const SEARCH_DIALOG_WIDTH: f32 = 430.0;

/// 搜索对话框打开时距离窗口左侧的默认位置。
///
/// 边界条件：
/// - 实际打开时会结合当前左侧栏宽度计算，让对话框默认出现在右侧内容区域内。
const SEARCH_DIALOG_DEFAULT_RIGHT_OFFSET: f32 = 48.0;

/// 搜索对话框打开时距离工具栏底部的默认位置。
const SEARCH_DIALOG_DEFAULT_TOP_OFFSET: f32 = 48.0;

/// 搜索输入框高度。
///
/// 业务意图：
/// - 搜索框只承载单行查询词，不支持多行输入，因此固定高度可以简化键盘和点击焦点处理。
const SEARCH_INPUT_HEIGHT: f32 = 30.0;

/// 搜索结果面板默认高度。
///
/// 业务意图：
/// - 面板从右侧内容区底部弹出，默认高度需要能展示多条结果，同时保留上方日志上下文。
const SEARCH_RESULTS_PANEL_DEFAULT_HEIGHT: f32 = 260.0;

/// 搜索结果面板最小高度。
///
/// 边界条件：
/// - 继续允许用户拖小结果面板，但不能小到关闭按钮、摘要和至少一条结果不可见。
const SEARCH_RESULTS_PANEL_MIN_HEIGHT: f32 = 140.0;

/// 搜索结果面板相对右侧内容区的最大高度比例。
///
/// 业务意图：
/// - 用户要求结果面板可以上下拖动高度，但不能完全覆盖日志正文。
const SEARCH_RESULTS_PANEL_MAX_RATIO: f32 = 0.6;

/// 搜索结果面板拖拽条高度。
const SEARCH_RESULTS_PANEL_RESIZER_HEIGHT: f32 = 8.0;

/// 搜索结果行固定高度。
///
/// 业务意图：
/// - 结果列表使用虚拟列表，固定行高可以避免大量命中时创建全部行元素。
const SEARCH_RESULT_ROW_HEIGHT: f32 = 46.0;

/// 顶部工具栏按钮的声明式配置。
///
/// 业务意图：
/// - 将按钮标签和图标绑定在一起，避免渲染代码里出现三组分散的硬编码。
/// - 后续如果按钮需要权限、禁用态或快捷键，可以在这个结构上扩展字段。
///
/// 边界条件：
/// - 当前只包含静态字符串，所有按钮在进程生命周期内固定不变。
/// - “加载日志”已绑定真实系统选择器和路径扫描流程；“智能诊断”和“设置”仍等待业务规则明确后接入。
struct ToolbarAction {
    /// 按钮前置图标。
    ///
    /// 业务意图：
    /// - 使用 Lucide 图标库提供的类型安全枚举，替换上一版难看的纯文本符号。
    /// - 图标语义需要和按钮动作一致，帮助用户快速识别入口类别。
    icon: Icon,

    /// 按钮显示文案。
    ///
    /// 业务意图：
    /// - 文案直接来自用户当前需求，后续如需国际化或快捷键提示再统一抽象。
    label: &'static str,
}

/// 顶部工具栏按钮列表。
///
/// 业务意图：
/// - “加载日志”使用 `FileText`，表达日志文本文件入口。
/// - “智能诊断”使用 `Stethoscope`，表达对日志问题进行诊断和定位，比脑回路图标更贴近按钮语义。
/// - “设置”使用 `Settings`，表达配置入口。
///
/// 边界条件：
/// - 当前图标依赖启动时注册的 Lucide 字体；如果字体注册失败，启动阶段会直接暴露错误。
const TOOLBAR_ACTIONS: &[ToolbarAction] = &[
    ToolbarAction {
        icon: Icon::FileText,
        label: "加载日志",
    },
    ToolbarAction {
        icon: Icon::Stethoscope,
        label: "智能诊断",
    },
    ToolbarAction {
        icon: Icon::Settings,
        label: "设置",
    },
];

/// 已加载日志目录树在 UI 层的交互状态。
///
/// 业务意图：
/// - `LoadedLogTree` 是加载模块输出的完整树数据，不能直接混入展开/收起这种界面会话状态。
/// - 这里集中保存展开节点集合和可见行缓存，让目录树可以支持折叠，同时为虚拟列表提供稳定数据源。
///
/// 边界条件：
/// - 展开集合只保存当前加载结果内部的节点 ID，重新加载后必须重新初始化，不能跨加载复用。
/// - 可见行缓存只包含当前应该渲染的行，不影响完整树数据；节点点击读取正文时仍应回到完整树来源模型。
struct LoadedLogTreeState {
    /// 加载层生成的完整目录树。
    ///
    /// 业务意图：
    /// - 完整树用于在用户切换展开状态时重新计算可见行。
    /// - 后续接入节点点击时，也应从完整树继续补充来源定位信息，而不是只依赖可见行。
    tree: LoadedLogTree,

    /// 当前处于展开状态的节点 ID 集合。
    ///
    /// 业务意图：
    /// - 使用集合可以让展开/收起切换保持 O(1) 查询，避免滚动渲染时反复线性查找。
    /// - 只记录展开节点；未出现的可展开节点视为收起。
    expanded_node_ids: HashSet<usize>,

    /// 当前需要展示给目录树虚拟列表的可见行。
    ///
    /// 业务意图：
    /// - 文件很多时不能把完整树每一行都渲染成 GPUI 元素，否则滚动会卡顿。
    /// - 先按展开状态算出可见行，再交给 `uniform_list` 按可视区间懒渲染。
    visible_rows: Vec<LoadedLogTreeRow>,
}

impl LoadedLogTreeState {
    /// 根据完整加载结果创建 UI 交互状态。
    ///
    /// 业务意图：
    /// - 首次加载时按用户要求默认展开 2 级目录，避免大目录一次性完全展开导致首屏过载。
    /// - 初始化后立即构建可见行缓存，保证标题摘要、虚拟列表数量和点击状态同步。
    fn new(tree: LoadedLogTree) -> Self {
        let expanded_node_ids = tree
            .rows
            .iter()
            .filter(|row| row.has_children && row.depth < LOG_TREE_DEFAULT_EXPANDED_DEPTH)
            .map(|row| row.id)
            .collect();

        let mut state = Self {
            tree,
            expanded_node_ids,
            visible_rows: Vec::new(),
        };
        state.rebuild_visible_rows();
        state
    }

    /// 返回标题区域展示的加载摘要。
    ///
    /// 边界条件：
    /// - 摘要来自加载层统计，表达完整树节点数量，而不是当前可见行数量。
    fn summary(&self) -> &str {
        &self.tree.summary
    }

    /// 判断指定节点是否处于展开状态。
    ///
    /// 业务意图：
    /// - 渲染可见行时需要根据该状态选择向右或向下的展开箭头。
    fn is_expanded(&self, node_id: usize) -> bool {
        self.expanded_node_ids.contains(&node_id)
    }

    /// 如果指定压缩包节点内部只有一个可打开文件，则返回该文件来源。
    ///
    /// 业务意图：
    /// - 用户从左侧树点击压缩包文件时，如果压缩包内部只有一个日志文件，直接打开正文比先展开再点文件更符合预期。
    /// - 该规则只在 UI 交互层生效，不改变加载层的目录树结构，后续仍可以展示完整压缩包内容。
    ///
    /// 边界条件：
    /// - 只有 `Archive` 节点会触发该规则；普通目录即使只有一个文件也保持展开/收起行为。
    /// - “只有一个文件”按可打开的文件节点统计，目录节点不计数；如果发现两个及以上文件则返回 `None`。
    /// - 错误节点和符号链接不作为可打开日志来源，避免把不可读条目误当作候选文件。
    fn single_file_source_for_archive(&self, node_id: usize) -> Option<LogFileSource> {
        let archive_index = self
            .tree
            .rows
            .iter()
            .position(|row| row.id == node_id && row.kind == LogTreeEntryKind::Archive)?;
        let archive_depth = self.tree.rows[archive_index].depth;
        let mut single_source: Option<LogFileSource> = None;

        for row in self.tree.rows.iter().skip(archive_index + 1) {
            if row.depth <= archive_depth {
                break;
            }

            if row.kind != LogTreeEntryKind::File {
                continue;
            }

            let source = row.source.clone()?;
            if single_source.replace(source).is_some() {
                return None;
            }
        }

        single_source
    }

    /// 切换某个可展开节点的展开状态。
    ///
    /// 业务意图：
    /// - 文件夹点击后需要立即展开或收起子树，并刷新虚拟列表的可见行数量。
    ///
    /// 边界条件：
    /// - 非文件夹或没有子节点的节点不能进入展开集合，避免 UI 状态出现无效 ID。
    /// - 如果传入的 ID 不属于当前树，函数保持静默，避免异步加载切换后旧事件导致崩溃。
    fn toggle_node(&mut self, node_id: usize) {
        let Some(row) = self.tree.rows.iter().find(|row| row.id == node_id) else {
            return;
        };

        if !row.has_children {
            return;
        }

        if !self.expanded_node_ids.remove(&node_id) {
            self.expanded_node_ids.insert(node_id);
        }

        self.rebuild_visible_rows();
    }

    /// 重新按展开状态计算虚拟列表可见行。
    ///
    /// 业务意图：
    /// - 完整目录树是前序扁平结构，收起某个节点时，其后连续的更深层级行都应该隐藏。
    /// - 使用一次线性扫描构建缓存，避免每次虚拟列表渲染可见范围时都重复计算父节点展开链。
    ///
    /// 边界条件：
    /// - 收起父节点后，即使子节点 ID 仍在展开集合中也不会显示；再次展开父节点时会恢复子节点原状态。
    /// - 错误节点、文件节点和符号链接没有子节点，不参与展开控制。
    fn rebuild_visible_rows(&mut self) {
        self.visible_rows.clear();

        let mut collapsed_depth: Option<usize> = None;
        for row in &self.tree.rows {
            if let Some(depth) = collapsed_depth {
                if row.depth > depth {
                    continue;
                }

                collapsed_depth = None;
            }

            self.visible_rows.push(row.clone());

            if row.has_children && !self.expanded_node_ids.contains(&row.id) {
                collapsed_depth = Some(row.depth);
            }
        }
    }
}

/// 左侧日志目录树当前展示的数据状态。
///
/// 业务意图：
/// - 初始状态不展示左侧目录树，让主内容区完整显示“需要先加载日志”的提示。
/// - 加载中、加载成功和加载失败都由同一个枚举表达，避免 UI 层使用多个布尔字段组合出非法状态。
///
/// 边界条件：
/// - 当前不保留历史加载结果；用户再次选择来源时，新结果会替换旧目录树。
/// - 当前不支持取消正在进行的扫描任务，后续大目录扫描若需要取消必须补充任务句柄和状态规则。
enum LogTreeLoadState {
    /// 尚未加载真实来源。
    Empty,
    /// 已经打开系统选择器并开始扫描用户选择的来源。
    Loading {
        /// 展示给用户的加载说明。
        ///
        /// 业务意图：
        /// - 说明当前是在扫描真实来源，而不是应用卡死或仍处于未加载状态。
        message: String,
    },
    /// 真实日志来源已经扫描完成，并已经构建 UI 交互状态。
    Loaded(LoadedLogTreeState),
    /// 加载流程发生致命错误，无法形成可展示结果。
    Failed {
        /// 中文错误说明。
        ///
        /// 边界条件：
        /// - 局部权限错误会作为树节点展示，只有系统对话框失败或加载流程整体失败才进入该状态。
        message: String,
    },
}

/// 右侧已经打开的日志 tab。
///
/// 业务意图：
/// - 每个日志文件对应一个 tab，tab 内保留来源、原始字节、编码选择和渲染状态。
/// - 同一来源重复点击时通过 `source_key` 定位已有 tab，避免重复打开同一份日志。
///
/// 边界条件：
/// - tab 状态只存在于当前进程内，不持久化到配置文件。
/// - 后台读取任务完成时如果 tab 已被关闭，会按 `id` 查找失败并静默忽略。
struct OpenLogTab {
    /// 当前进程内唯一 tab ID。
    id: usize,
    /// 日志文件来源。
    source: LogFileSource,
    /// 用于去重的稳定来源键。
    source_key: String,
    /// tab 上展示的短标题。
    title: String,
    /// 用户当前选择的编码策略。
    encoding_choice: EncodingChoice,
    /// 已读取的原始字节。
    ///
    /// 业务意图：
    /// - 自动检测失败时仍保留原始字节，用户手动切换编码可以直接重新解码。
    /// - 读取失败时保持 `None`，因为没有可复用的正文数据。
    raw_bytes: Option<Arc<Vec<u8>>>,
    /// tab 当前正文状态。
    state: LogTabState,
    /// 当前 tab 的日志正文虚拟列表滚动句柄。
    ///
    /// 业务意图：
    /// - 每个 tab 独立保存滚动上下文，切换 tab 时不会把其它文件的滚动位置混进来。
    scroll_handle: UniformListScrollHandle,
    /// 打开后需要滚动定位的目标行。
    ///
    /// 业务意图：
    /// - 搜索结果点击可能打开一个尚未读取完成的新 tab，目标行必须暂存到 tab 上，等后台解码成功后再滚动。
    /// - 使用 0 基行号与 `DecodedLogDocument.lines` 下标保持一致，避免 UI 展示行号和数据下标混用。
    pending_scroll_to_line: Option<usize>,
    /// 最近一次通过搜索结果跳转的命中行。
    ///
    /// 业务意图：
    /// - 点击搜索结果后不仅要滚动到目标位置，还要用背景色标记命中行，避免用户在密集日志中丢失上下文。
    /// - 该状态只属于当前 tab 的临时视觉反馈，不持久化，也不影响日志语法高亮。
    highlighted_search_line: Option<usize>,
    /// 当前 tab 内日志正文的只读文本选择范围。
    ///
    /// 业务意图：
    /// - 日志正文采用虚拟列表自绘，不是系统文本控件，GPUI 不会自动保存跨行选择状态。
    /// - 将选择范围绑定到 tab，可以让用户切换 tab 后仍保留当前文件的选择上下文，并支持复制和填入搜索框。
    ///
    /// 边界条件：
    /// - 选择范围使用行号和字符列，不使用 UTF-8 字节下标；复制时再转换为安全字节边界，避免中文被截断。
    /// - 重新解码、重新加载或打开失败时必须清理该字段，因为旧行列可能不再对应新文档。
    text_selection: Option<LogTextSelection>,
    /// 当前正在拖拽选择时的固定起点。
    ///
    /// 业务意图：
    /// - 鼠标按下确定锚点，后续移动只更新焦点，才能正确支持从下往上或从右往左反向选择。
    /// - 鼠标释放后清空该临时字段，但保留 `text_selection` 供复制和搜索预填使用。
    selection_drag_anchor: Option<LogTextPosition>,
}

/// 日志正文中的文本位置。
///
/// 业务意图：
/// - 自绘日志行没有系统 selection，因此需要一个轻量位置类型表达“第几行第几个字符”。
/// - `column` 使用 Unicode 字符序号而不是字节序号，保证中文、全角符号和 emoji 不会被拆成非法 UTF-8。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct LogTextPosition {
    /// 0 基日志行号，对应 `DecodedLogDocument.lines` 下标。
    line_index: usize,
    /// 0 基字符列，对应该行 `chars()` 序号。
    column: usize,
}

/// 日志正文只读选择范围。
///
/// 业务意图：
/// - `anchor` 表示鼠标按下位置，`focus` 表示当前拖拽位置；二者顺序不固定。
/// - 复制、渲染选区和填充搜索框前都通过 `normalized` 统一为从前到后的范围。
#[derive(Clone, Debug, PartialEq, Eq)]
struct LogTextSelection {
    /// 选择起始锚点。
    anchor: LogTextPosition,
    /// 选择当前焦点。
    focus: LogTextPosition,
}

impl LogTextSelection {
    /// 返回按文档顺序排列后的选择范围端点。
    ///
    /// 边界条件：
    /// - 用户可能从后往前拖选，因此不能假设 `anchor <= focus`。
    fn normalized(&self) -> (LogTextPosition, LogTextPosition) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// 判断当前选择是否为空。
    fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }
}

/// 单行日志渲染所需的输入数据。
///
/// 业务意图：
/// - 日志行渲染需要同时知道 tab、行号、正文、高亮、行号列和搜索定位状态；集中成结构体可以避免函数参数膨胀。
/// - 该结构只在当前 UI 帧内短暂使用，不保存到 tab 状态，避免复制日志正文或高亮状态的生命周期变复杂。
struct LogLineRenderData {
    /// 所属 tab ID，用于鼠标选择事件回写到正确 tab。
    tab_id: usize,
    /// 0 基日志行号。
    line_index: usize,
    /// 当前行正文。
    line: String,
    /// 当前行语法高亮与选区高亮范围，范围必须是 UTF-8 字节边界。
    highlights: Vec<(Range<usize>, gpui::HighlightStyle)>,
    /// 行号列宽度。
    line_number_width: f32,
    /// 横向滚动时行号列的反向补偿偏移。
    horizontal_line_number_offset: Pixels,
    /// 当前行是否是搜索结果跳转后的目标行。
    search_highlighted: bool,
}

/// 日志正文自绘滚动条的方向。
///
/// 业务意图：
/// - 右侧日志查看器需要同时支持纵向和横向滚动条拖动，两者共享拖动生命周期，但坐标轴和偏移字段不同。
/// - 使用枚举明确区分方向，避免在拖动计算中通过布尔值表达业务含义。
#[derive(Clone, Copy)]
enum LogScrollbarAxis {
    /// 控制日志正文上下滚动。
    Vertical,
    /// 控制日志正文左右滚动。
    Horizontal,
}

/// 日志正文滚动条正在被拖动时的临时状态。
///
/// 业务意图：
/// - 鼠标按下滑块后，后续移动事件需要知道拖动的是哪个 tab、哪个方向，以及鼠标按下点在滑块内的偏移。
/// - 保存滑块内偏移可以避免拖动开始瞬间滑块跳动，符合原生滚动条的交互预期。
///
/// 边界条件：
/// - 该状态只存在于鼠标左键拖动期间；关闭 tab、释放鼠标或来源重新加载时都会被清理。
#[derive(Clone, Copy)]
struct LogScrollbarDrag {
    /// 正在拖动滚动条的 tab ID。
    tab_id: usize,
    /// 当前拖动的滚动条方向。
    axis: LogScrollbarAxis,
    /// 鼠标按下点相对滑块起点的偏移。
    cursor_offset: Pixels,
}

/// 日志正文自绘滚动条的布局测量结果。
///
/// 业务意图：
/// - 滑块渲染、按下命中和拖动换算都必须使用同一套轨道与滑块尺寸，避免视觉位置和实际滚动位置不一致。
/// - 坐标均为相对日志列表视口左上角的局部坐标，调用方再根据方向映射到窗口坐标。
#[derive(Clone, Copy)]
struct LogScrollbarMetrics {
    /// 滑块起点在当前轴向上的局部坐标。
    thumb_start: Pixels,
    /// 滑块在当前轴向上的长度。
    thumb_length: Pixels,
    /// 轨道起点在当前轴向上的局部坐标。
    track_start: Pixels,
    /// 轨道在当前轴向上的总长度。
    track_length: Pixels,
    /// 当前轴向可滚动的最大距离。
    max_scroll: Pixels,
}

/// 左侧目录树滚动条正在被拖动时的临时状态。
///
/// 业务意图：
/// - 左侧树和右侧日志正文都使用虚拟列表，但左侧树没有 tab 维度，因此用独立状态记录滑块内鼠标偏移。
/// - 保存偏移可以避免拖动开始时滑块跳到鼠标中心，交互行为更接近系统滚动条。
///
/// 边界条件：
/// - 该状态只在鼠标左键拖动期间有效；重新加载日志、鼠标释放或列表测量失效时都会清空。
#[derive(Clone, Copy)]
struct LogTreeScrollbarDrag {
    /// 鼠标按下点相对滑块顶部的偏移。
    cursor_offset: Pixels,
}

/// 搜索对话框中的文本输入槽位。
///
/// 业务意图：
/// - 搜索对话框现在包含“关键字”和“目录目标”两个可编辑文本框，二者都需要走 GPUI 平台输入协议以支持中文 IME。
/// - 用枚举标识当前编辑槽位，可以复用同一套 UTF-16/UTF-8 范围转换和组合文本替换逻辑。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchTextInputKind {
    /// 搜索关键字输入框。
    Query,
    /// 当前目录搜索的目标目录输入框。
    DirectoryTarget,
}

/// 搜索对话框的交互状态。
///
/// 业务意图：
/// - `Ctrl+F` 打开后，对话框需要保存查询词、搜索范围、大小写策略和实时进度。
/// - 该状态只存在于当前会话，不写入配置文件，避免在搜索历史和隐私规则未定义前持久化用户输入。
///
/// 边界条件：
/// - 关闭对话框会取消当前搜索任务，但不会主动清空结果面板，方便用户继续查看上一次结果。
#[derive(Clone)]
struct SearchDialogState {
    /// 查询词。
    ///
    /// 业务意图：
    /// - 当前只支持单行普通文本搜索，输入中的换行会被忽略。
    query: String,
    /// 搜索输入框的当前选择范围，使用 UTF-8 字节下标。
    ///
    /// 业务意图：
    /// - GPUI 输入协议对外使用 UTF-16 下标，但内部查询词和 `String` 操作必须使用 UTF-8 字节边界。
    /// - 第一版搜索框不支持鼠标选择文本，选择范围通常是光标位置；仍保留范围字段以兼容 IME 替换组合文本。
    selection_range: Range<usize>,
    /// 中文、日文等输入法正在组合的文本范围，使用 UTF-8 字节下标。
    ///
    /// 边界条件：
    /// - 组合完成后必须清空该范围，否则后续普通输入会错误替换旧组合文本。
    marked_range: Option<Range<usize>>,
    /// 搜索范围。
    scope: SearchScope,
    /// 当前目录搜索的目标目录文本。
    ///
    /// 业务意图：
    /// - 选择“当前目录”时，需要把实际搜索目标展示给用户，允许用户缩小到子目录或修改为加载树中的其它目录片段。
    /// - 该字段只用于过滤已经加载树中收集到的可搜索来源，不会扩大文件系统访问范围。
    directory_target: String,
    /// 目录目标输入框的选择范围，使用 UTF-8 字节下标。
    directory_selection_range: Range<usize>,
    /// 目录目标输入框的 IME 组合文本范围，使用 UTF-8 字节下标。
    directory_marked_range: Option<Range<usize>>,
    /// 是否区分大小写。
    case_sensitive: bool,
    /// 对话框左上角在窗口内容坐标中的横坐标。
    x: f32,
    /// 对话框左上角在窗口内容坐标中的纵坐标。
    y: f32,
    /// 当前是否有后台搜索任务仍在运行。
    is_searching: bool,
    /// 当前搜索任务的进度快照。
    progress: SearchProgress,
    /// 搜索状态提示。
    message: String,
    /// 对话框绑定的任务 ID。
    ///
    /// 业务意图：
    /// - 用户快速连续搜索时，旧后台任务可能晚于新任务返回；任务 ID 用于丢弃过期更新。
    job_id: usize,
}

/// 搜索对话框拖动状态。
///
/// 业务意图：
/// - 对话框可以自由拖动，拖动时需要保存鼠标按下点和对话框左上角之间的偏移，避免按下瞬间跳动。
#[derive(Clone, Copy)]
struct SearchDialogDrag {
    /// 鼠标按下点相对对话框左上角的横向偏移。
    cursor_offset_x: Pixels,
    /// 鼠标按下点相对对话框左上角的纵向偏移。
    cursor_offset_y: Pixels,
}

/// 单次搜索历史记录。
///
/// 业务意图：
/// - 用户可能连续尝试多个关键字或范围，底部面板需要保留每一次搜索作为可展开记录，避免新搜索覆盖旧上下文。
/// - 每条记录独立保存进度、命中和错误，后台任务回调只更新对应 `job_id` 的记录。
#[derive(Clone)]
struct SearchHistoryRecord {
    /// 后台任务 ID。
    job_id: usize,
    /// 本次搜索使用的查询词。
    query: String,
    /// 本次搜索范围。
    scope: SearchScope,
    /// 当前目录搜索的目标目录；当前文件搜索为 `None`。
    directory_target: Option<String>,
    /// 本次搜索是否区分大小写。
    case_sensitive: bool,
    /// 搜索进度终态或当前进度。
    progress: SearchProgress,
    /// 搜索命中结果。
    results: Vec<SearchResultItem>,
    /// 单文件读取或解码失败列表。
    errors: Vec<SearchFileError>,
    /// 当前记录是否来自被用户取消的搜索任务。
    canceled: bool,
    /// 记录是否展开显示明细。
    expanded: bool,
    /// 当前记录中已经展开的文件分组来源键。
    ///
    /// 业务意图：
    /// - 历史记录只负责“这次搜索是否展开”，文件分组负责“某个文件内结果是否展开”。
    /// - 使用来源稳定键而不是文件名，避免同名文件或压缩包内同名成员互相影响展开状态。
    expanded_file_keys: HashSet<String>,
}

impl SearchHistoryRecord {
    /// 返回当前记录的状态文案。
    ///
    /// 边界条件：
    /// - 取消状态优先于进度判断，否则关闭对话框后旧任务回调被丢弃，记录会长期误显示为“搜索中”。
    fn state_label(&self) -> &'static str {
        if self.canceled {
            "已取消"
        } else if self.progress.searched_files < self.progress.total_files {
            "搜索中"
        } else {
            "完成"
        }
    }
}

/// 搜索结果面板虚拟列表行。
///
/// 业务意图：
/// - 面板需要同时展示搜索历史记录头、命中明细、错误明细和空态说明。
/// - 统一展平成固定高度行后仍可继续使用 `uniform_list`，避免大量结果时重新引入滚动卡顿。
#[derive(Clone)]
enum SearchResultsPanelRow {
    /// 一条搜索历史记录的摘要行。
    RecordHeader { record_index: usize },
    /// 一条文件分组摘要行。
    FileHeader {
        /// 所属历史记录下标。
        record_index: usize,
        /// 文件来源稳定键。
        source_key: String,
        /// 文件完整展示路径。
        ///
        /// 业务意图：
        /// - 文件分组行渲染时直接使用缓存的展示文本，避免滚动可见行时重新扫描该搜索记录的全部命中。
        full_path: String,
        /// 当前文件内命中数量。
        ///
        /// 业务意图：
        /// - 该数量在展平行时已经可以确定，缓存到行模型中可以让虚拟列表渲染保持 O(可见行数)。
        result_count: usize,
    },
    /// 一条搜索命中明细行。
    Result {
        /// 所属历史记录下标。
        record_index: usize,
        /// 命中结果下标。
        result_index: usize,
    },
    /// 一条文件级错误明细行。
    Error {
        /// 所属历史记录下标。
        record_index: usize,
        /// 错误下标。
        error_index: usize,
    },
    /// 展开记录但暂无命中或错误时显示的说明行。
    Empty { record_index: usize },
}

/// 搜索历史记录内的文件分组。
///
/// 业务意图：
/// - 同一个文件可能有大量命中，结果面板需要先按文件归类，再让用户按需展开某个文件。
/// - 分组只保存命中结果在 `record.results` 中的下标，避免复制整条命中记录导致内存上涨。
#[derive(Clone)]
struct SearchResultFileGroup {
    /// 文件来源稳定键。
    source_key: String,
    /// 文件完整展示路径。
    ///
    /// 业务意图：
    /// - 结果面板按文件分组时，用户需要直接看到完整路径判断命中来源，避免文件名相同但目录不同造成误判。
    full_path: String,
    /// 当前文件内的命中结果下标。
    result_indices: Vec<usize>,
}

/// 搜索结果面板状态。
///
/// 业务意图：
/// - 面板从右侧内容区底部弹出，保存搜索历史记录和高度。
/// - 结果列表使用虚拟列表滚动句柄，避免大量命中时滚动卡顿。
#[derive(Clone)]
struct SearchResultsPanelState {
    /// 搜索历史记录，按发起时间从旧到新排列。
    records: Vec<SearchHistoryRecord>,
    /// 已展平的虚拟列表行缓存。
    ///
    /// 业务意图：
    /// - 搜索结果可能包含成千上万条命中，滚动时不能每一帧都重新展平全部历史记录和文件分组。
    /// - 该缓存只在记录增删、命中追加或展开状态改变时重建，渲染时按可见 range 直接取片段。
    rows: Vec<SearchResultsPanelRow>,
    /// 面板当前高度。
    height: f32,
    /// 结果虚拟列表滚动句柄。
    scroll_handle: UniformListScrollHandle,
}

/// 搜索结果面板高度拖动状态。
///
/// 业务意图：
/// - 用户按住面板顶部拖拽条上下拖动时，保存拖动开始点和开始高度，后续移动可稳定换算新高度。
#[derive(Clone, Copy)]
struct SearchResultsResizeDrag {
    /// 拖动开始时鼠标在窗口内容坐标中的纵坐标。
    start_y: Pixels,
    /// 拖动开始时结果面板高度。
    start_height: f32,
}

/// 一次搜索任务的后台输入。
///
/// 业务意图：
/// - 当前文件搜索复用已解码行；当前目录搜索复用加载树中收集到的文件来源。
/// - 使用枚举可以在启动任务前完成所有 UI 状态校验，后台逻辑只处理明确输入。
enum SearchTarget {
    /// 搜索当前文件。
    CurrentFile {
        /// 当前文件来源。
        source: LogFileSource,
        /// 当前文件已解码行。
        ///
        /// 业务意图：
        /// - 搜索大文件时不能在 UI 线程克隆整份日志，使用 `Arc` 共享解码结果，让后台任务只增加引用计数。
        lines: Arc<Vec<String>>,
    },
    /// 搜索当前目录递归来源。
    CurrentDirectory {
        /// 当前目录下所有可打开文件来源。
        sources: Vec<LogFileSource>,
    },
}

/// 搜索输入框的 IME 注册元素。
///
/// 业务意图：
/// - GPUI 的中文输入法、候选词窗口和组合文本必须通过 `Window::handle_input` 接入平台输入系统。
/// - 普通 `div().on_key_down(...)` 只能处理按键事件，无法可靠接收 macOS/Windows IME 提交文本。
///
/// 实现原因：
/// - 该元素不绘制任何像素，只在 paint 阶段把当前搜索框的边界和 `MainView` 输入处理器注册给窗口。
/// - 可见输入框仍由外层 `div` 渲染，避免为了输入协议重写整套搜索框视觉样式。
struct SearchInputImeElement {
    /// 主视图实体，用于把平台输入回写到搜索对话框状态。
    view: Entity<MainView>,
    /// 搜索输入框焦点句柄，只有该焦点激活时平台才会把文本输入发送给本元素。
    focus_handle: gpui::FocusHandle,
}

impl IntoElement for SearchInputImeElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SearchInputImeElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        context: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], context), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _context: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        context: &mut App,
    ) {
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            context,
        );
    }
}

/// 单个日志 tab 的正文状态。
///
/// 业务意图：
/// - 读取、解码和失败状态需要在 UI 中明确区分，避免右侧空白让用户误以为点击无效。
enum LogTabState {
    /// 正在读取或重新解码。
    Loading {
        /// 展示给用户的中文状态。
        message: String,
    },
    /// 已成功解码。
    Ready {
        /// 可供右侧虚拟列表渲染的日志文档。
        document: DecodedLogDocument,
    },
    /// 读取或解码失败。
    Failed {
        /// 中文错误说明。
        message: String,
    },
}

/// 日志 tab 读取完成后回到 UI 线程的数据。
///
/// 业务意图：
/// - 后台任务不能直接修改 GPUI 状态，必须把结果封装后在 `view.update` 中合并。
enum LogTabLoadResult {
    /// 读取和自动解码都成功。
    Ready {
        /// 原始字节，用于后续手动切换编码。
        raw_bytes: Arc<Vec<u8>>,
        /// 自动解码后的文档。
        document: DecodedLogDocument,
    },
    /// 原始字节读取成功，但自动检测或解码失败。
    DecodeFailed {
        /// 原始字节仍需保留，用户可以手动选择编码重新解析。
        raw_bytes: Arc<Vec<u8>>,
        /// 中文错误说明。
        message: String,
    },
    /// 原始字节读取失败。
    ReadFailed {
        /// 中文错误说明。
        message: String,
    },
}

/// 日志 tab 重新解码完成后的结果。
///
/// 业务意图：
/// - 手动切换编码只需要更新文档状态，不需要替换原始字节。
enum LogTabDecodeResult {
    /// 解码成功。
    Ready {
        /// 触发该后台任务时用户选择的编码策略。
        ///
        /// 业务意图：
        /// - 用户快速连续切换编码时，较早的后台任务可能晚返回；合并阶段需要用该字段识别并丢弃过期结果。
        encoding_choice: EncodingChoice,
        /// 新编码下的日志文档。
        document: DecodedLogDocument,
    },
    /// 解码失败。
    Failed {
        /// 触发该后台任务时用户选择的编码策略。
        ///
        /// 边界条件：
        /// - 如果该值已经不是 tab 当前选择，说明用户又切换了编码，旧错误不能覆盖新的加载状态。
        encoding_choice: EncodingChoice,
        /// 中文错误说明。
        message: String,
    },
}

/// tab 右键菜单状态。
///
/// 业务意图：
/// - GPUI 0.2.2 的 `show_window_menu` 是系统窗口菜单，不适合作为 tab 操作菜单。
/// - 因此这里保存右键位置并用 GPUI 元素自绘一个轻量菜单。
struct TabContextMenu {
    /// 菜单针对的 tab ID。
    tab_id: usize,
    /// 菜单左上角的窗口内容区横坐标。
    x: f32,
    /// 菜单左上角的窗口内容区纵坐标。
    y: f32,
}

/// 编码选择下拉框状态。
///
/// 业务意图：
/// - 用户要求编码选择不要平铺在顶部，因此这里把当前打开的编码菜单作为独立弹层状态保存。
/// - 菜单只绑定一个 tab，切换 tab、关闭 tab 或重新加载日志时都会收起，避免对旧 tab 继续操作。
struct EncodingDropdownMenu {
    /// 下拉框所属的 tab ID。
    tab_id: usize,
    /// 菜单左上角相对右侧日志工作区的横坐标。
    ///
    /// 业务意图：
    /// - 编码选择器已经移动到右侧状态栏，按钮位置会随状态文本宽度变化，不能继续使用固定左边距。
    x: f32,
    /// 菜单左上角相对右侧日志工作区的纵坐标。
    y: f32,
}

/// tab 右键菜单命令。
///
/// 业务意图：
/// - 菜单项直接对应用户要求的三个关闭操作，避免 UI 文案和处理逻辑分散。
#[derive(Clone, Copy)]
enum TabContextMenuAction {
    /// 关闭被右键点击的 tab。
    Current,
    /// 关闭除被右键点击 tab 之外的所有 tab。
    OtherTabs,
    /// 关闭所有 tab。
    AllTabs,
}

/// 左侧目录树单行渲染所需的输入数据。
///
/// 业务意图：
/// - 目录树行需要同时表达层级、图标、元信息和点击行为，字段较多；集中成结构体可以保持渲染函数签名稳定。
/// - 该结构只服务 UI 渲染，不回写加载层，也不作为文件来源持久化模型。
struct LogTreeRowRenderData {
    /// 当前加载结果内部的节点 ID，用于展开状态和元素稳定标识。
    node_id: usize,
    /// 当前节点在树中的深度，用于计算左侧缩进。
    depth: usize,
    /// 展开箭头图标；没有子节点时为 `None`，仍会保留占位宽度。
    expand_icon: Option<Icon>,
    /// 节点类型图标。
    item_icon: Icon,
    /// 节点类型图标颜色。
    icon_color: u32,
    /// 当前行是否可以展开或收起。
    can_toggle: bool,
    /// 当前行是否绑定日志正文来源；存在时点击打开右侧 tab。
    source: Option<LogFileSource>,
    /// 当前行展示文本。
    label: String,
    /// 当前行右侧短元信息。
    meta: Option<String>,
}

/// 用户通过“加载日志”按钮选择的路径来源类型。
///
/// 业务意图：
/// - “加载日志”按钮现在直接打开系统选择器，减少点击后无反馈的歧义。
/// - 枚举保留路径选择配置入口，后续如需区分平台或补充 Windows 专用目录选择策略时不影响调用方。
#[derive(Clone, Copy)]
enum LoadPromptKind {
    /// 选择普通文件、目录或压缩包来源。
    LogSources,
}

impl LoadPromptKind {
    /// 为 GPUI 系统路径选择器生成配置。
    ///
    /// 业务意图：
    /// - `files` 和 `directories` 同时开启，让 macOS 能像常见桌面工具一样在一次对话框里选择文件或目录。
    /// - `multiple` 保持为 `true`，允许用户一次加载多个文件、目录或压缩包来源。
    ///
    /// 边界条件：
    /// - GPUI 0.2.2 的 `PathPromptOptions` 不提供扩展名过滤字段，因此压缩包类型由加载层识别。
    /// - GPUI 0.2.2 的 Windows 后端对混选支持有限，后续 Windows 真机验收时如不满足需要补专用文件/目录入口。
    fn to_prompt_options(self) -> PathPromptOptions {
        match self {
            Self::LogSources => PathPromptOptions {
                files: true,
                directories: true,
                multiple: true,
                prompt: Some("选择日志来源".into()),
            },
        }
    }

    /// 返回进入加载状态时展示的中文说明。
    ///
    /// 边界条件：
    /// - 当前文案只用于左侧目录树区域，不作为错误判断依据。
    fn loading_message(self) -> &'static str {
        match self {
            Self::LogSources => "正在扫描已选择的日志来源...",
        }
    }
}

/// 主窗口根视图。
///
/// 业务意图：
/// - 作为 GPUI 窗口的根实体，当前承载顶部工具栏、左右分栏和左侧日志目录树。
/// - 后续日志列表、状态栏、设置面板等 UI 应从这个根视图向下扩展，而不是直接写在
///   `main` 函数里，从而保持启动流程和渲染逻辑分离。
///
/// 边界条件：
/// - 当前视图只在用户点击“加载日志”并完成系统选择器确认后读取路径。
/// - 当前视图不保存持久状态，因此不会产生跨平台配置写入差异。
struct MainView {
    /// 左侧内容区域当前宽度。
    ///
    /// 业务意图：
    /// - 保存用户在当前进程内拖动分割线后的结果，让左右栏布局能即时响应。
    /// - 初始值来自 `LEFT_PANEL_DEFAULT_WIDTH`，严格满足默认左侧 300px 的需求。
    ///
    /// 边界条件：
    /// - 当前宽度不写入磁盘；关闭应用后会恢复默认值。
    /// - 更新宽度时必须经过最小宽度约束，避免左右任一区域被拖到不可用。
    left_panel_width: f32,

    /// 当前是否处于拖动分割线状态。
    ///
    /// 业务意图：
    /// - 区分普通鼠标移动和调整分栏宽度，避免鼠标经过内容区域时误改布局。
    ///
    /// 边界条件：
    /// - 鼠标左键在分割线按下时变为 `true`，在内容区域收到左键释放时恢复为 `false`。
    /// - 当前不处理窗口失焦或鼠标释放发生在窗口外的情况，后续如需更强交互再补充捕获策略。
    is_resizing_splitter: bool,

    /// 左侧日志目录树当前的数据状态。
    ///
    /// 业务意图：
    /// - 保存未加载、加载中、加载成功和加载失败状态，驱动内容区渲染。
    /// - 真实加载结果只保存在内存中，不写入配置或缓存目录。
    load_state: LogTreeLoadState,

    /// 左侧目录树虚拟列表的滚动句柄。
    ///
    /// 业务意图：
    /// - `uniform_list` 需要稳定句柄保存滚动位置和测量结果，否则每次重绘都可能重置滚动上下文。
    /// - 每次重新加载日志后会替换为新句柄，让新目录树从顶部开始展示，避免继承上一棵树的滚动偏移。
    ///
    /// 边界条件：
    /// - 该句柄只服务左侧目录树，不用于右侧日志正文或其它未来列表。
    log_tree_scroll_handle: UniformListScrollHandle,

    /// 右侧 tab 栏横向滚动句柄。
    ///
    /// 业务意图：
    /// - tab 标题需要完整展示，不再省略；打开日志较多时通过横向滚动查看所有 tab。
    /// - 使用 GPUI `ScrollHandle` 保存横向偏移，滚动箭头和鼠标滚动可以共享同一份滚动状态。
    tab_bar_scroll_handle: ScrollHandle,

    /// 右侧已经打开的日志 tab 列表。
    ///
    /// 业务意图：
    /// - 每个日志文件一个 tab，保存读取状态、编码选择和滚动句柄。
    /// - Vec 顺序就是 tab 栏展示顺序，第一版不支持拖拽重排。
    open_tabs: Vec<OpenLogTab>,

    /// 当前激活的日志 tab ID。
    ///
    /// 边界条件：
    /// - 没有打开任何 tab 时为 `None`，右侧展示“点击左侧日志文件查看内容”的提示。
    /// - 关闭当前 tab 后会自动切换到剩余 tab 中靠近当前位置的一个。
    active_tab_id: Option<usize>,

    /// 下一个待分配的 tab ID。
    ///
    /// 业务意图：
    /// - 使用单调递增 ID 避免 Vec 下标在关闭 tab 后失效，后台任务也能安全定位目标 tab。
    next_tab_id: usize,

    /// 当前打开的 tab 右键菜单。
    ///
    /// 边界条件：
    /// - 菜单只在当前窗口内显示，不跨 tab 或跨加载持久化。
    tab_context_menu: Option<TabContextMenu>,

    /// 当前打开的编码下拉框。
    ///
    /// 业务意图：
    /// - 编码切换控件从平铺按钮改为下拉框后，需要记录哪个 tab 的菜单处于展开状态。
    /// - 该状态只影响当前 UI 帧，不持久化，也不参与日志内容解码结果。
    encoding_dropdown_menu: Option<EncodingDropdownMenu>,

    /// 日志正文自绘滚动条的拖动状态。
    ///
    /// 业务意图：
    /// - GPUI `uniform_list` 本身负责滚轮滚动和虚拟渲染，但当前 UI 需要可见滚动条支持鼠标拖动。
    /// - 该状态把鼠标拖动生命周期和具体 tab 绑定，避免切换 tab 后继续写入旧日志视图的滚动偏移。
    ///
    /// 边界条件：
    /// - 只支持鼠标左键拖动滑块；点击轨道跳转、键盘滚动和触控条行为后续再按验收标准扩展。
    log_scrollbar_drag: Option<LogScrollbarDrag>,

    /// 左侧目录树自绘滚动条的拖动状态。
    ///
    /// 业务意图：
    /// - 大目录树使用虚拟列表时需要可见滚动条，并支持鼠标拖动定位。
    /// - 该状态只保存拖动过程中的临时偏移，不影响目录树展开状态或加载结果。
    ///
    /// 边界条件：
    /// - 重新加载日志会替换目录树滚动句柄，同时清理该拖动状态，避免旧测量数据继续参与滚动计算。
    log_tree_scrollbar_drag: Option<LogTreeScrollbarDrag>,

    /// 当前搜索对话框状态。
    ///
    /// 业务意图：
    /// - `Ctrl+F` 或 `Cmd+F` 打开对话框；关闭后该字段为 `None`。
    /// - 搜索条件只保留在内存中，不写入磁盘，避免在隐私规则未定义前保存用户查询词。
    search_dialog: Option<SearchDialogState>,

    /// 搜索对话框拖动状态。
    ///
    /// 边界条件：
    /// - 只在鼠标左键按住标题栏期间有效；释放鼠标或关闭对话框时必须清空。
    search_dialog_drag: Option<SearchDialogDrag>,

    /// 搜索结果底部面板状态。
    ///
    /// 业务意图：
    /// - 搜索开始后立即打开结果面板展示进行中状态，完成后保留结果供用户点击定位。
    /// - 关闭面板只隐藏当前结果，不影响搜索对话框中的查询词。
    search_results_panel: Option<SearchResultsPanelState>,

    /// 搜索结果面板高度拖动状态。
    ///
    /// 边界条件：
    /// - 如果用户关闭结果面板或鼠标释放，拖动状态必须清空，避免下一次鼠标移动继续改变高度。
    search_results_resize_drag: Option<SearchResultsResizeDrag>,

    /// 下一个搜索任务 ID。
    ///
    /// 业务意图：
    /// - 后台搜索任务可能乱序返回，使用单调递增 ID 可以丢弃旧任务更新。
    next_search_job_id: usize,

    /// 搜索输入框焦点句柄。
    ///
    /// 业务意图：
    /// - `Ctrl+F` 打开对话框后应立即把键盘输入导向查询框。
    /// - 焦点句柄只服务搜索输入，不和日志正文或目录树共用，避免快捷键和普通输入互相干扰。
    search_input_focus: gpui::FocusHandle,

    /// 当前目录搜索目标输入框焦点句柄。
    ///
    /// 业务意图：
    /// - 目录目标输入框同样需要支持中文路径和 IME 组合输入，必须拥有独立焦点以区分平台输入应该写入哪个字段。
    /// - 不与关键字输入框共用焦点，避免用户编辑目录时误把文本写入查询词。
    search_directory_focus: gpui::FocusHandle,

    /// 全局键盘监听订阅。
    ///
    /// 业务意图：
    /// - GPUI 的全局快捷键监听返回订阅句柄，必须跟随主视图保存，否则订阅被释放后 `Ctrl+F` 将不再生效。
    /// - 该订阅只服务当前窗口生命周期，窗口关闭后随视图释放即可。
    global_keystroke_subscription: Option<gpui::Subscription>,

    /// 主界面根节点焦点句柄。
    ///
    /// 业务意图：
    /// - GPUI 快捷键绑定需要存在稳定的焦点路径，根节点持有焦点后 `Ctrl+F` / `Cmd+F` 可以由 Action 系统可靠分发。
    /// - 搜索对话框关闭后可把焦点还给根节点，避免焦点停在已经隐藏的搜索输入框上导致后续快捷键无响应。
    root_focus_handle: gpui::FocusHandle,
}

impl MainView {
    /// 创建主窗口根视图。
    ///
    /// 业务意图：
    /// - 集中初始化所有首屏 UI 状态，避免在 `main` 的窗口创建回调中散落默认值。
    /// - 左侧栏默认 300px 是用户明确要求，必须从这里作为唯一入口初始化。
    fn new(context: &mut Context<Self>) -> Self {
        Self {
            left_panel_width: LEFT_PANEL_DEFAULT_WIDTH,
            is_resizing_splitter: false,
            load_state: LogTreeLoadState::Empty,
            log_tree_scroll_handle: UniformListScrollHandle::new(),
            tab_bar_scroll_handle: ScrollHandle::new(),
            open_tabs: Vec::new(),
            active_tab_id: None,
            next_tab_id: 1,
            tab_context_menu: None,
            encoding_dropdown_menu: None,
            log_scrollbar_drag: None,
            log_tree_scrollbar_drag: None,
            search_dialog: None,
            search_dialog_drag: None,
            search_results_panel: None,
            search_results_resize_drag: None,
            next_search_job_id: 1,
            search_input_focus: context.focus_handle(),
            search_directory_focus: context.focus_handle(),
            global_keystroke_subscription: None,
            root_focus_handle: context.focus_handle(),
        }
    }

    /// 构建顶部工具栏。
    ///
    /// 业务意图：
    /// - 将全局操作入口集中在窗口顶部，符合日志查看客户端的主要工作流。
    /// - “加载日志”已经接入真实路径选择和目录树扫描；诊断和设置仍只保留入口。
    ///
    /// 边界条件：
    /// - 工具栏内容固定为单行，超窄窗口下的折叠、隐藏或溢出行为尚未定义。
    /// - 诊断和设置按钮点击目前是空操作，后续实现具体功能时必须补充对应业务规则和错误处理。
    fn render_toolbar(&self, context: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(TOOLBAR_HEIGHT))
            .pl(px(LOG_TREE_ROW_HORIZONTAL_PADDING))
            .pr_4()
            .bg(rgb(0xf7f8fa))
            .border_b_1()
            .border_color(rgb(0xe1e4e8))
            .child(self.render_load_toolbar_button(context))
            .children(TOOLBAR_ACTIONS[1..].iter().map(Self::render_toolbar_button))
    }

    /// 构建“加载日志”工具栏按钮。
    ///
    /// 业务意图：
    /// - 该按钮是当前首个真实业务入口，点击后直接打开系统选择器，避免用户误判为没有响应。
    /// - 与其它工具栏按钮保持相同文字按钮样式，避免功能入口因为有状态而产生视觉突兀。
    ///
    /// 边界条件：
    /// - 按钮左内边距为 0，使图标左缘和加载后目录树标题左缘使用同一条基准线。
    /// - 当前通过 GPUI 的路径选择器同时请求文件和目录；Windows 混选能力需在后续真机验收中确认。
    fn render_load_toolbar_button(&self, context: &mut Context<Self>) -> impl IntoElement {
        let action = &TOOLBAR_ACTIONS[0];

        div()
            .id(SharedString::from(action.label))
            .flex()
            .items_center()
            .gap_1()
            .flex_none()
            .pl(px(0.0))
            .pr(px(TOOLBAR_BUTTON_HORIZONTAL_PADDING))
            .py(px(TOOLBAR_BUTTON_VERTICAL_PADDING))
            .text_sm()
            .text_color(rgb(0x24292f))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(|button| button.text_color(rgb(0x0969da)))
            .active(|button| button.opacity(0.82))
            .child(Self::render_lucide_icon(
                Some(action.icon),
                TOOLBAR_BUTTON_ICON_WIDTH,
                TOOLBAR_BUTTON_ICON_SIZE,
                0x57606a,
            ))
            .child(action.label)
            .on_click(context.listener(Self::open_log_sources_prompt))
    }

    /// 构建一个顶部工具栏按钮。
    ///
    /// 业务意图：
    /// - 统一工具栏入口的视觉样式，避免后续每个功能入口自行定义按钮外观。
    /// - 按钮使用“图标 + 中文文字”的文字按钮形态，满足轻量工具栏要求。
    ///
    /// 边界条件：
    /// - 当前按钮没有真实业务动作，点击后不改变状态、不访问文件、不请求网络。
    /// - `id` 使用按钮标签生成，当前三个标签唯一；后续如果允许重复入口，需要改为稳定枚举 ID。
    fn render_toolbar_button(action: &ToolbarAction) -> impl IntoElement {
        div()
            .id(SharedString::from(action.label))
            .flex()
            .items_center()
            .gap_1()
            .flex_none()
            .px(px(TOOLBAR_BUTTON_HORIZONTAL_PADDING))
            .py(px(TOOLBAR_BUTTON_VERTICAL_PADDING))
            .text_sm()
            .text_color(rgb(0x24292f))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(|button| button.text_color(rgb(0x0969da)))
            .active(|button| button.opacity(0.82))
            .child(Self::render_lucide_icon(
                Some(action.icon),
                TOOLBAR_BUTTON_ICON_WIDTH,
                TOOLBAR_BUTTON_ICON_SIZE,
                0x57606a,
            ))
            .child(action.label)
            .on_click(|_event, _window, _context| {
                // 智能诊断和设置当前只要求保留入口，具体业务动作尚未定义。
                // 后续实现时应按 AGENTS.md 先确认权限、数据边界和验收标准，再绑定真实处理逻辑。
            })
    }

    /// 打开日志来源选择器。
    ///
    /// 业务意图：
    /// - 用户点击“加载日志”后必须立即看到系统选择器反馈，避免工具栏按钮看起来没有响应。
    /// - 选择器允许选择文件、目录和压缩包；具体来源类型由加载模块根据路径和元数据判断。
    fn open_log_sources_prompt(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.begin_path_prompt(LoadPromptKind::LogSources, context);
    }

    /// 启动系统路径选择器，并在用户确认后异步扫描路径。
    ///
    /// 业务意图：
    /// - 系统选择器必须在前台应用上下文中打开；文件系统扫描可能较慢，因此放到后台执行器。
    /// - 新一轮扫描完成前会清空旧 tab，避免右侧继续展示不属于当前来源树的日志内容。
    ///
    /// 边界条件：
    /// - 用户取消选择时保持现有目录树不变。
    /// - 当前不支持取消后台扫描；如果用户连续触发多次加载，后完成的任务会覆盖先完成的任务。
    fn begin_path_prompt(&mut self, prompt_kind: LoadPromptKind, context: &mut Context<Self>) {
        let options = prompt_kind.to_prompt_options();
        let loading_message = prompt_kind.loading_message().to_string();

        context
            .spawn(async move |view, app| {
                let receiver = match app.update(|app| app.prompt_for_paths(options)) {
                    Ok(receiver) => receiver,
                    Err(error) => {
                        view.update(app, |view, context| {
                            view.load_state = LogTreeLoadState::Failed {
                                message: format!("无法打开系统路径选择器：{}", error),
                            };
                            context.notify();
                        })
                        .ok();
                        return;
                    }
                };

                let selected_paths: Vec<PathBuf> = match receiver.await {
                    Ok(Ok(Some(paths))) if !paths.is_empty() => paths,
                    Ok(Ok(_)) => return,
                    Ok(Err(error)) => {
                        view.update(app, |view, context| {
                            view.load_state = LogTreeLoadState::Failed {
                                message: format!("路径选择器返回错误：{}", error),
                            };
                            context.notify();
                        })
                        .ok();
                        return;
                    }
                    Err(error) => {
                        view.update(app, |view, context| {
                            view.load_state = LogTreeLoadState::Failed {
                                message: format!("路径选择器被中断：{}", error),
                            };
                            context.notify();
                        })
                        .ok();
                        return;
                    }
                };

                view.update(app, |view, context| {
                    // 新一轮加载会替换左侧来源树；旧 tab 的来源可能已经不属于当前树，因此同步清空右侧工作区。
                    view.open_tabs.clear();
                    view.active_tab_id = None;
                    view.tab_context_menu = None;
                    view.encoding_dropdown_menu = None;
                    view.log_scrollbar_drag = None;
                    view.log_tree_scrollbar_drag = None;
                    view.tab_bar_scroll_handle = ScrollHandle::new();
                    view.load_state = LogTreeLoadState::Loading {
                        message: loading_message,
                    };
                    context.notify();
                })
                .ok();

                let load_result = app
                    .background_executor()
                    .spawn(async move { load_log_sources(selected_paths) })
                    .await;

                view.update(app, |view, context| {
                    view.load_state = match load_result {
                        Ok(tree) => {
                            view.log_tree_scroll_handle = UniformListScrollHandle::new();
                            view.log_tree_scrollbar_drag = None;
                            LogTreeLoadState::Loaded(LoadedLogTreeState::new(tree))
                        }
                        Err(error) => LogTreeLoadState::Failed {
                            message: error.to_string(),
                        },
                    };
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 渲染一个 Lucide 字体图标。
    ///
    /// 业务意图：
    /// - 工具栏和目录树都依赖同一套第三方图标库，集中渲染可以避免字体族、字号和占位宽度分散。
    /// - `icon` 允许为空，用于目录树文件节点的展开箭头占位，保证不同类型节点文本对齐。
    ///
    /// 边界条件：
    /// - 该函数只负责渲染图标字形，不处理点击、悬浮说明或无障碍标签。
    /// - 字体必须已在应用启动时注册，否则图标字符可能被系统字体渲染成错误符号。
    fn render_lucide_icon(
        icon: Option<Icon>,
        width: f32,
        icon_size: f32,
        icon_color: u32,
    ) -> impl IntoElement {
        // Lucide 枚举通过 `char` 映射到字体字形；空图标使用空字符串占位，避免目录树行错位。
        let icon_text = icon
            .map(|icon| char::from(icon).to_string())
            .unwrap_or_default();

        div()
            .w(px(width))
            .flex_none()
            .font_family(LUCIDE_FONT_FAMILY)
            .text_size(px(icon_size))
            .text_color(rgb(icon_color))
            .child(icon_text)
    }

    /// 渲染左侧日志目录树面板。
    ///
    /// 业务意图：
    /// - 左侧区域用于承载“加载日志”后的目录树，当前已经支持真实文件、目录和压缩包来源。
    /// - 面板包含标题、加载状态和目录树行，帮助用户确认当前加载结果是否来自真实选择。
    ///
    /// 边界条件：
    /// - 当前面板只负责目录树展示和节点点击入口，不直接读取正文；正文读取由 tab 工作区异步处理。
    /// - 真实节点支持展开/收起，行渲染交给虚拟列表处理；后续如需搜索或过滤树节点需要新增独立状态。
    fn render_log_tree_panel(&self, context: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0xf8fafc))
            .child(self.render_log_tree_header())
            .child(self.render_log_tree_body(context))
    }

    /// 渲染左侧日志目录树标题。
    ///
    /// 业务意图：
    /// - 用户要求去除“日志目录”文字，因此左侧只保留目录树图标作为轻量语义提示。
    /// - 右侧摘要展示真实加载结果的节点规模和错误数量，方便用户确认加载范围。
    ///
    /// 边界条件：
    /// - 当前标题不显示真实绝对路径，避免在路径脱敏和悬浮提示规则未定义前挤压窄面板。
    fn render_log_tree_header(&self) -> impl IntoElement {
        let summary = match &self.load_state {
            LogTreeLoadState::Loaded(tree_state) => tree_state.summary().to_string(),
            LogTreeLoadState::Empty
            | LogTreeLoadState::Loading { .. }
            | LogTreeLoadState::Failed { .. } => String::new(),
        };

        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(LOG_TREE_HEADER_HEIGHT))
            .px(px(LOG_TREE_ROW_HORIZONTAL_PADDING))
            .border_b_1()
            .border_color(rgb(0xe5e7eb))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_sm()
                    .text_color(rgb(0x24292f))
                    .child(Self::render_lucide_icon(
                        Some(Icon::FolderTree),
                        LOG_TREE_ITEM_ICON_WIDTH,
                        LOG_TREE_ITEM_ICON_SIZE,
                        0x57606a,
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x6b7280))
                    .truncate()
                    .child(summary),
            )
    }

    /// 渲染左侧日志目录树主体区域。
    ///
    /// 业务意图：
    /// - 只渲染真实加载完成后的目录树；未加载、加载中或加载失败时左侧栏不会显示。
    /// - 文件数量较多时使用 GPUI `uniform_list` 只渲染当前可见区间，避免滚动时为所有节点创建元素。
    ///
    /// 边界条件：
    /// - 目录树行高固定为 `LOG_TREE_ROW_HEIGHT`，符合 `uniform_list` 对等高元素的要求。
    /// - 虚拟列表数量来自当前可见行缓存，展开/收起后会重新计算并驱动列表更新。
    fn render_log_tree_body(&self, context: &mut Context<Self>) -> impl IntoElement {
        let visible_row_count = match &self.load_state {
            LogTreeLoadState::Loaded(tree_state) => tree_state.visible_rows.len(),
            LogTreeLoadState::Empty
            | LogTreeLoadState::Loading { .. }
            | LogTreeLoadState::Failed { .. } => 0,
        };

        div()
            .id("log-tree-body")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .overflow_hidden()
            .child(
                uniform_list(
                    "log-tree-virtual-list",
                    visible_row_count,
                    context.processor(|view, range: std::ops::Range<usize>, _window, context| {
                        // 先复制当前可见区间的数据，再渲染元素，避免同时持有 `load_state` 的不可变借用和
                        // 需要注册点击监听的可变 `Context`，这是 Rust 借用规则下最清晰的分界。
                        let rows: Vec<LoadedLogTreeRow> = match &view.load_state {
                            LogTreeLoadState::Loaded(tree_state) => range
                                .filter_map(|index| tree_state.visible_rows.get(index).cloned())
                                .collect(),
                            LogTreeLoadState::Empty
                            | LogTreeLoadState::Loading { .. }
                            | LogTreeLoadState::Failed { .. } => Vec::new(),
                        };

                        rows.iter()
                            .map(|row| view.render_loaded_log_tree_row(row, context))
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full()
                .track_scroll(self.log_tree_scroll_handle.clone()),
            )
            .child(self.render_log_tree_scrollbar(visible_row_count, context))
    }

    /// 渲染左侧目录树的纵向可见滚动条。
    ///
    /// 业务意图：
    /// - 左侧目录树文件较多时必须显示滚动位置，并支持鼠标拖动快速定位。
    /// - 滚动条复用目录树虚拟列表的 `UniformListScrollHandle`，确保滚轮滚动、虚拟渲染和滑块位置保持同源。
    ///
    /// 边界条件：
    /// - 只有内容高度超过视口时显示；首帧尚未完成测量但节点明显较多时，会显示一个临时滑块提示可滚动。
    fn render_log_tree_scrollbar(
        &self,
        visible_row_count: usize,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) = Self::log_tree_scrollbar_metrics(&self.log_tree_scroll_handle)
            .or_else(|| Self::fallback_log_tree_scrollbar_metrics(visible_row_count))
        else {
            return div().id("log-tree-scrollbar-empty").hidden();
        };

        div()
            .id("log-tree-scrollbar")
            .absolute()
            .top(metrics.thumb_start)
            .right(px(LOG_TREE_SCROLLBAR_PADDING))
            .w(px(LOG_TREE_SCROLLBAR_WIDTH))
            .h(metrics.thumb_length)
            .rounded(px(LOG_TREE_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(0xc9d1d9))
            .cursor_pointer()
            .hover(|thumb| thumb.bg(rgb(0x8c959f)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.start_log_tree_scrollbar_drag(event);
                    context.notify();
                }),
            )
    }

    /// 计算左侧目录树滚动条滑块位置和高度。
    ///
    /// 业务意图：
    /// - 使用虚拟列表上一次布局记录的视口高度、内容高度和滚动偏移计算滑块，让滚轮滚动和滑块位置同步。
    /// - 目录树行高固定，因此 `UniformListScrollHandle` 的测量结果可以稳定反映完整内容高度。
    fn log_tree_scrollbar_metrics(
        scroll_handle: &UniformListScrollHandle,
    ) -> Option<LogScrollbarMetrics> {
        let state = scroll_handle.0.borrow();
        let size = state.last_item_size?;
        let viewport_height = size.item.height;
        let content_height = size.contents.height;
        if viewport_height <= px(0.0) || content_height <= viewport_height {
            return None;
        }

        let max_scroll = content_height - viewport_height;
        let scroll_top = (-state.base_handle.offset().y).clamp(px(0.0), max_scroll);
        let track_start = px(LOG_TREE_SCROLLBAR_PADDING);
        let track_length = (viewport_height - track_start * 2.0).max(px(1.0));
        let min_thumb_length = px(LOG_TREE_SCROLLBAR_MIN_THUMB_HEIGHT).min(track_length);
        let thumb_length = (viewport_height * (viewport_height / content_height))
            .clamp(min_thumb_length, track_length);
        let movable_length = (track_length - thumb_length).max(px(0.0));
        let thumb_start = track_start + movable_length * (scroll_top / max_scroll);

        Some(LogScrollbarMetrics {
            thumb_start,
            thumb_length,
            track_start,
            track_length,
            max_scroll,
        })
    }

    /// 在目录树首帧尚未完成测量时提供临时滚动条提示。
    ///
    /// 业务意图：
    /// - 大目录刚加载完成时，虚拟列表需要一帧后才写入真实测量；临时滑块可以立即告诉用户左侧列表可滚动。
    /// - 该结果只用于视觉提示，真实布局完成后会被 `log_tree_scrollbar_metrics` 替换。
    fn fallback_log_tree_scrollbar_metrics(
        visible_row_count: usize,
    ) -> Option<LogScrollbarMetrics> {
        if visible_row_count <= 24 {
            return None;
        }

        Some(LogScrollbarMetrics {
            thumb_start: px(LOG_TREE_SCROLLBAR_PADDING),
            thumb_length: px(LOG_TREE_SCROLLBAR_MIN_THUMB_HEIGHT),
            track_start: px(LOG_TREE_SCROLLBAR_PADDING),
            track_length: px(LOG_TREE_SCROLLBAR_MIN_THUMB_HEIGHT),
            max_scroll: px(0.0),
        })
    }

    /// 渲染真实加载结果中的单行节点。
    ///
    /// 业务意图：
    /// - 根据加载模块提供的节点类型选择图标、颜色和展开占位，保持 UI 逻辑不反向解析文件名。
    /// - 文件夹和压缩包节点支持点击展开/收起，普通文件节点支持点击打开到右侧 tab。
    /// - 真实结果只展示名称和短元信息，不展示绝对路径或压缩包内部完整路径。
    ///
    /// 边界条件：
    /// - 展开状态由 `LoadedLogTreeState` 保存；行号来自虚拟列表可见区间，不能作为展开键使用。
    /// - 错误详情暂不展开显示，只保存在加载结果中，后续可接入悬浮提示或状态面板。
    fn render_loaded_log_tree_row(
        &self,
        row: &LoadedLogTreeRow,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        // 性能约束：虚拟列表渲染会频繁调用本函数，只有压缩包根节点才需要扫描子树判断是否可直接打开。
        // 普通文件、目录和错误节点不能进入该线性扫描路径，否则大目录滚动时会把每行渲染放大为 O(整棵树)。
        let direct_archive_source = if row.kind == LogTreeEntryKind::Archive {
            self.single_file_archive_source(row.id)
        } else {
            None
        };
        let row_source = row.source.clone().or(direct_archive_source);
        let can_toggle = row.has_children && row_source.is_none();
        let is_expanded = self.is_log_tree_node_expanded(row.id);
        let expand_icon = if can_toggle {
            Some(if is_expanded {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            })
        } else {
            None
        };
        let (item_icon, icon_color) = Self::loaded_log_tree_icon(row.kind);

        Self::render_log_tree_row(
            LogTreeRowRenderData {
                node_id: row.id,
                depth: row.depth,
                expand_icon,
                item_icon,
                icon_color,
                can_toggle,
                source: row_source,
                label: row.label.clone(),
                meta: row.meta.clone(),
            },
            context,
        )
    }

    /// 渲染左侧日志目录树中的通用单行节点。
    ///
    /// 业务意图：
    /// - 真实节点共享同一行模板，避免文本截断、缩进和元信息样式分叉。
    /// - 可展开节点在这里绑定展开事件，可打开文件节点在这里绑定右侧 tab 打开事件。
    /// - 行根节点必须占满虚拟列表宽度，让 hover 激活态覆盖整行，而不是只包住文件名和大小文本。
    ///
    /// 边界条件：
    /// - 文件来源来自加载层，不从展示文案反推真实路径，避免目录同名或压缩包路径分隔符差异导致误读。
    fn render_log_tree_row(
        row_data: LogTreeRowRenderData,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let LogTreeRowRenderData {
            node_id,
            depth,
            expand_icon,
            item_icon,
            icon_color,
            can_toggle,
            source,
            label,
            meta,
        } = row_data;
        let left_padding = LOG_TREE_ROW_HORIZONTAL_PADDING + depth as f32 * LOG_TREE_ROW_INDENT;

        let row = div()
            .id(SharedString::from(format!("log-tree-row-{}", node_id)))
            .flex()
            .items_center()
            .gap_1()
            .h(px(LOG_TREE_ROW_HEIGHT))
            .w_full()
            .min_w_0()
            .pl(px(left_padding))
            .pr(px(LOG_TREE_ROW_HORIZONTAL_PADDING))
            .text_sm()
            .text_color(rgb(0x24292f))
            .hover(|tree_row| tree_row.bg(rgb(0xeef2f7)))
            .child(Self::render_lucide_icon(
                expand_icon,
                LOG_TREE_CHEVRON_WIDTH,
                LOG_TREE_CHEVRON_SIZE,
                0x6b7280,
            ))
            .child(Self::render_lucide_icon(
                Some(item_icon),
                LOG_TREE_ITEM_ICON_WIDTH,
                LOG_TREE_ITEM_ICON_SIZE,
                icon_color,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .flex_1()
                    .min_w_0()
                    .child(div().min_w_0().truncate().child(label))
                    .child(Self::render_log_tree_meta(meta.as_deref())),
            );

        if let Some(source) = source {
            row.cursor_pointer().on_click(context.listener(
                move |view, _event: &ClickEvent, _window, context| {
                    view.open_log_file(source.clone(), context);
                },
            ))
        } else if can_toggle {
            row.cursor_pointer().on_click(context.listener(
                move |view, _event: &ClickEvent, _window, context| {
                    view.toggle_log_tree_node(node_id, context);
                },
            ))
        } else {
            row
        }
    }

    /// 判断某个日志目录树节点当前是否展开。
    ///
    /// 业务意图：
    /// - 渲染虚拟列表可见行时需要给文件夹选择正确箭头方向。
    ///
    /// 边界条件：
    /// - 非已加载状态下没有目录树，统一返回 `false`，避免旧事件或重绘路径访问不存在的树状态。
    fn is_log_tree_node_expanded(&self, node_id: usize) -> bool {
        match &self.load_state {
            LogTreeLoadState::Loaded(tree_state) => tree_state.is_expanded(node_id),
            LogTreeLoadState::Empty
            | LogTreeLoadState::Loading { .. }
            | LogTreeLoadState::Failed { .. } => false,
        }
    }

    /// 返回单文件压缩包根节点可以直接打开的内部文件来源。
    ///
    /// 业务意图：
    /// - 该方法把“压缩包只有一个文件时直接打开”的交互规则收口在主视图，避免渲染函数理解完整树扫描细节。
    /// - 非加载状态和非压缩包节点统一返回 `None`，调用方可以继续走普通展开/收起逻辑。
    fn single_file_archive_source(&self, node_id: usize) -> Option<LogFileSource> {
        match &self.load_state {
            LogTreeLoadState::Loaded(tree_state) => {
                tree_state.single_file_source_for_archive(node_id)
            }
            LogTreeLoadState::Empty
            | LogTreeLoadState::Loading { .. }
            | LogTreeLoadState::Failed { .. } => None,
        }
    }

    /// 切换日志目录树节点的展开状态。
    ///
    /// 业务意图：
    /// - 用户点击文件夹或压缩包行时，左侧树应立即展开或收起对应子树。
    /// - 切换后只重建可见行缓存，完整加载结果保持不变，避免重复扫描文件系统或压缩包。
    ///
    /// 边界条件：
    /// - 只有当前已加载状态会响应；加载中、失败或未加载状态下的旧点击事件会被忽略。
    fn toggle_log_tree_node(&mut self, node_id: usize, context: &mut Context<Self>) {
        if let LogTreeLoadState::Loaded(tree_state) = &mut self.load_state {
            tree_state.toggle_node(node_id);
            context.notify();
        }
    }

    /// 打开左侧目录树中的日志文件。
    ///
    /// 业务意图：
    /// - 普通文件和压缩包成员都通过 `LogFileSource` 统一进入右侧 tab 工作区。
    /// - 同一个来源重复点击时只激活已有 tab，避免用户误打开多个相同文件。
    ///
    /// 边界条件：
    /// - 读取和自动解码都放到后台执行器，避免大文件或压缩包流式读取阻塞 GPUI 主线程。
    /// - 如果 tab 在后台任务完成前被关闭，结果合并时会因找不到 ID 而被忽略。
    fn open_log_file(&mut self, source: LogFileSource, context: &mut Context<Self>) {
        let source_key = source.stable_key();
        if let Some(tab) = self
            .open_tabs
            .iter()
            .find(|tab| tab.source_key == source_key)
        {
            self.activate_tab(tab.id);
            context.notify();
            return;
        }

        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        let title = source.display_name();
        if self.open_tabs.is_empty() {
            self.tab_bar_scroll_handle = ScrollHandle::new();
        }
        self.open_tabs.push(OpenLogTab {
            id: tab_id,
            source: source.clone(),
            source_key,
            title,
            encoding_choice: EncodingChoice::Auto,
            raw_bytes: None,
            state: LogTabState::Loading {
                message: "正在读取日志文件...".to_string(),
            },
            scroll_handle: UniformListScrollHandle::new(),
            pending_scroll_to_line: None,
            highlighted_search_line: None,
            text_selection: None,
            selection_drag_anchor: None,
        });
        self.activate_tab(tab_id);
        context.notify();
        self.spawn_log_tab_load(tab_id, source, context);
    }

    /// 激活指定日志 tab 并确保 tab 页签在横向滚动区域内可见。
    ///
    /// 业务意图：
    /// - 打开、点击、右键和后台内容刷新都会改变用户关注的 tab，tab 栏需要自动滚动到能看到当前 tab 的位置。
    /// - 将激活逻辑集中到一个方法，避免不同入口遗漏关闭弹层或遗漏页签自动定位。
    ///
    /// 边界条件：
    /// - 如果 tab 已经被关闭或不存在，直接忽略，避免旧异步任务或过期点击事件激活不存在的页面。
    fn activate_tab(&mut self, tab_id: usize) {
        if !self.open_tabs.iter().any(|tab| tab.id == tab_id) {
            return;
        }

        self.active_tab_id = Some(tab_id);
        self.tab_context_menu = None;
        self.encoding_dropdown_menu = None;
        self.scroll_tab_bar_to_tab(tab_id);
    }

    /// 将指定 tab 页签滚动到可视范围内。
    ///
    /// 业务意图：
    /// - tab 标题完整展示后，页签总宽度可能超过右侧区域；激活 tab 时自动定位能减少用户手动点箭头的次数。
    /// - GPUI `ScrollHandle::scroll_to_item` 会在下一次布局时按 child 下标做最小滚动，适合这里的横向页签列表。
    fn scroll_tab_bar_to_tab(&self, tab_id: usize) {
        if let Some(index) = self.open_tabs.iter().position(|tab| tab.id == tab_id) {
            self.tab_bar_scroll_handle.scroll_to_item(index);
        }
    }

    /// 将指定 tab 的日志正文滚动到目标行。
    ///
    /// 业务意图：
    /// - 搜索结果点击后需要把命中行带到用户视野中央，而不是只打开文件让用户手动查找。
    /// - 使用 `UniformListScrollHandle::scroll_to_item_strict` 延迟到下一次布局执行，适合虚拟列表尚未完成测量的场景。
    ///
    /// 边界条件：
    /// - 只有已打开 tab 才能滚动；如果 tab 仍在加载，调用方应先把行号写入 `pending_scroll_to_line`。
    /// - 行号来自搜索时的解码结果，如果文件在搜索后被外部修改，滚动位置可能不再对应同一内容，这是当前未实现文件监听的已知边界。
    fn scroll_log_tab_to_line(&self, tab_id: usize, line_index: usize) {
        if let Some(tab) = self.open_tabs.iter().find(|tab| tab.id == tab_id) {
            tab.scroll_handle
                .scroll_to_item_strict(line_index, ScrollStrategy::Center);
        }
    }

    /// 启动日志 tab 的后台读取任务。
    ///
    /// 业务意图：
    /// - 读取压缩包成员和解码大文本都可能耗时，必须离开 UI 线程执行。
    /// - 后台任务返回时只按 tab ID 合并状态，避免持有过期引用。
    fn spawn_log_tab_load(
        &self,
        tab_id: usize,
        source: LogFileSource,
        context: &mut Context<Self>,
    ) {
        let source_name = source.display_name();
        context
            .spawn(async move |view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move {
                        match read_log_source_bytes(&source) {
                            Ok(raw_bytes) => {
                                match decode_log_bytes(
                                    &raw_bytes,
                                    EncodingChoice::Auto,
                                    &source_name,
                                ) {
                                    Ok(document) => LogTabLoadResult::Ready {
                                        raw_bytes,
                                        document,
                                    },
                                    Err(error) => LogTabLoadResult::DecodeFailed {
                                        raw_bytes,
                                        message: error.to_string(),
                                    },
                                }
                            }
                            Err(error) => LogTabLoadResult::ReadFailed {
                                message: error.to_string(),
                            },
                        }
                    })
                    .await;

                view.update(app, |view, context| {
                    view.apply_log_tab_load_result(tab_id, result);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 合并日志 tab 的后台读取结果。
    ///
    /// 业务意图：
    /// - 读取成功后保存原始字节；即使自动检测失败，也允许用户通过手动编码按钮重新解析。
    fn apply_log_tab_load_result(&mut self, tab_id: usize, result: LogTabLoadResult) {
        let mut pending_scroll_to_line = None;
        {
            let Some(tab) = self.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
                return;
            };

            tab.scroll_handle = UniformListScrollHandle::new();
            tab.text_selection = None;
            tab.selection_drag_anchor = None;
            match result {
                LogTabLoadResult::Ready {
                    raw_bytes,
                    document,
                } => {
                    tab.raw_bytes = Some(raw_bytes);
                    tab.state = LogTabState::Ready { document };
                    pending_scroll_to_line = tab.pending_scroll_to_line.take();
                }
                LogTabLoadResult::DecodeFailed { raw_bytes, message } => {
                    tab.raw_bytes = Some(raw_bytes);
                    tab.state = LogTabState::Failed { message };
                    tab.pending_scroll_to_line = None;
                }
                LogTabLoadResult::ReadFailed { message } => {
                    tab.raw_bytes = None;
                    tab.state = LogTabState::Failed { message };
                    tab.pending_scroll_to_line = None;
                }
            }
        }

        if self.active_tab_id == Some(tab_id) {
            self.scroll_tab_bar_to_tab(tab_id);
        }
        if let Some(line_index) = pending_scroll_to_line {
            self.scroll_log_tab_to_line(tab_id, line_index);
        }
    }

    /// 为指定 tab 切换编码并重新解码。
    ///
    /// 业务意图：
    /// - 编码切换必须复用 tab 内保存的原始字节，避免重新读取压缩包成员或普通文件。
    /// - 切换后重置该 tab 的正文滚动句柄，让用户从文件开头重新检查解码结果。
    fn select_tab_encoding(
        &mut self,
        tab_id: usize,
        encoding_choice: EncodingChoice,
        context: &mut Context<Self>,
    ) {
        let Some(tab) = self.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };

        let Some(raw_bytes) = tab.raw_bytes.clone() else {
            // 原始字节尚未读取完成时不能提前写入编码选择，否则后台自动加载完成后会出现
            // “下拉框显示手动编码、正文却来自自动识别”的状态不一致。
            self.tab_context_menu = None;
            self.encoding_dropdown_menu = None;
            context.notify();
            return;
        };

        tab.encoding_choice = encoding_choice;
        tab.scroll_handle = UniformListScrollHandle::new();
        tab.pending_scroll_to_line = None;
        tab.highlighted_search_line = None;
        tab.text_selection = None;
        tab.selection_drag_anchor = None;
        tab.state = LogTabState::Loading {
            message: format!("正在按 {} 重新解析...", encoding_choice.label()),
        };
        self.tab_context_menu = None;
        self.encoding_dropdown_menu = None;
        let source_name = tab.title.clone();
        if self
            .log_scrollbar_drag
            .is_some_and(|drag| drag.tab_id == tab_id)
        {
            self.log_scrollbar_drag = None;
        }
        context.notify();
        self.spawn_log_tab_decode(tab_id, raw_bytes, encoding_choice, source_name, context);
    }

    /// 启动日志 tab 的后台重新解码任务。
    ///
    /// 业务意图：
    /// - 200MB 以内的日志重新解码仍可能耗时，放到后台执行可以避免界面短暂停顿。
    fn spawn_log_tab_decode(
        &self,
        tab_id: usize,
        raw_bytes: Arc<Vec<u8>>,
        encoding_choice: EncodingChoice,
        source_name: String,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move {
                        decode_log_bytes(&raw_bytes, encoding_choice, &source_name)
                            .map(|document| LogTabDecodeResult::Ready {
                                encoding_choice,
                                document,
                            })
                            .unwrap_or_else(|error: LogContentError| LogTabDecodeResult::Failed {
                                encoding_choice,
                                message: error.to_string(),
                            })
                    })
                    .await;

                view.update(app, |view, context| {
                    view.apply_log_tab_decode_result(tab_id, result);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 合并日志 tab 的重新解码结果。
    ///
    /// 边界条件：
    /// - 如果用户在解码完成前关闭了 tab，则结果会被忽略。
    /// - 如果用户在旧任务完成前再次切换编码，则旧结果会被丢弃，避免 UI 显示内容和编码按钮不一致。
    fn apply_log_tab_decode_result(&mut self, tab_id: usize, result: LogTabDecodeResult) {
        {
            let Some(tab) = self.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
                return;
            };

            let result_encoding_choice = match &result {
                LogTabDecodeResult::Ready {
                    encoding_choice, ..
                }
                | LogTabDecodeResult::Failed {
                    encoding_choice, ..
                } => *encoding_choice,
            };
            if tab.encoding_choice != result_encoding_choice {
                return;
            }

            tab.scroll_handle = UniformListScrollHandle::new();
            tab.text_selection = None;
            tab.selection_drag_anchor = None;
            match result {
                LogTabDecodeResult::Ready { document, .. } => {
                    tab.state = LogTabState::Ready { document };
                }
                LogTabDecodeResult::Failed { message, .. } => {
                    tab.state = LogTabState::Failed { message };
                }
            }
        }

        if self.active_tab_id == Some(tab_id) {
            self.scroll_tab_bar_to_tab(tab_id);
        }
    }

    /// 处理全局键盘快捷键。
    ///
    /// 业务意图：
    /// - 搜索是日志查看器的核心工作流，需要即使焦点停在日志正文或目录树上也能通过 `Ctrl+F` 打开。
    /// - macOS 用户通常使用 `Cmd+F`，因此在保留用户要求的 `Ctrl+F` 同时支持平台键。
    ///
    /// 边界条件：
    /// - 这里不处理普通字符输入，避免全局监听截获搜索框或未来编辑控件的文本输入。
    /// - Enter 仅在搜索对话框打开时触发搜索，Esc 仅关闭搜索对话框。
    fn handle_global_keystroke(
        &mut self,
        keystroke: Keystroke,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if Self::is_copy_keystroke(&keystroke) && self.copy_selected_log_text(context) {
            return;
        }

        if Self::is_open_search_keystroke(&keystroke) {
            self.open_search_dialog(window, context);
            return;
        }

        if self.search_dialog.is_some() && keystroke.key == "enter" {
            self.start_search(context);
            return;
        }

        if self.search_dialog.is_some() && keystroke.key == "escape" {
            self.close_search_dialog(window, context);
        }
    }

    /// 判断是否为打开搜索对话框的快捷键。
    fn is_open_search_keystroke(keystroke: &Keystroke) -> bool {
        keystroke.key.eq_ignore_ascii_case("f")
            && (keystroke.modifiers.control || keystroke.modifiers.platform)
    }

    /// 判断是否为复制日志选中文本的快捷键。
    ///
    /// 业务意图：
    /// - macOS 用户习惯 `Cmd+C`，Windows 用户习惯 `Ctrl+C`，两者都应复制当前日志正文选区。
    /// - 当前只在日志正文有非空选择时消费该快捷键；没有选择时保持后续控件自己的复制行为空间。
    fn is_copy_keystroke(keystroke: &Keystroke) -> bool {
        keystroke.key.eq_ignore_ascii_case("c")
            && (keystroke.modifiers.control || keystroke.modifiers.platform)
    }

    /// 复制当前激活日志 tab 的选中文本。
    ///
    /// 业务意图：
    /// - 日志正文是只读查看器，复制操作应从已解码的真实文本行中提取，而不是从屏幕像素或渲染元素反推。
    /// - 只有存在非空选择时才写入剪贴板，避免用户在搜索框等其它控件内按复制键时被日志查看器误拦截。
    ///
    /// 边界条件：
    /// - 复制文本保留跨行选择中的换行符，满足从日志中截取堆栈片段或多行上下文的常见需求。
    /// - 选择范围使用字符列，截取前会转换为 UTF-8 字节边界，中文不会被截断为非法字符串。
    fn copy_selected_log_text(&self, context: &mut Context<Self>) -> bool {
        let Some(text) = self.selected_log_text() else {
            return false;
        };
        if text.is_empty() {
            return false;
        }

        context.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// 取得当前激活日志 tab 的选中文本。
    fn selected_log_text(&self) -> Option<String> {
        let active_tab_id = self.active_tab_id?;
        let tab = self.open_tabs.iter().find(|tab| tab.id == active_tab_id)?;
        let selection = tab.text_selection.as_ref()?;
        if selection.is_empty() {
            return None;
        }
        let LogTabState::Ready { document } = &tab.state else {
            return None;
        };

        let (start, end) = selection.normalized();
        if document.lines.is_empty() || start.line_index >= document.lines.len() {
            return None;
        }
        let end_line_index = end.line_index.min(document.lines.len().saturating_sub(1));
        if start.line_index > end_line_index {
            return None;
        }

        let mut selected_text = String::new();
        for line_index in start.line_index..=end_line_index {
            let Some(line) = document.lines.get(line_index) else {
                continue;
            };
            if line_index > start.line_index {
                selected_text.push('\n');
            }
            let Some((start_column, end_column)) =
                Self::selection_columns_for_line(selection, line_index, line)
            else {
                continue;
            };
            let start_byte = Self::byte_index_for_char_column(line, start_column);
            let end_byte = Self::byte_index_for_char_column(line, end_column);
            if start_byte < end_byte {
                selected_text.push_str(&line[start_byte..end_byte]);
            }
        }

        Some(selected_text)
    }

    /// 取得可填入搜索框的日志选中文本。
    ///
    /// 业务意图：
    /// - 当前搜索引擎是逐行普通文本搜索，搜索框也是单行输入；当用户选中多行日志时，直接填入换行会让搜索条件不可用。
    /// - 因此这里取第一段非空行作为搜索关键字，复制功能仍保留完整多行内容。
    fn selected_log_text_for_search_query(&self) -> Option<String> {
        self.selected_log_text()
            .and_then(|text| {
                text.lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(ToOwned::to_owned)
            })
            .filter(|query| !query.is_empty())
    }

    /// 返回某一行被当前选择覆盖的字符列范围。
    ///
    /// 业务意图：
    /// - 渲染选区和复制文本都需要同一套范围规则，避免屏幕高亮和复制结果不一致。
    /// - 这里输出字符列而不是字节范围，由调用方按具体字符串转换为 UTF-8 安全字节边界。
    fn selection_columns_for_line(
        selection: &LogTextSelection,
        line_index: usize,
        line: &str,
    ) -> Option<(usize, usize)> {
        let (start, end) = selection.normalized();
        if line_index < start.line_index || line_index > end.line_index {
            return None;
        }

        let line_char_count = line.chars().count();
        let start_column = if line_index == start.line_index {
            start.column.min(line_char_count)
        } else {
            0
        };
        let end_column = if line_index == end.line_index {
            end.column.min(line_char_count)
        } else {
            line_char_count
        };

        Some((start_column, end_column))
    }

    /// 返回某一行被选择覆盖的 UTF-8 字节范围。
    ///
    /// 边界条件：
    /// - 空范围不参与渲染高亮，但复制跨行选择时仍会通过行间换行保留空行语义。
    fn selected_byte_range_for_line(
        selection: &LogTextSelection,
        line_index: usize,
        line: &str,
    ) -> Option<Range<usize>> {
        let (start_column, end_column) =
            Self::selection_columns_for_line(selection, line_index, line)?;
        let start_byte = Self::byte_index_for_char_column(line, start_column);
        let end_byte = Self::byte_index_for_char_column(line, end_column);
        (start_byte < end_byte).then_some(start_byte..end_byte)
    }

    /// 将字符列转换为字符串的 UTF-8 字节下标。
    ///
    /// 业务意图：
    /// - 鼠标选择以字符列表达，`String::replace_range`、切片和 `StyledText` 高亮范围都要求 UTF-8 字节边界。
    /// - 统一转换函数可以避免中文、全角字符或 emoji 出现在选区边缘时产生非法切片。
    fn byte_index_for_char_column(text: &str, column: usize) -> usize {
        if column == 0 {
            return 0;
        }
        text.char_indices()
            .nth(column)
            .map(|(index, _)| index)
            .unwrap_or(text.len())
    }

    /// 返回当前激活文件所在目录的展示标签。
    ///
    /// 业务意图：
    /// - 用户切换到“当前目录”搜索时，需要看到实际将被搜索的目录目标。
    /// - 这里仅从当前活动 tab 的 `LogFileSource` 派生展示文本，不访问磁盘，也不扩大已加载目录树的权限边界。
    fn active_search_directory_label(&self) -> Option<String> {
        let active_tab_id = self.active_tab_id?;
        let active_tab = self.open_tabs.iter().find(|tab| tab.id == active_tab_id)?;
        Some(source_location_label(&active_tab.source))
    }

    /// 判断某个文件来源是否匹配用户编辑后的目录目标文本。
    ///
    /// 业务意图：
    /// - 当前目录搜索的基础范围仍由 `collect_current_directory_sources` 从加载树中递归收集。
    /// - 用户修改目录目标时，只在这批已授权来源内按展示路径做二次过滤，满足缩小范围或输入子目录片段的需求。
    ///
    /// 边界条件：
    /// - 目标为空时不额外过滤，保持“当前文件所在目录递归搜索”的默认行为。
    /// - 路径大小写在 Windows 上通常不敏感；这里统一使用小写包含匹配，优先保证跨平台用户输入的容错性。
    fn source_matches_directory_target(source: &LogFileSource, target: &str) -> bool {
        let target = target.trim();
        if target.is_empty() {
            return true;
        }

        source_location_label(source)
            .to_lowercase()
            .contains(&target.to_lowercase())
    }

    /// 从已加载目录树中收集匹配用户目录目标文本的来源。
    ///
    /// 业务意图：
    /// - “当前目录”搜索默认落在当前文件所在目录；当用户手动修改目标目录时，应允许定位到加载树中的其它目录或子目录片段。
    /// - 搜索仍只遍历已加载树中存在的 `LogFileSource`，不会因为用户输入路径而额外扫描磁盘或解压压缩包。
    ///
    /// 边界条件：
    /// - 同一来源可能因压缩包单文件快捷入口等原因重复出现在树中，必须按稳定键去重。
    fn collect_sources_matching_directory_target(
        tree: &LoadedLogTree,
        target: &str,
    ) -> Vec<LogFileSource> {
        let mut seen_keys = HashSet::new();
        let mut sources = Vec::new();

        for source in tree.rows.iter().filter_map(|row| row.source.as_ref()) {
            if !Self::source_matches_directory_target(source, target) {
                continue;
            }

            let key = source.stable_key();
            if seen_keys.insert(key) {
                sources.push(source.clone());
            }
        }

        sources
    }

    /// 标记指定搜索历史记录已取消。
    ///
    /// 业务意图：
    /// - 关闭搜索对话框会让后台任务回调失效，结果面板必须同步从“搜索中”切换为“已取消”。
    /// - 只修改对应任务的记录，避免影响面板中其它历史搜索。
    fn mark_search_record_canceled(&mut self, job_id: usize) {
        let Some(panel) = self.search_results_panel.as_mut() else {
            return;
        };
        if let Some(record) = panel
            .records
            .iter_mut()
            .find(|record| record.job_id == job_id)
        {
            record.canceled = true;
        }
    }

    /// 打开搜索对话框并聚焦输入框。
    ///
    /// 业务意图：
    /// - 如果日志正文当前存在选区，打开或再次唤起搜索框时用选中文本预填关键字，减少复制再搜索的重复操作。
    /// - 如果对话框已经打开但没有日志选区，再次按快捷键只重新聚焦输入框，不重置查询词和搜索范围。
    /// - 初次打开位置优先落在右侧内容区，避免遮挡左侧目录树。
    fn open_search_dialog(&mut self, window: &mut Window, context: &mut Context<Self>) {
        let selected_query = self.selected_log_text_for_search_query();
        if self.search_dialog.is_none() {
            let default_x = if matches!(self.load_state, LogTreeLoadState::Loaded(_)) {
                self.left_panel_width + SPLITTER_VISIBLE_WIDTH + SEARCH_DIALOG_DEFAULT_RIGHT_OFFSET
            } else {
                SEARCH_DIALOG_DEFAULT_RIGHT_OFFSET
            };
            let directory_target = self.active_search_directory_label().unwrap_or_default();
            let query = selected_query.clone().unwrap_or_default();
            let query_cursor = query.len();
            self.search_dialog = Some(SearchDialogState {
                query,
                selection_range: query_cursor..query_cursor,
                marked_range: None,
                scope: SearchScope::CurrentFile,
                directory_target,
                directory_selection_range: 0..0,
                directory_marked_range: None,
                case_sensitive: false,
                x: default_x,
                y: TOOLBAR_HEIGHT + SEARCH_DIALOG_DEFAULT_TOP_OFFSET,
                is_searching: false,
                progress: SearchProgress::default(),
                message: if selected_query.is_some() {
                    "已填入选中文本，按 Enter 或点击搜索".to_string()
                } else {
                    "输入关键字后按 Enter 或点击搜索".to_string()
                },
                job_id: 0,
            });
        } else if let Some(selected_query) = selected_query
            && let Some(dialog) = self.search_dialog.as_mut()
        {
            let cursor = selected_query.len();
            dialog.query = selected_query;
            dialog.selection_range = cursor..cursor;
            dialog.marked_range = None;
            dialog.message = "已填入选中文本，按 Enter 或点击搜索".to_string();
        }

        self.tab_context_menu = None;
        self.encoding_dropdown_menu = None;
        window.focus(&self.search_input_focus);
        context.notify();
    }

    /// 关闭搜索对话框并让当前后台搜索任务失效。
    ///
    /// 业务意图：
    /// - 用户关闭对话框时表示不再关注当前搜索过程；旧任务即使稍后返回，也不应继续更新进度或结果。
    /// - 结果面板不在这里清空，方便用户保留已完成的结果上下文；如果任务仍在运行，则标记为已取消，避免面板永远停留在进行中。
    fn close_search_dialog(&mut self, window: &mut Window, context: &mut Context<Self>) {
        let running_job_id = self
            .search_dialog
            .as_ref()
            .filter(|dialog| dialog.is_searching)
            .map(|dialog| dialog.job_id);
        self.search_dialog = None;
        self.search_dialog_drag = None;
        self.next_search_job_id += 1;
        if let Some(job_id) = running_job_id {
            self.mark_search_record_canceled(job_id);
        }
        window.focus(&self.root_focus_handle);
        context.notify();
    }

    /// 启动一次搜索。
    ///
    /// 业务意图：
    /// - 从搜索对话框读取当前查询词、范围和大小写规则，统一分发到当前文件或当前目录搜索。
    /// - 新搜索会保留历史记录，但如果上一轮搜索仍在运行，必须先标记为已取消，避免旧任务回调被丢弃后面板长期显示“搜索中”。
    fn start_search(&mut self, context: &mut Context<Self>) {
        let Some(dialog) = self.search_dialog.as_ref() else {
            return;
        };
        let options = SearchOptions {
            query: dialog.query.trim().to_string(),
            case_sensitive: dialog.case_sensitive,
        };
        if options.is_empty_query() {
            self.update_search_dialog_message("请输入要搜索的关键字", context);
            return;
        }

        let Some(active_tab_id) = self.active_tab_id else {
            self.update_search_dialog_message("请先从左侧打开一个日志文件", context);
            return;
        };
        let Some(active_tab) = self.open_tabs.iter().find(|tab| tab.id == active_tab_id) else {
            self.update_search_dialog_message("当前日志 tab 不存在，请重新选择文件", context);
            return;
        };

        let job_id = self.next_search_job_id;
        self.next_search_job_id += 1;
        let scope = dialog.scope;
        let case_sensitive = dialog.case_sensitive;
        let directory_target = dialog.directory_target.trim().to_string();

        let search_target = match scope {
            SearchScope::CurrentFile => match &active_tab.state {
                LogTabState::Ready { document } => SearchTarget::CurrentFile {
                    source: active_tab.source.clone(),
                    lines: Arc::clone(&document.lines),
                },
                LogTabState::Loading { .. } => {
                    self.update_search_dialog_message("当前文件仍在加载，完成后再搜索", context);
                    return;
                }
                LogTabState::Failed { .. } => {
                    self.update_search_dialog_message("当前文件打开失败，无法搜索正文", context);
                    return;
                }
            },
            SearchScope::CurrentDirectory => {
                let LogTreeLoadState::Loaded(tree_state) = &self.load_state else {
                    self.update_search_dialog_message("请先加载日志目录后再搜索当前目录", context);
                    return;
                };
                let sources = if directory_target.is_empty() {
                    collect_current_directory_sources(&tree_state.tree, &active_tab.source)
                } else {
                    Self::collect_sources_matching_directory_target(
                        &tree_state.tree,
                        &directory_target,
                    )
                };
                if sources.is_empty() {
                    self.update_search_dialog_message("当前目录中没有可搜索的日志文件", context);
                    return;
                }
                SearchTarget::CurrentDirectory { sources }
            }
        };

        let total_files = match &search_target {
            SearchTarget::CurrentFile { .. } => 1,
            SearchTarget::CurrentDirectory { sources } => sources.len(),
        };
        let previous_running_job_id = dialog.is_searching.then_some(dialog.job_id);
        let panel_height = self
            .search_results_panel
            .as_ref()
            .map(|panel| panel.height)
            .unwrap_or(SEARCH_RESULTS_PANEL_DEFAULT_HEIGHT);
        let new_record = SearchHistoryRecord {
            job_id,
            query: options.query.clone(),
            scope,
            directory_target: (scope == SearchScope::CurrentDirectory)
                .then(|| directory_target.clone())
                .filter(|target| !target.is_empty()),
            case_sensitive,
            progress: SearchProgress {
                searched_files: 0,
                total_files,
                matched_lines: 0,
            },
            results: Vec::new(),
            errors: Vec::new(),
            canceled: false,
            expanded: true,
            expanded_file_keys: HashSet::new(),
        };
        if let Some(previous_job_id) = previous_running_job_id {
            self.mark_search_record_canceled(previous_job_id);
        }
        if let Some(panel) = self.search_results_panel.as_mut() {
            for record in &mut panel.records {
                record.expanded = false;
            }
            panel.records.push(new_record);
            panel.rows = Self::search_results_panel_rows_from_records(&panel.records);
            panel.scroll_handle = UniformListScrollHandle::new();
        } else {
            let records = vec![new_record];
            let rows = Self::search_results_panel_rows_from_records(&records);
            self.search_results_panel = Some(SearchResultsPanelState {
                records,
                rows,
                height: panel_height,
                scroll_handle: UniformListScrollHandle::new(),
            });
        }
        if let Some(dialog) = self.search_dialog.as_mut() {
            dialog.job_id = job_id;
            dialog.is_searching = true;
            dialog.progress = SearchProgress {
                searched_files: 0,
                total_files,
                matched_lines: 0,
            };
            dialog.message = format!("正在搜索 0/{total_files} 个文件...");
        }
        context.notify();

        match search_target {
            SearchTarget::CurrentFile { source, lines } => {
                self.spawn_current_file_search(job_id, source, lines, options, context);
            }
            SearchTarget::CurrentDirectory { sources } => {
                self.spawn_current_directory_search(job_id, sources, options, context);
            }
        }
    }

    /// 更新搜索对话框提示文案。
    fn update_search_dialog_message(
        &mut self,
        message: impl Into<String>,
        context: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.search_dialog.as_mut() {
            dialog.message = message.into();
            dialog.is_searching = false;
        }
        context.notify();
    }

    /// 启动当前文件后台搜索。
    ///
    /// 业务意图：
    /// - 当前文件虽然已经解码，但逐行搜索大日志仍可能耗时，因此放到后台执行器运行。
    /// - 这里传入 `Arc<Vec<String>>`，只共享已解码行集合，不在 UI 线程复制 200MB 级别日志。
    fn spawn_current_file_search(
        &self,
        job_id: usize,
        source: LogFileSource,
        lines: Arc<Vec<String>>,
        options: SearchOptions,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |view, app| {
                let results = app
                    .background_executor()
                    .spawn(async move { search_lines(&source, lines.as_ref(), &options) })
                    .await;

                view.update(app, |view, context| {
                    view.apply_search_file_result(job_id, Ok(results), 1);
                    view.finish_search_job(job_id);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 启动当前目录递归后台搜索。
    ///
    /// 业务意图：
    /// - 目录搜索可能涉及多个本地文件或压缩包成员，必须逐文件放到后台读取和解码。
    /// - 每个文件完成后立即回传进度，让搜索对话框显示真实进展，而不是等全部完成才更新。
    fn spawn_current_directory_search(
        &self,
        job_id: usize,
        sources: Vec<LogFileSource>,
        options: SearchOptions,
        context: &mut Context<Self>,
    ) {
        let total_files = sources.len();
        context
            .spawn(async move |view, app| {
                for source in sources {
                    let search_options = options.clone();
                    let result = app
                        .background_executor()
                        .spawn(async move { Self::search_one_source(source, search_options) })
                        .await;

                    let should_continue = view
                        .update(app, |view, context| {
                            let current =
                                view.apply_search_file_result(job_id, result, total_files);
                            context.notify();
                            current
                        })
                        .unwrap_or(false);

                    if !should_continue {
                        return;
                    }
                }

                view.update(app, |view, context| {
                    view.finish_search_job(job_id);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 搜索单个文件来源。
    ///
    /// 业务意图：
    /// - 目录搜索中的每个文件独立读取、自动解码和搜索；失败时转换为文件级错误，调用方继续处理其它文件。
    /// - 这里复用现有 200MB 大小上限和编码检测逻辑，避免搜索路径绕过日志打开边界。
    fn search_one_source(
        source: LogFileSource,
        options: SearchOptions,
    ) -> Result<Vec<SearchResultItem>, SearchFileError> {
        let file_name = source.display_name();
        let raw_bytes = read_log_source_bytes(&source).map_err(|error| SearchFileError {
            file_name: file_name.clone(),
            message: error.to_string(),
        })?;
        let document =
            decode_log_bytes(&raw_bytes, EncodingChoice::Auto, &file_name).map_err(|error| {
                SearchFileError {
                    file_name: file_name.clone(),
                    message: error.to_string(),
                }
            })?;

        Ok(search_lines(&source, &document.lines, &options))
    }

    /// 合并单个文件的搜索结果并返回任务是否仍然有效。
    ///
    /// 业务意图：
    /// - 后台任务回到 UI 线程时必须校验任务 ID，避免旧任务覆盖新搜索状态。
    /// - 文件级错误只累积到结果面板，不中断当前任务。
    fn apply_search_file_result(
        &mut self,
        job_id: usize,
        result: Result<Vec<SearchResultItem>, SearchFileError>,
        total_files: usize,
    ) -> bool {
        if !self.is_current_search_job(job_id) {
            return false;
        }

        let mut matched_lines = 0usize;
        if let Some(panel) = self.search_results_panel.as_mut() {
            {
                let Some(record) = panel
                    .records
                    .iter_mut()
                    .find(|record| record.job_id == job_id)
                else {
                    return false;
                };
                match result {
                    Ok(mut results) => {
                        matched_lines = results.len();
                        for result in &results {
                            record.expanded_file_keys.insert(result.source_key.clone());
                        }
                        record.results.append(&mut results);
                    }
                    Err(error) => {
                        record.errors.push(error);
                    }
                }
                record.progress.searched_files += 1;
                record.progress.total_files = total_files;
                record.progress.matched_lines = record.results.len();
            }
            panel.rows = Self::search_results_panel_rows_from_records(&panel.records);
        }

        if let Some(dialog) = self.search_dialog.as_mut() {
            dialog.progress.searched_files += 1;
            dialog.progress.total_files = total_files;
            dialog.progress.matched_lines += matched_lines;
            dialog.message = format!(
                "正在搜索 {}/{} 个文件，已命中 {} 行",
                dialog.progress.searched_files,
                dialog.progress.total_files,
                dialog.progress.matched_lines
            );
        }

        true
    }

    /// 标记搜索任务完成并关闭搜索对话框。
    ///
    /// 业务意图：
    /// - 搜索对话框只负责输入条件和展示进行中进度；任务完成后应自动收起，把空间让给正文和底部结果面板。
    /// - 结果面板保留历史记录和明细，用户可以继续查看、展开和点击定位。
    fn finish_search_job(&mut self, job_id: usize) {
        if !self.is_current_search_job(job_id) {
            return;
        }

        if let Some(panel) = self.search_results_panel.as_mut()
            && let Some(record) = panel
                .records
                .iter_mut()
                .find(|record| record.job_id == job_id)
        {
            record.canceled = false;
        }
        self.search_dialog = None;
        self.search_dialog_drag = None;
    }

    /// 判断后台回调是否属于当前仍有效的搜索任务。
    fn is_current_search_job(&self, job_id: usize) -> bool {
        self.search_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.job_id == job_id && dialog.is_searching)
    }

    /// 点击搜索结果后打开文件并滚动到命中行。
    ///
    /// 业务意图：
    /// - 搜索结果不仅用于查看，还应成为跨文件定位入口。
    /// - 如果目标文件尚未打开，则新建 tab；如果已经打开，则直接激活并滚动。
    fn open_search_result(&mut self, result: SearchResultItem, context: &mut Context<Self>) {
        let source_key = result.source_key.clone();
        let line_index = result.line_index;

        if !self
            .open_tabs
            .iter()
            .any(|tab| tab.source_key == source_key)
        {
            self.open_log_file(result.source.clone(), context);
        }

        let Some(tab_index) = self
            .open_tabs
            .iter()
            .position(|tab| tab.source_key == source_key)
        else {
            return;
        };
        let tab_id = self.open_tabs[tab_index].id;
        let ready = matches!(self.open_tabs[tab_index].state, LogTabState::Ready { .. });
        self.open_tabs[tab_index].pending_scroll_to_line = Some(line_index);
        self.open_tabs[tab_index].highlighted_search_line = Some(line_index);
        self.activate_tab(tab_id);
        if ready {
            self.open_tabs[tab_index].pending_scroll_to_line = None;
            self.scroll_log_tab_to_line(tab_id, line_index);
        }
        context.notify();
    }

    /// 处理根视图鼠标移动。
    ///
    /// 业务意图：
    /// - 搜索对话框拖动和结果面板高度调整都可能跨过右侧正文、左侧树或工具栏区域，因此放到根视图统一处理。
    /// - 现有左右分割线和滚动条拖动仍在内容区处理，避免扩大它们的鼠标命中范围。
    fn handle_root_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.update_search_dialog_drag(event, window, context);
        self.update_search_results_resize_drag(event, window, context);
    }

    /// 处理根视图鼠标释放。
    ///
    /// 业务意图：
    /// - 搜索浮层拖动和结果面板 resize 都依赖鼠标释放清理临时状态。
    fn handle_root_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let had_drag = self.search_dialog_drag.take().is_some()
            || self.search_results_resize_drag.take().is_some();
        self.stop_log_text_selection(context);
        if had_drag {
            context.notify();
        }
    }

    /// 开始拖动搜索对话框。
    fn start_search_dialog_drag(&mut self, event: &MouseDownEvent) {
        let Some(dialog) = &self.search_dialog else {
            return;
        };
        self.search_dialog_drag = Some(SearchDialogDrag {
            cursor_offset_x: event.position.x - px(dialog.x),
            cursor_offset_y: event.position.y - px(dialog.y),
        });
        self.tab_context_menu = None;
        self.encoding_dropdown_menu = None;
    }

    /// 根据鼠标移动更新搜索对话框位置。
    fn update_search_dialog_drag(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.search_dialog_drag else {
            return;
        };
        if !event.dragging() {
            self.search_dialog_drag = None;
            context.notify();
            return;
        }

        let Some(dialog) = self.search_dialog.as_mut() else {
            self.search_dialog_drag = None;
            context.notify();
            return;
        };
        let viewport = window.viewport_size();
        let max_x = (f32::from(viewport.width) - SEARCH_DIALOG_WIDTH).max(0.0);
        let max_y = (f32::from(viewport.height) - 120.0).max(0.0);
        dialog.x = f32::from(event.position.x - drag.cursor_offset_x).clamp(0.0, max_x);
        dialog.y = f32::from(event.position.y - drag.cursor_offset_y).clamp(0.0, max_y);
        context.notify();
    }

    /// 开始调整搜索结果面板高度。
    fn start_search_results_resize(&mut self, event: &MouseDownEvent) {
        let Some(panel) = &self.search_results_panel else {
            return;
        };
        self.search_results_resize_drag = Some(SearchResultsResizeDrag {
            start_y: event.position.y,
            start_height: panel.height,
        });
        self.tab_context_menu = None;
        self.encoding_dropdown_menu = None;
    }

    /// 根据鼠标拖动更新搜索结果面板高度。
    fn update_search_results_resize_drag(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.search_results_resize_drag else {
            return;
        };
        if !event.dragging() {
            self.search_results_resize_drag = None;
            context.notify();
            return;
        }

        let Some(panel) = self.search_results_panel.as_mut() else {
            self.search_results_resize_drag = None;
            context.notify();
            return;
        };
        let content_height = (f32::from(window.viewport_size().height) - TOOLBAR_HEIGHT).max(1.0);
        let max_height =
            (content_height * SEARCH_RESULTS_PANEL_MAX_RATIO).max(SEARCH_RESULTS_PANEL_MIN_HEIGHT);
        let next_height = drag.start_height + f32::from(drag.start_y - event.position.y);
        panel.height = next_height.clamp(SEARCH_RESULTS_PANEL_MIN_HEIGHT, max_height);
        context.notify();
    }

    /// 根据真实加载节点类型返回目录树图标和颜色。
    ///
    /// 业务意图：
    /// - 将类型到视觉符号的映射集中管理，后续新增节点类型时不会散落在多个渲染分支里。
    fn loaded_log_tree_icon(kind: LogTreeEntryKind) -> (Icon, u32) {
        match kind {
            LogTreeEntryKind::Directory => (Icon::FolderOpen, 0x0969da),
            LogTreeEntryKind::File => (Icon::FileText, 0x57606a),
            LogTreeEntryKind::Archive => (Icon::FileArchive, 0x8250df),
            LogTreeEntryKind::Symlink => (Icon::FolderSymlink, 0x6b7280),
            LogTreeEntryKind::Error => (Icon::FileX, 0xcf222e),
        }
    }

    /// 渲染目录树节点右侧的补充信息。
    ///
    /// 业务意图：
    /// - 元信息用于展示文件大小或错误类别，让目录树信息更便于扫描。
    /// - 没有元信息的节点仍通过统一函数返回空占位，保持行模板简单稳定。
    ///
    /// 边界条件：
    /// - 当前元信息不参与排序、过滤或诊断，只是短文本展示。
    /// - 后续若元信息可能很长，需要定义截断、悬浮提示和优先级规则。
    fn render_log_tree_meta(meta: Option<&str>) -> gpui::Div {
        let meta_element = div()
            .flex_none()
            .text_xs()
            .text_color(rgb(0x8c959f))
            .child(meta.unwrap_or_default().to_string());

        if meta.is_some() {
            meta_element
        } else {
            meta_element.hidden()
        }
    }

    /// 开始拖动左右分栏分割线。
    ///
    /// 业务意图：
    /// - 用户按下分割线后进入调整模式，后续鼠标移动将改变左侧区域宽度。
    ///
    /// 边界条件：
    /// - 只响应鼠标左键，避免右键菜单或其它鼠标键误触发布局变化。
    /// - 按下瞬间不立即改变宽度，实际宽度在移动事件中按当前位置计算。
    fn start_resizing_splitter(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        _context: &mut Context<Self>,
    ) {
        self.is_resizing_splitter = true;
    }

    /// 开始拖动左侧目录树滚动条滑块。
    ///
    /// 业务意图：
    /// - 目录树节点较多时，用户需要可以直接拖动滚动条快速定位，而不是只能依赖滚轮。
    /// - 记录鼠标在滑块内的偏移，避免拖动开始时滑块位置突变。
    ///
    /// 边界条件：
    /// - 如果列表尚未完成测量或内容不足以滚动，则忽略本次按下。
    fn start_log_tree_scrollbar_drag(&mut self, event: &MouseDownEvent) {
        let Some(metrics) = Self::log_tree_scrollbar_metrics(&self.log_tree_scroll_handle) else {
            return;
        };
        if metrics.max_scroll <= px(0.0) {
            return;
        }
        let Some(viewport_top) = Self::uniform_list_viewport_axis_origin(
            &self.log_tree_scroll_handle,
            LogScrollbarAxis::Vertical,
        ) else {
            return;
        };

        self.log_tree_scrollbar_drag = Some(LogTreeScrollbarDrag {
            cursor_offset: event.position.y - viewport_top - metrics.thumb_start,
        });
        self.tab_context_menu = None;
        self.encoding_dropdown_menu = None;
    }

    /// 处理内容区鼠标移动事件。
    ///
    /// 业务意图：
    /// - 左右分割线拖动、左侧树滚动条拖动和日志滚动条拖动都依赖窗口级鼠标移动；统一入口可以保证这些交互在同一帧内得到更新。
    /// - 该方法只分发交互，不直接渲染 UI，便于后续继续追加其它拖动类操作。
    fn handle_content_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.resize_splitter(event, window, context);
        self.update_log_tree_scrollbar_drag(event, context);
        self.update_log_scrollbar_drag(event, context);
    }

    /// 处理内容区鼠标左键释放事件。
    ///
    /// 业务意图：
    /// - 用户释放鼠标时需要同时结束分割线拖动、左侧树滚动条拖动和日志滚动条拖动，避免后续移动继续改变布局或滚动位置。
    fn handle_content_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.stop_resizing_splitter(event, window, context);
        self.stop_log_tree_scrollbar_drag(context);
        self.stop_log_scrollbar_drag(context);
        self.stop_log_text_selection(context);
    }

    /// 根据鼠标移动更新左侧目录树滚动条拖动结果。
    ///
    /// 业务意图：
    /// - 自绘滚动条拖动必须写回目录树虚拟列表底层滚动偏移，才能和滚轮滚动、虚拟行渲染保持一致。
    /// - 只在鼠标左键仍按下时更新；如果释放发生在其它元素上，也会在下一次移动时清理拖动状态。
    fn update_log_tree_scrollbar_drag(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.log_tree_scrollbar_drag else {
            return;
        };
        if !event.dragging() {
            self.log_tree_scrollbar_drag = None;
            context.notify();
            return;
        }
        let Some(metrics) = Self::log_tree_scrollbar_metrics(&self.log_tree_scroll_handle) else {
            self.log_tree_scrollbar_drag = None;
            context.notify();
            return;
        };
        let Some(viewport_top) = Self::uniform_list_viewport_axis_origin(
            &self.log_tree_scroll_handle,
            LogScrollbarAxis::Vertical,
        ) else {
            self.log_tree_scrollbar_drag = None;
            context.notify();
            return;
        };

        let movable_length = (metrics.track_length - metrics.thumb_length).max(px(0.0));
        if metrics.max_scroll <= px(0.0) || movable_length <= px(0.0) {
            return;
        }

        let requested_thumb_start = event.position.y - viewport_top - drag.cursor_offset;
        let thumb_start =
            requested_thumb_start.clamp(metrics.track_start, metrics.track_start + movable_length);
        let scroll_offset =
            metrics.max_scroll * ((thumb_start - metrics.track_start) / movable_length);
        let base_scroll_handle = {
            // 克隆底层滚动句柄后释放 `RefCell` 借用，再写入偏移，避免测量状态借用和滚动状态更新交叉。
            self.log_tree_scroll_handle.0.borrow().base_handle.clone()
        };
        let current_offset = base_scroll_handle.offset();
        base_scroll_handle.set_offset(point(current_offset.x, -scroll_offset));
        context.notify();
    }

    /// 结束左侧目录树滚动条拖动。
    ///
    /// 业务意图：
    /// - 鼠标释放后清理拖动状态，避免普通鼠标移动继续改变目录树滚动位置。
    fn stop_log_tree_scrollbar_drag(&mut self, context: &mut Context<Self>) {
        if self.log_tree_scrollbar_drag.is_some() {
            self.log_tree_scrollbar_drag = None;
            context.notify();
        }
    }

    /// 拖动分割线时更新左侧内容区域宽度。
    ///
    /// 业务意图：
    /// - 将鼠标横坐标映射为左侧区域宽度，从而提供直接、可预期的分栏拖动体验。
    ///
    /// 边界条件：
    /// - 只有处于拖动状态时才更新宽度。
    /// - 宽度会被限制在左侧最小宽度和右侧最小可用空间之间，避免任一面板不可用。
    fn resize_splitter(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if !self.is_resizing_splitter {
            return;
        }

        self.left_panel_width = Self::clamp_left_panel_width(
            f32::from(event.position.x),
            f32::from(window.bounds().size.width),
        );
        context.notify();
    }

    /// 结束拖动左右分栏分割线。
    ///
    /// 业务意图：
    /// - 鼠标左键释放后退出调整模式，后续鼠标移动不再改变分栏宽度。
    ///
    /// 边界条件：
    /// - 当前不做宽度持久化，因此释放动作只影响内存状态。
    fn stop_resizing_splitter(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        _context: &mut Context<Self>,
    ) {
        self.is_resizing_splitter = false;
    }

    /// 将左侧区域宽度限制在可用范围内。
    ///
    /// 业务意图：
    /// - 分割线拖动必须保留左右两栏的基本可用空间。
    /// - 独立函数便于后续为该边界逻辑补充单元测试。
    ///
    /// 边界条件：
    /// - 如果窗口极窄导致右侧最小宽度无法满足，仍优先保证左侧不低于最小宽度。
    /// - 当前只使用窗口总宽度计算，工具栏高度不影响水平分割。
    fn clamp_left_panel_width(requested_width: f32, window_width: f32) -> f32 {
        let max_left_width = (window_width - SPLITTER_VISIBLE_WIDTH - RIGHT_PANEL_MIN_WIDTH)
            .max(LEFT_PANEL_MIN_WIDTH);
        requested_width.clamp(LEFT_PANEL_MIN_WIDTH, max_left_width)
    }

    /// 渲染未显示左侧目录树时的主内容提示。
    ///
    /// 业务意图：
    /// - 用户明确要求未加载日志时不显示左侧区域，因此主内容区需要占满窗口剩余空间。
    /// - 提示文案直接说明下一步动作，避免空白内容区让用户误以为界面卡住。
    ///
    /// 边界条件：
    /// - 加载中和加载失败也使用同一块主内容区提示，不提前显示左侧栏。
    /// - 当前提示不承载按钮，避免和顶部“加载日志”入口形成重复操作路径。
    fn render_primary_content_message(&self) -> impl IntoElement {
        let (icon, icon_color, title, description) = match &self.load_state {
            LogTreeLoadState::Empty => (
                Icon::FileText,
                0x57606a,
                "请先加载日志".to_string(),
                "点击左上角“加载日志”，选择日志文件、目录或压缩包。".to_string(),
            ),
            LogTreeLoadState::Loading { message } => (
                Icon::Loader,
                0x57606a,
                "正在加载日志".to_string(),
                message.clone(),
            ),
            LogTreeLoadState::Failed { message } => (
                Icon::FileX,
                0xcf222e,
                "加载日志失败".to_string(),
                message.clone(),
            ),
            LogTreeLoadState::Loaded(_) => (Icon::FileText, 0x57606a, String::new(), String::new()),
        };

        div()
            .id("primary-content-message")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .size_full()
            .px_4()
            .bg(rgb(0xffffff))
            .child(Self::render_lucide_icon(Some(icon), 32.0, 28.0, icon_color))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(0x24292f))
                    .child(title),
            )
            .child(div().text_sm().text_color(rgb(0x6b7280)).child(description))
    }

    /// 渲染右侧日志工作区。
    ///
    /// 业务意图：
    /// - 加载日志后但尚未打开任何文件时，右侧给出“点击左侧日志文件查看内容”的友好提示。
    /// - 打开文件后显示 tab 栏、编码切换工具条和只读日志正文。
    /// - 搜索结果面板打开后作为底部分栏参与布局，日志正文和滚动条高度会自动扣除面板高度。
    ///
    /// 边界条件：
    /// - 当前不持久化 tab，不支持拖拽重排，也不实现复制菜单。
    fn render_right_log_panel(&self, context: &mut Context<Self>) -> impl IntoElement {
        let content = if self.open_tabs.is_empty() {
            div()
                .id("right-log-panel-empty")
                .flex()
                .flex_1()
                .size_full()
                .child(self.render_loaded_right_empty_message())
        } else {
            div()
                .id("right-log-panel-tabs")
                .flex()
                .flex_col()
                .flex_1()
                .size_full()
                .child(self.render_log_tab_bar(context))
                .child(self.render_active_log_tab(context))
        };

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .child(content)
            .child(self.render_search_results_panel(context))
            .child(self.render_popup_dismiss_overlay(context))
            .child(self.render_tab_context_menu(context))
            .child(self.render_encoding_dropdown_menu(context))
    }

    /// 渲染搜索对话框浮层。
    ///
    /// 业务意图：
    /// - 搜索对话框由快捷键打开，独立于日志正文布局，可以自由拖动。
    /// - 对话框中直接展示搜索进度，让用户知道目录搜索仍在推进而不是界面卡住。
    fn render_search_dialog(
        &self,
        window: &Window,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(dialog) = &self.search_dialog else {
            return div().id("search-dialog-empty").hidden();
        };
        let can_search = self.search_can_start(dialog);

        div()
            .id("search-dialog")
            .absolute()
            .left(px(dialog.x))
            .top(px(dialog.y))
            .w(px(SEARCH_DIALOG_WIDTH))
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(0xd0d7de))
            .bg(rgb(0xffffff))
            .shadow_lg()
            .child(self.render_search_dialog_header(context))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .child(self.render_search_input(window, context))
                    .child(self.render_search_scope_controls(dialog.scope, context))
                    .child(self.render_search_directory_target(window, context))
                    .child(self.render_search_options_row(
                        dialog.case_sensitive,
                        can_search,
                        context,
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(if dialog.is_searching {
                                0x0969da
                            } else {
                                0x6b7280
                            }))
                            .child(dialog.message.clone()),
                    ),
            )
    }

    /// 渲染搜索对话框标题栏。
    ///
    /// 业务意图：
    /// - 标题栏承担拖动命中区；关闭按钮只关闭对话框，不清空结果面板。
    fn render_search_dialog_header(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("search-dialog-header")
            .flex()
            .items_center()
            .justify_between()
            .h(px(34.0))
            .px_3()
            .border_b_1()
            .border_color(rgb(0xe5e7eb))
            .cursor_move()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(0x24292f))
                    .child(Self::render_lucide_icon(
                        Some(Icon::Search),
                        15.0,
                        15.0,
                        0x57606a,
                    ))
                    .child("搜索"),
            )
            .child(
                div()
                    .id("search-dialog-close")
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(22.0))
                    .h(px(22.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(|button| button.bg(rgb(0xf6f8fa)))
                    .child(Self::render_lucide_icon(
                        Some(Icon::X),
                        13.0,
                        13.0,
                        0x57606a,
                    ))
                    .on_click(
                        context.listener(|view, _event: &ClickEvent, window, context| {
                            view.close_search_dialog(window, context);
                        }),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, _context| {
                    view.start_search_dialog_drag(event);
                }),
            )
    }

    /// 渲染搜索输入框。
    ///
    /// 业务意图：
    /// - 第一版只支持单行普通文本查询；输入框聚焦时处理字符、退格和删除。
    /// - 中文输入在 GPUI 平台层提交为 `key_char` 时会按完整字符串追加，范围高亮仍使用 UTF-8 边界。
    fn render_search_input(
        &self,
        window: &Window,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let query = self
            .search_dialog
            .as_ref()
            .map(|dialog| dialog.query.clone())
            .unwrap_or_default();
        let is_empty = query.is_empty();
        let input_focused = self.search_input_focus.is_focused(window);

        div()
            .id("search-input")
            .relative()
            .flex()
            .items_center()
            .gap_1()
            .h(px(SEARCH_INPUT_HEIGHT))
            .w_full()
            .px_2()
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(0xd0d7de))
            .bg(rgb(0xffffff))
            .track_focus(&self.search_input_focus)
            .key_context("search-input")
            .on_key_down(context.listener(Self::handle_search_input_key_down))
            .on_click(
                context.listener(|view, _event: &ClickEvent, window, context| {
                    if let Some(dialog) = view.search_dialog.as_mut() {
                        let cursor = dialog.query.len();
                        dialog.selection_range = cursor..cursor;
                    }
                    window.focus(&view.search_input_focus);
                    context.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .top(px(0.0))
                    .size_full()
                    .child(SearchInputImeElement {
                        view: context.entity(),
                        focus_handle: self.search_input_focus.clone(),
                    }),
            )
            .child(Self::render_search_cursor(
                input_focused && is_empty,
                "search-query-cursor-empty",
            ))
            .child(
                div()
                    .min_w_0()
                    .max_w_full()
                    .truncate()
                    .text_sm()
                    .text_color(rgb(if is_empty { 0x8c959f } else { 0x24292f }))
                    .child(if is_empty {
                        "输入搜索关键字".to_string()
                    } else {
                        query
                    }),
            )
            .child(Self::render_search_cursor(
                input_focused && !is_empty,
                "search-query-cursor-text",
            ))
    }

    /// 渲染搜索输入框光标。
    ///
    /// 业务意图：
    /// - 自绘输入框没有系统文本光标，必须提供明确的聚焦反馈。
    /// - 空输入时光标应位于占位提示前，而不是跟在提示文字后；非空时光标跟随当前文本末尾。
    ///
    /// 实现原因：
    /// - GPUI 当前没有复用系统输入控件，这里通过循环动画控制透明度，模拟常见编辑器光标闪烁。
    fn render_search_cursor(visible: bool, animation_id: &'static str) -> gpui::AnyElement {
        let cursor = div().flex_none().w(px(1.0)).h(px(16.0)).bg(rgb(0x0969da));

        if visible {
            cursor
                .with_animation(
                    animation_id,
                    Animation::new(Duration::from_millis(1000)).repeat(),
                    |cursor, delta| cursor.opacity(if delta < 0.55 { 1.0 } else { 0.0 }),
                )
                .into_any_element()
        } else {
            cursor.hidden().into_any_element()
        }
    }

    /// 渲染当前目录搜索目标输入区域。
    ///
    /// 业务意图：
    /// - 用户选择“当前目录”搜索时，需要明确看到搜索会落在哪个目录，并能把目标改成加载树中的子目录片段。
    /// - 该输入框只在目录搜索范围下显示，避免当前文件搜索时出现无意义的路径控件。
    fn render_search_directory_target(
        &self,
        window: &Window,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(dialog) = self.search_dialog.as_ref() else {
            return div().id("search-directory-target-empty").hidden();
        };
        if dialog.scope != SearchScope::CurrentDirectory {
            return div().id("search-directory-target-hidden").hidden();
        }

        let target = dialog.directory_target.clone();
        let is_empty = target.is_empty();
        let input_focused = self.search_directory_focus.is_focused(window);

        div()
            .id("search-directory-target")
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_xs().text_color(rgb(0x57606a)).child("搜索目录"))
            .child(
                div()
                    .id("search-directory-input")
                    .relative()
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(SEARCH_INPUT_HEIGHT))
                    .w_full()
                    .px_2()
                    .rounded(px(5.0))
                    .border_1()
                    .border_color(rgb(0xd0d7de))
                    .bg(rgb(0xffffff))
                    .track_focus(&self.search_directory_focus)
                    .key_context("search-directory-input")
                    .on_key_down(context.listener(Self::handle_search_directory_key_down))
                    .on_click(
                        context.listener(|view, _event: &ClickEvent, window, context| {
                            if let Some(dialog) = view.search_dialog.as_mut() {
                                let cursor = dialog.directory_target.len();
                                dialog.directory_selection_range = cursor..cursor;
                            }
                            window.focus(&view.search_directory_focus);
                            context.notify();
                        }),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(0.0))
                            .top(px(0.0))
                            .size_full()
                            .child(SearchInputImeElement {
                                view: context.entity(),
                                focus_handle: self.search_directory_focus.clone(),
                            }),
                    )
                    .child(Self::render_search_cursor(
                        input_focused && is_empty,
                        "search-directory-cursor-empty",
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .max_w_full()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(if is_empty { 0x8c959f } else { 0x24292f }))
                            .child(if is_empty {
                                "输入目录路径或子目录关键字".to_string()
                            } else {
                                target
                            }),
                    )
                    .child(Self::render_search_cursor(
                        input_focused && !is_empty,
                        "search-directory-cursor-text",
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x8c959f))
                    .child("仅在已加载目录树内过滤，不会额外扫描磁盘"),
            )
    }

    /// 处理搜索输入框按键。
    ///
    /// 边界条件：
    /// - 组合键不写入查询词，避免 `Ctrl+F`、复制粘贴快捷键等被当作普通文本。
    /// - 输入中的换行会被忽略，搜索框只保存单行查询。
    fn handle_search_input_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.handle_search_text_key_down(SearchTextInputKind::Query, event, context);
    }

    /// 处理搜索目录输入框按键。
    ///
    /// 业务意图：
    /// - 目录目标支持退格删除和 IME 提交；Enter 仍启动搜索，Esc 仍关闭对话框。
    fn handle_search_directory_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.handle_search_text_key_down(SearchTextInputKind::DirectoryTarget, event, context);
    }

    /// 处理搜索对话框中任一文本输入框的基础编辑按键。
    ///
    /// 业务意图：
    /// - 查询词和目录目标都只需要单行文本能力，复用同一套退格、删除和组合文本清理逻辑可避免两处行为不一致。
    /// - 普通字符输入交给 `EntityInputHandler`，这里不处理 `key_char`，从而保留中文 IME 的平台提交路径。
    fn handle_search_text_key_down(
        &mut self,
        input_kind: SearchTextInputKind,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        let Some(dialog) = self.search_dialog.as_mut() else {
            return;
        };
        let (text, selection_range, marked_range) = Self::search_text_state_mut(dialog, input_kind);

        match event.keystroke.key.as_str() {
            "backspace" => {
                if let Some(range) = marked_range.take().or_else(|| {
                    (selection_range.start != selection_range.end).then(|| selection_range.clone())
                }) {
                    text.replace_range(range.clone(), "");
                    *selection_range = range.start..range.start;
                } else if let Some((previous_index, _)) =
                    text[..selection_range.end].char_indices().next_back()
                {
                    text.replace_range(previous_index..selection_range.end, "");
                    *selection_range = previous_index..previous_index;
                }
                context.stop_propagation();
                context.notify();
            }
            "delete" => {
                context.stop_propagation();
            }
            "enter" | "escape" => {
                // Enter 和 Escape 由全局快捷键处理，保持搜索框只负责文本编辑。
            }
            _ => {}
        }
    }

    /// 返回指定输入槽位的可变文本、选择范围和组合范围。
    ///
    /// 实现原因：
    /// - Rust 需要在同一分支中同时借用三个字段，封装后可以让按键处理和 IME 回调共享同一套字段选择逻辑。
    fn search_text_state_mut(
        dialog: &mut SearchDialogState,
        input_kind: SearchTextInputKind,
    ) -> (&mut String, &mut Range<usize>, &mut Option<Range<usize>>) {
        match input_kind {
            SearchTextInputKind::Query => (
                &mut dialog.query,
                &mut dialog.selection_range,
                &mut dialog.marked_range,
            ),
            SearchTextInputKind::DirectoryTarget => (
                &mut dialog.directory_target,
                &mut dialog.directory_selection_range,
                &mut dialog.directory_marked_range,
            ),
        }
    }

    /// 返回指定输入槽位的只读文本、选择范围和组合范围。
    fn search_text_state(
        dialog: &SearchDialogState,
        input_kind: SearchTextInputKind,
    ) -> (&str, Range<usize>, Option<Range<usize>>) {
        match input_kind {
            SearchTextInputKind::Query => (
                &dialog.query,
                dialog.selection_range.clone(),
                dialog.marked_range.clone(),
            ),
            SearchTextInputKind::DirectoryTarget => (
                &dialog.directory_target,
                dialog.directory_selection_range.clone(),
                dialog.directory_marked_range.clone(),
            ),
        }
    }

    /// 根据当前窗口焦点判断平台输入应写入哪个搜索文本框。
    ///
    /// 边界条件：
    /// - 如果两个输入框都未聚焦，默认写入查询词，保证 `Ctrl+F` 打开后立即输入仍符合用户预期。
    fn active_search_text_input_kind(&self, window: &Window) -> SearchTextInputKind {
        if self.search_directory_focus.is_focused(window) {
            SearchTextInputKind::DirectoryTarget
        } else {
            SearchTextInputKind::Query
        }
    }

    /// 将 UTF-16 范围转换为搜索查询词内部可安全切片的 UTF-8 字节范围。
    ///
    /// 业务意图：
    /// - 平台输入协议按 UTF-16 字符计数，Rust `String` 必须按 UTF-8 字节边界切片。
    /// - 所有中文输入、组合文本替换和候选词提交都必须通过该转换，避免把多字节字符切坏。
    fn search_input_range_from_utf16(query: &str, range_utf16: Range<usize>) -> Range<usize> {
        let start = Self::search_input_byte_index_from_utf16(query, range_utf16.start);
        let end = Self::search_input_byte_index_from_utf16(query, range_utf16.end);
        start.min(end)..end.max(start)
    }

    /// 将搜索查询词内部 UTF-8 字节范围转换为平台输入协议需要的 UTF-16 范围。
    fn search_input_range_to_utf16(query: &str, range: Range<usize>) -> Range<usize> {
        let start = Self::search_input_utf16_offset_from_byte(query, range.start);
        let end = Self::search_input_utf16_offset_from_byte(query, range.end);
        start..end
    }

    /// 把 UTF-16 偏移映射到 UTF-8 字节边界。
    ///
    /// 边界条件：
    /// - 如果平台给出超过文本长度的偏移，统一夹到字符串末尾。
    /// - 如果偏移落在代理对或多字节字符内部，返回该字符起点，保证后续 `replace_range` 安全。
    fn search_input_byte_index_from_utf16(query: &str, target_utf16: usize) -> usize {
        let mut utf16_cursor = 0usize;
        for (byte_index, character) in query.char_indices() {
            if utf16_cursor >= target_utf16 {
                return byte_index;
            }
            utf16_cursor += character.len_utf16();
        }
        query.len()
    }

    /// 把 UTF-8 字节边界映射到 UTF-16 偏移。
    fn search_input_utf16_offset_from_byte(query: &str, byte_index: usize) -> usize {
        query
            .char_indices()
            .take_while(|(index, _)| *index < byte_index)
            .map(|(_, character)| character.len_utf16())
            .sum()
    }

    /// 清理平台输入文本，确保搜索框保持单行普通文本。
    fn sanitize_search_input_text(text: &str) -> String {
        text.replace(['\n', '\r'], "")
    }

    /// 渲染搜索范围切换控件。
    fn render_search_scope_controls(
        &self,
        selected_scope: SearchScope,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.render_search_scope_button(
                SearchScope::CurrentFile,
                selected_scope,
                context,
            ))
            .child(self.render_search_scope_button(
                SearchScope::CurrentDirectory,
                selected_scope,
                context,
            ))
    }

    /// 渲染单个搜索范围按钮。
    fn render_search_scope_button(
        &self,
        scope: SearchScope,
        selected_scope: SearchScope,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = scope == selected_scope;
        div()
            .id(SharedString::from(format!(
                "search-scope-{}",
                scope.label()
            )))
            .flex()
            .items_center()
            .justify_center()
            .h(px(26.0))
            .px_2()
            .rounded(px(4.0))
            .text_xs()
            .text_color(rgb(if selected { 0x0969da } else { 0x57606a }))
            .bg(rgb(if selected { 0xddf4ff } else { 0xf6f8fa }))
            .cursor_pointer()
            .hover(|button| button.bg(rgb(0xeaeef2)))
            .child(scope.label())
            .on_click(
                context.listener(move |view, _event: &ClickEvent, window, context| {
                    let default_directory_target = (scope == SearchScope::CurrentDirectory)
                        .then(|| view.active_search_directory_label())
                        .flatten();
                    if let Some(dialog) = view.search_dialog.as_mut() {
                        dialog.scope = scope;
                        if let Some(target) = default_directory_target
                            && dialog.directory_target.trim().is_empty()
                        {
                            dialog.directory_target = target;
                        }
                        if scope == SearchScope::CurrentDirectory {
                            let cursor = dialog.directory_target.len();
                            dialog.directory_selection_range = cursor..cursor;
                            dialog.directory_marked_range = None;
                        }
                        dialog.message = "输入关键字后按 Enter 或点击搜索".to_string();
                    }
                    if scope == SearchScope::CurrentDirectory {
                        window.focus(&view.search_directory_focus);
                    } else {
                        window.focus(&view.search_input_focus);
                    }
                    context.notify();
                }),
            )
    }

    /// 渲染大小写选项和搜索按钮。
    fn render_search_options_row(
        &self,
        case_sensitive: bool,
        can_search: bool,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(0x57606a))
                    .id("search-case-sensitive-toggle")
                    .cursor_pointer()
                    .child(Self::render_checkbox(case_sensitive))
                    .child("区分大小写")
                    .on_click(
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            if let Some(dialog) = view.search_dialog.as_mut() {
                                dialog.case_sensitive = !dialog.case_sensitive;
                            }
                            context.notify();
                        }),
                    ),
            )
            .child(
                div()
                    .id("search-submit-button")
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_1()
                    .h(px(28.0))
                    .px_3()
                    .rounded(px(5.0))
                    .text_xs()
                    .text_color(rgb(0xffffff))
                    .bg(rgb(if can_search { 0x0969da } else { 0x8c959f }))
                    .when(can_search, |button| {
                        button
                            .cursor_pointer()
                            .hover(|button| button.bg(rgb(0x0757b8)))
                    })
                    .when(!can_search, |button| button.opacity(0.72))
                    .child(Self::render_lucide_icon(
                        Some(Icon::Search),
                        12.0,
                        12.0,
                        0xffffff,
                    ))
                    .child("搜索")
                    .on_click(
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.start_search(context);
                        }),
                    ),
            )
    }

    /// 渲染搜索选项复选框。
    fn render_checkbox(checked: bool) -> gpui::Stateful<gpui::Div> {
        let checkbox = div()
            .id("search-checkbox")
            .flex()
            .items_center()
            .justify_center()
            .w(px(14.0))
            .h(px(14.0))
            .rounded(px(3.0))
            .border_1()
            .border_color(rgb(if checked { 0x0969da } else { 0xd0d7de }))
            .bg(rgb(if checked { 0x0969da } else { 0xffffff }));

        if checked {
            checkbox.child(Self::render_lucide_icon(
                Some(Icon::Check),
                10.0,
                10.0,
                0xffffff,
            ))
        } else {
            checkbox
        }
    }

    /// 判断搜索按钮是否具备基本启动条件。
    fn search_can_start(&self, dialog: &SearchDialogState) -> bool {
        !dialog.query.trim().is_empty() && self.active_tab_id.is_some()
    }

    /// 渲染右侧底部搜索结果面板。
    ///
    /// 业务意图：
    /// - 搜索结果作为右侧工作区底部分栏展示，避免覆盖日志正文和正文滚动条。
    /// - 面板高度可拖动，满足大量结果和保留正文上下文之间的取舍。
    fn render_search_results_panel(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(panel) = &self.search_results_panel else {
            return div().id("search-results-panel-empty").hidden();
        };
        let row_count = panel.rows.len();
        let panel_height = panel.height;
        let scroll_handle = panel.scroll_handle.clone();

        div()
            .id("search-results-panel")
            .h(px(panel_height))
            .w_full()
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(rgb(0xd0d7de))
            .bg(rgb(0xffffff))
            .shadow_lg()
            .child(self.render_search_results_resizer(context))
            .child(self.render_search_results_header(panel, context))
            .child(if row_count == 0 {
                self.render_search_results_empty(panel)
            } else {
                div()
                    .id("search-results-list-wrapper")
                    .flex()
                    .flex_1()
                    .w_full()
                    .overflow_hidden()
                    .child(
                        uniform_list(
                            "search-results-list",
                            row_count,
                            context.processor(
                                move |view, range: std::ops::Range<usize>, _window, context| {
                                    let rows = view
                                        .search_results_panel
                                        .as_ref()
                                        .map(|panel| {
                                            range
                                                .filter_map(|index| panel.rows.get(index).cloned())
                                                .collect::<Vec<_>>()
                                        })
                                        .unwrap_or_default();

                                    rows.into_iter()
                                        .map(|row| {
                                            view.render_search_results_panel_row(row, context)
                                        })
                                        .collect::<Vec<_>>()
                                },
                            ),
                        )
                        .size_full()
                        .track_scroll(scroll_handle),
                    )
            })
    }

    /// 将搜索历史记录展平成虚拟列表行。
    ///
    /// 业务意图：
    /// - 搜索历史记录头、命中结果和错误明细都需要在同一个可滚动区域内显示。
    /// - 仅为展开的记录生成明细行，可以让用户保留多次搜索历史而不被旧结果淹没。
    /// - 该函数只在结果数据或展开状态变化时调用，不能放到虚拟列表滚动渲染路径中反复执行。
    fn search_results_panel_rows_from_records(
        records: &[SearchHistoryRecord],
    ) -> Vec<SearchResultsPanelRow> {
        let mut rows = Vec::new();
        for (record_index, record) in records.iter().enumerate().rev() {
            rows.push(SearchResultsPanelRow::RecordHeader { record_index });

            if !record.expanded {
                continue;
            }

            if record.results.is_empty() && record.errors.is_empty() {
                rows.push(SearchResultsPanelRow::Empty { record_index });
                continue;
            }

            for group in Self::search_result_file_groups(record) {
                let expanded = record.expanded_file_keys.contains(&group.source_key);
                rows.push(SearchResultsPanelRow::FileHeader {
                    record_index,
                    source_key: group.source_key,
                    full_path: group.full_path,
                    result_count: group.result_indices.len(),
                });
                if expanded {
                    rows.extend(group.result_indices.into_iter().map(|result_index| {
                        SearchResultsPanelRow::Result {
                            record_index,
                            result_index,
                        }
                    }));
                }
            }
            rows.extend(
                (0..record.errors.len()).map(|error_index| SearchResultsPanelRow::Error {
                    record_index,
                    error_index,
                }),
            );
        }
        rows
    }

    /// 按文件来源对搜索命中进行稳定分组。
    ///
    /// 业务意图：
    /// - 同一文件的命中需要集中展示并可独立展开/收起。
    /// - 分组顺序按首次命中的顺序保留，避免每次渲染排序造成结果跳动。
    fn search_result_file_groups(record: &SearchHistoryRecord) -> Vec<SearchResultFileGroup> {
        let mut groups: Vec<SearchResultFileGroup> = Vec::new();

        for (result_index, result) in record.results.iter().enumerate() {
            if let Some(group) = groups
                .iter_mut()
                .find(|group| group.source_key == result.source_key)
            {
                group.result_indices.push(result_index);
                continue;
            }

            groups.push(SearchResultFileGroup {
                source_key: result.source_key.clone(),
                full_path: Self::search_result_source_full_path(&result.source),
                result_indices: vec![result_index],
            });
        }

        groups
    }

    /// 返回搜索结果来源的完整展示路径。
    ///
    /// 业务意图：
    /// - 文件分组行需要单行显示完整来源，帮助用户区分同名日志和压缩包内同名成员。
    /// - 这里仅用于 UI 展示；实际打开和定位仍依赖 `LogFileSource` 和稳定键。
    fn search_result_source_full_path(source: &LogFileSource) -> String {
        match source {
            LogFileSource::LocalFile { path } => path.display().to_string(),
            LogFileSource::ArchiveMember {
                archive_path,
                member_path,
                ..
            } => format!("{}/{}", archive_path.display(), member_path),
        }
    }

    /// 渲染搜索结果面板中的一行。
    ///
    /// 边界条件：
    /// - 后台搜索回调可能在渲染帧之间改变记录数量，因此每一行都必须重新通过下标安全读取。
    fn render_search_results_panel_row(
        &self,
        row: SearchResultsPanelRow,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(panel) = self.search_results_panel.as_ref() else {
            return div().id("search-results-row-missing-panel").hidden();
        };

        match row {
            SearchResultsPanelRow::RecordHeader { record_index } => {
                let Some(record) = panel.records.get(record_index) else {
                    return div().id("search-results-row-missing-record").hidden();
                };
                self.render_search_history_record_row(record_index, record, context)
            }
            SearchResultsPanelRow::FileHeader {
                record_index,
                source_key,
                full_path,
                result_count,
            } => self.render_search_file_group_row(
                record_index,
                source_key,
                full_path,
                result_count,
                context,
            ),
            SearchResultsPanelRow::Result {
                record_index,
                result_index,
            } => {
                let Some(result) = panel
                    .records
                    .get(record_index)
                    .and_then(|record| record.results.get(result_index))
                    .cloned()
                else {
                    return div().id("search-results-row-missing-result").hidden();
                };
                self.render_search_result_row(result, context)
            }
            SearchResultsPanelRow::Error {
                record_index,
                error_index,
            } => {
                let Some(error) = panel
                    .records
                    .get(record_index)
                    .and_then(|record| record.errors.get(error_index))
                    .cloned()
                else {
                    return div().id("search-results-row-missing-error").hidden();
                };
                self.render_search_error_row(record_index, error)
            }
            SearchResultsPanelRow::Empty { record_index } => {
                let Some(record) = panel.records.get(record_index) else {
                    return div().id("search-results-row-missing-empty").hidden();
                };
                self.render_search_record_empty_row(record_index, record)
            }
        }
    }

    /// 渲染搜索结果面板顶部拖拽条。
    fn render_search_results_resizer(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("search-results-resizer")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .h(px(SEARCH_RESULTS_PANEL_RESIZER_HEIGHT))
            .cursor_row_resize()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, _context| {
                    view.start_search_results_resize(event);
                }),
            )
    }

    /// 渲染搜索结果面板标题栏。
    fn render_search_results_header(
        &self,
        panel: &SearchResultsPanelState,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let search_count = panel.records.len();
        let result_count: usize = panel
            .records
            .iter()
            .map(|record| record.results.len())
            .sum();
        let error_count: usize = panel.records.iter().map(|record| record.errors.len()).sum();
        let running_count = panel
            .records
            .iter()
            .filter(|record| {
                !record.canceled && record.progress.searched_files < record.progress.total_files
            })
            .count();
        let summary = format!(
            "{} 次搜索 · {} 条命中 · {} 个错误{}",
            search_count,
            result_count,
            error_count,
            if running_count == 0 {
                String::new()
            } else {
                format!(" · {} 个进行中", running_count)
            }
        );

        div()
            .id("search-results-header")
            .flex()
            .items_center()
            .justify_between()
            .h(px(38.0))
            .pt(px(SEARCH_RESULTS_PANEL_RESIZER_HEIGHT))
            .px_3()
            .border_b_1()
            .border_color(rgb(0xe5e7eb))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(Self::render_lucide_icon(
                        Some(Icon::ListFilter),
                        14.0,
                        14.0,
                        0x57606a,
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(0x24292f))
                            .child("搜索结果"),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(rgb(0x6b7280))
                            .child(summary),
                    ),
            )
            .child(
                div()
                    .id("search-results-close")
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(24.0))
                    .h(px(24.0))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(|button| button.bg(rgb(0xf6f8fa)))
                    .child(Self::render_lucide_icon(
                        Some(Icon::X),
                        13.0,
                        13.0,
                        0x57606a,
                    ))
                    .on_click(
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.search_results_panel = None;
                            view.search_results_resize_drag = None;
                            context.notify();
                        }),
                    ),
            )
    }

    /// 渲染搜索结果空态。
    fn render_search_results_empty(
        &self,
        panel: &SearchResultsPanelState,
    ) -> gpui::Stateful<gpui::Div> {
        let message = if panel.records.is_empty() {
            "暂无搜索记录"
        } else {
            "暂无可显示的搜索结果"
        };

        div()
            .id("search-results-empty")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .flex_1()
            .text_sm()
            .text_color(rgb(0x6b7280))
            .child(Self::render_lucide_icon(
                Some(Icon::SearchX),
                26.0,
                24.0,
                0x8c959f,
            ))
            .child(message)
    }

    /// 渲染单条搜索历史记录摘要。
    ///
    /// 业务意图：
    /// - 历史记录行展示查询词、范围、目标目录和命中统计，点击可展开或收起明细。
    /// - 最新搜索默认展开，旧搜索保留但折叠，便于对比不同关键字而不占满结果区域。
    fn render_search_history_record_row(
        &self,
        record_index: usize,
        record: &SearchHistoryRecord,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let case_label = if record.case_sensitive {
            "区分大小写"
        } else {
            "忽略大小写"
        };
        let target_label = record
            .directory_target
            .as_ref()
            .filter(|target| !target.is_empty())
            .map(|target| format!(" · {}", target))
            .unwrap_or_default();
        let summary = format!(
            "{}{} · {} · {} · {}/{} 个文件 · {} 条命中 · {} 个错误",
            record.scope.label(),
            target_label,
            case_label,
            record.state_label(),
            record.progress.searched_files,
            record.progress.total_files,
            record.results.len(),
            record.errors.len()
        );
        let expanded = record.expanded;

        div()
            .id(SharedString::from(format!(
                "search-history-record-{record_index}"
            )))
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(SEARCH_RESULT_ROW_HEIGHT))
            .px_3()
            .border_b_1()
            .border_color(rgb(0xeaeef2))
            .bg(rgb(if expanded { 0xf6f8fa } else { 0xffffff }))
            .cursor_pointer()
            .hover(|row| row.bg(rgb(0xf6f8fa)))
            .child(Self::render_lucide_icon(
                Some(if expanded {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                }),
                14.0,
                14.0,
                0x57606a,
            ))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(0x24292f))
                            .child(record.query.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(0x6b7280))
                            .child(summary),
                    ),
            )
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    if let Some(panel) = view.search_results_panel.as_mut()
                        && let Some(record) = panel.records.get_mut(record_index)
                    {
                        record.expanded = !record.expanded;
                        panel.rows = Self::search_results_panel_rows_from_records(&panel.records);
                        panel.scroll_handle = UniformListScrollHandle::new();
                    }
                    context.notify();
                }),
            )
    }

    /// 渲染某次搜索记录下的文件分组行。
    ///
    /// 业务意图：
    /// - 文件分组行承载“这个文件有多少命中”的摘要，并提供展开/收起入口。
    /// - 命中明细行只显示行号和预览，避免每条结果重复展示同一个文件名。
    fn render_search_file_group_row(
        &self,
        record_index: usize,
        source_key: String,
        full_path: String,
        result_count: usize,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let expanded = self
            .search_results_panel
            .as_ref()
            .and_then(|panel| panel.records.get(record_index))
            .is_some_and(|record| record.expanded_file_keys.contains(&source_key));
        let source_key_for_click = source_key.clone();

        div()
            .id(SharedString::from(format!(
                "search-file-group-{record_index}-{}",
                source_key
            )))
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(SEARCH_RESULT_ROW_HEIGHT))
            .pl(px(28.0))
            .pr_3()
            .border_b_1()
            .border_color(rgb(0xf0f2f4))
            .bg(rgb(0xffffff))
            .cursor_pointer()
            .hover(|row| row.bg(rgb(0xf6f8fa)))
            .child(Self::render_lucide_icon(
                Some(if expanded {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                }),
                13.0,
                13.0,
                0x57606a,
            ))
            .child(Self::render_lucide_icon(
                Some(Icon::FileText),
                14.0,
                14.0,
                0x57606a,
            ))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .child(
                        div()
                            .whitespace_nowrap()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(0x24292f))
                            .child(full_path),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(rgb(0x6b7280))
                    .child(format!("{result_count} 条命中")),
            )
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    if let Some(panel) = view.search_results_panel.as_mut()
                        && let Some(record) = panel.records.get_mut(record_index)
                    {
                        if !record.expanded_file_keys.remove(&source_key_for_click) {
                            record
                                .expanded_file_keys
                                .insert(source_key_for_click.clone());
                        }
                        panel.rows = Self::search_results_panel_rows_from_records(&panel.records);
                        panel.scroll_handle = UniformListScrollHandle::new();
                    }
                    context.notify();
                }),
            )
    }

    /// 渲染单条搜索结果。
    ///
    /// 业务意图：
    /// - 文件名由上层文件分组展示，结果行只展示行号和命中预览；点击后打开对应文件并滚动到命中行。
    /// - 命中片段只用文字颜色高亮，保持和日志正文高亮策略一致。
    fn render_search_result_row(
        &self,
        result: SearchResultItem,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let highlight_style = gpui::HighlightStyle {
            color: Some(rgb(0xcf222e).into()),
            font_weight: Some(FontWeight::SEMIBOLD),
            background_color: None,
            ..Default::default()
        };
        let highlights = vec![(result.match_range.clone(), highlight_style)];
        let line_number = result.line_index + 1;
        let result_for_click = result.clone();

        div()
            .id(SharedString::from(format!(
                "search-result-{}-{}",
                result.source_key, line_number
            )))
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(SEARCH_RESULT_ROW_HEIGHT))
            .pl(px(56.0))
            .pr_3()
            .border_b_1()
            .border_color(rgb(0xf0f2f4))
            .cursor_pointer()
            .hover(|row| row.bg(rgb(0xf6f8fa)))
            .child(
                div()
                    .flex_none()
                    .w(px(70.0))
                    .text_xs()
                    .text_color(rgb(0x6b7280))
                    .child(format!("第 {} 行", line_number)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(LOG_VIEWER_FONT_SIZE))
                    .font_family(LOG_VIEWER_FONT_FAMILY)
                    .text_color(rgb(0x24292f))
                    .child(StyledText::new(result.line_text).with_highlights(highlights)),
            )
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.open_search_result(result_for_click.clone(), context);
                }),
            )
    }

    /// 渲染搜索中的单文件错误。
    ///
    /// 业务意图：
    /// - 目录搜索不能因为单个文件读取失败而中断；错误行让用户知道哪些文件没有被覆盖。
    fn render_search_error_row(
        &self,
        record_index: usize,
        error: SearchFileError,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "search-error-{record_index}-{}",
                error.file_name
            )))
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(SEARCH_RESULT_ROW_HEIGHT))
            .pl(px(32.0))
            .pr_3()
            .border_b_1()
            .border_color(rgb(0xf0f2f4))
            .child(Self::render_lucide_icon(
                Some(Icon::FileX),
                14.0,
                14.0,
                0xcf222e,
            ))
            .child(
                div()
                    .w(px(220.0))
                    .flex_none()
                    .truncate()
                    .text_sm()
                    .text_color(rgb(0x24292f))
                    .child(error.file_name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(rgb(0xcf222e))
                    .child(error.message),
            )
    }

    /// 渲染展开搜索记录时的空态明细行。
    fn render_search_record_empty_row(
        &self,
        record_index: usize,
        record: &SearchHistoryRecord,
    ) -> gpui::Stateful<gpui::Div> {
        let message = if record.canceled {
            "搜索已取消，取消前没有产生匹配结果"
        } else if record.progress.searched_files < record.progress.total_files {
            "正在搜索，请稍候..."
        } else {
            "没有找到匹配结果"
        };

        div()
            .id(SharedString::from(format!(
                "search-record-empty-{record_index}"
            )))
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .h(px(SEARCH_RESULT_ROW_HEIGHT))
            .pl(px(32.0))
            .pr_3()
            .border_b_1()
            .border_color(rgb(0xf0f2f4))
            .text_sm()
            .text_color(rgb(0x6b7280))
            .child(Self::render_lucide_icon(
                Some(Icon::SearchX),
                14.0,
                14.0,
                0x8c959f,
            ))
            .child(message)
    }

    /// 渲染弹层关闭遮罩。
    ///
    /// 业务意图：
    /// - 编码下拉框或 tab 右键菜单打开后，用户点击右侧工作区其它位置应关闭弹层。
    /// - 遮罩放在内容之上、菜单之下，既能接收空白区域点击，又不会挡住菜单项点击。
    fn render_popup_dismiss_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if self.tab_context_menu.is_none() && self.encoding_dropdown_menu.is_none() {
            return div().id("popup-dismiss-overlay-empty").hidden();
        }

        div()
            .id("popup-dismiss-overlay")
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .size_full()
            .on_click(
                context.listener(|view, _event: &ClickEvent, _window, context| {
                    view.tab_context_menu = None;
                    view.encoding_dropdown_menu = None;
                    context.notify();
                }),
            )
    }

    /// 渲染覆盖在右侧内容左缘的透明分割线拖动命中区。
    ///
    /// 业务意图：
    /// - 可见分割线只占 1px 布局宽度，右侧内容视觉上紧贴分割线。
    /// - 透明命中区覆盖在右侧内容最左侧，保留原有拖动易用性，避免额外白色拖动间隔。
    fn render_splitter_hit_overlay(&self, context: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("content-splitter-hit-overlay")
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .h_full()
            .w(px(SPLITTER_OVERLAY_HIT_WIDTH))
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(Self::start_resizing_splitter),
            )
    }

    /// 渲染已加载目录树但尚未打开文件时的右侧提示。
    ///
    /// 业务意图：
    /// - 用户完成“加载日志”后，下一步是从左侧树选择具体日志文件，右侧需要明确指引当前空态。
    fn render_loaded_right_empty_message(&self) -> impl IntoElement {
        div()
            .id("loaded-right-empty-message")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .size_full()
            .px_4()
            .bg(rgb(0xffffff))
            .child(Self::render_lucide_icon(
                Some(Icon::FileSearch),
                34.0,
                30.0,
                0x57606a,
            ))
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(0x24292f))
                    .child("点击左侧日志文件查看内容"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x6b7280))
                    .child("支持普通日志文件和压缩包内日志文件。"),
            )
    }

    /// 渲染右侧 tab 栏。
    ///
    /// 业务意图：
    /// - 每个打开的日志文件对应一个 tab，用户可以在多个日志之间快速切换。
    /// - tab 上的右键菜单提供关闭当前、关闭其他和关闭所有三个命令。
    /// - tab 标题完整展示，超出可视宽度时通过横向滚动和两侧箭头访问。
    fn render_log_tab_bar(&self, context: &mut Context<Self>) -> impl IntoElement {
        let tabs = self
            .open_tabs
            .iter()
            .map(|tab| {
                (
                    tab.id,
                    tab.title.clone(),
                    self.active_tab_id == Some(tab.id),
                )
            })
            .collect::<Vec<_>>();
        let tab_elements = tabs
            .into_iter()
            .map(|(tab_id, title, active)| self.render_log_tab(tab_id, title, active, context))
            .collect::<Vec<_>>();

        div()
            .id("log-tab-bar")
            .relative()
            .flex()
            .items_center()
            .h(px(LOG_TAB_BAR_HEIGHT))
            .flex_none()
            .overflow_hidden()
            .bg(rgb(0xf6f8fa))
            .border_b_1()
            .border_color(rgb(0xd0d7de))
            .child(self.render_tab_scroll_button(Icon::ChevronLeft, -LOG_TAB_SCROLL_STEP, context))
            .child(
                div()
                    .id("log-tab-scroll-viewport")
                    .flex()
                    .items_end()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_scroll()
                    .scrollbar_width(px(0.0))
                    .track_scroll(&self.tab_bar_scroll_handle)
                    .children(tab_elements),
            )
            .child(self.render_tab_scroll_button(Icon::ChevronRight, LOG_TAB_SCROLL_STEP, context))
    }

    /// 渲染 tab 栏横向滚动箭头。
    ///
    /// 业务意图：
    /// - 打开的日志较多时，用户可以通过左右箭头移动 tab 栏，而不需要依赖触控板横向滚动。
    /// - 按钮始终显示，符合用户“tab页签两边显示滚动箭头”的要求。
    fn render_tab_scroll_button(
        &self,
        icon: Icon,
        delta: f32,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("tab-scroll-{}", delta)))
            .flex()
            .items_center()
            .justify_center()
            .h_full()
            .w(px(LOG_TAB_SCROLL_BUTTON_WIDTH))
            .flex_none()
            .border_color(rgb(0xd0d7de))
            .bg(rgb(0xf6f8fa))
            .text_color(rgb(0x57606a))
            .cursor_pointer()
            .hover(|button| button.bg(rgb(0xffffff)).text_color(rgb(0x0969da)))
            .when(delta < 0.0, |button| button.border_r_1())
            .when(delta > 0.0, |button| button.border_l_1())
            .child(Self::render_lucide_icon(Some(icon), 14.0, 14.0, 0x57606a))
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.scroll_tab_bar(delta, context);
                }),
            )
    }

    /// 按给定像素距离横向滚动 tab 栏。
    ///
    /// 业务意图：
    /// - `ScrollHandle` 的偏移量向左滚动时为负值，因此向右箭头需要减少 x 偏移。
    /// - 滚动结果限制在 `[最大负偏移, 0]`，避免箭头点击后出现空白区域。
    fn scroll_tab_bar(&mut self, delta: f32, context: &mut Context<Self>) {
        let current_offset = self.tab_bar_scroll_handle.offset();
        let max_scroll = self.tab_bar_scroll_handle.max_offset().width;
        let next_x = (current_offset.x - px(delta)).clamp(-max_scroll, px(0.0));
        self.tab_bar_scroll_handle
            .set_offset(point(next_x, current_offset.y));
        self.tab_context_menu = None;
        self.encoding_dropdown_menu = None;
        context.notify();
    }

    /// 渲染单个日志 tab。
    ///
    /// 业务意图：
    /// - 左键激活 tab，右键打开 tab 操作菜单。
    /// - tab 标题完整展示，不做省略；关闭按钮放在右侧，便于快速收起单个日志。
    fn render_log_tab(
        &self,
        tab_id: usize,
        title: String,
        active: bool,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let background = if active { 0xffffff } else { 0xf6f8fa };
        div()
            .id(SharedString::from(format!("log-tab-{}", tab_id)))
            .flex()
            .items_center()
            .h(px(LOG_TAB_BAR_HEIGHT - 1.0))
            .min_w(px(96.0))
            .flex_none()
            .px_3()
            .gap_1()
            .border_r_1()
            .border_color(rgb(0xd0d7de))
            .bg(rgb(background))
            .text_sm()
            .text_color(rgb(if active { 0x24292f } else { 0x57606a }))
            .cursor_pointer()
            .hover(|tab| tab.bg(rgb(0xffffff)))
            .child(Self::render_lucide_icon(
                Some(Icon::FileText),
                14.0,
                13.0,
                0x57606a,
            ))
            .child(div().flex_none().whitespace_nowrap().child(title))
            .child(self.render_tab_close_button(tab_id, context))
            .on_click(
                context.listener(move |view, event: &ClickEvent, _window, context| {
                    if event.standard_click() {
                        view.activate_tab(tab_id);
                        context.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.open_tab_context_menu(
                        tab_id,
                        f32::from(event.position.x),
                        f32::from(event.position.y),
                        context,
                    );
                }),
            )
    }

    /// 渲染 tab 右侧关闭按钮。
    ///
    /// 业务意图：
    /// - 每个 tab 都提供明确的关闭入口，避免用户只能通过右键菜单关闭。
    /// - 关闭后按 `close_tab` 的统一规则切换激活 tab，并清理相关弹层状态。
    fn render_tab_close_button(
        &self,
        tab_id: usize,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("log-tab-close-{}", tab_id)))
            .flex()
            .items_center()
            .justify_center()
            .w(px(LOG_TAB_CLOSE_BUTTON_WIDTH))
            .h(px(LOG_TAB_CLOSE_BUTTON_WIDTH))
            .flex_none()
            .rounded(px(3.0))
            .text_color(rgb(0x6b7280))
            .hover(|button| button.bg(rgb(0xeaeef2)).text_color(rgb(0x24292f)))
            .child(Self::render_lucide_icon(
                Some(Icon::X),
                12.0,
                12.0,
                0x6b7280,
            ))
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.close_tab(tab_id);
                    view.tab_context_menu = None;
                    view.encoding_dropdown_menu = None;
                    context.notify();
                }),
            )
    }

    /// 渲染当前激活 tab 的内容。
    ///
    /// 业务意图：
    /// - 激活 tab 统一显示编码工具条；正文根据读取状态显示加载、错误或日志虚拟列表。
    fn render_active_log_tab(&self, context: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let Some(active_tab_id) = self.active_tab_id else {
            return div()
                .id("active-log-tab-empty")
                .flex()
                .flex_1()
                .child(self.render_loaded_right_empty_message());
        };
        let Some(tab) = self.open_tabs.iter().find(|tab| tab.id == active_tab_id) else {
            return div()
                .id("active-log-tab-missing")
                .flex()
                .flex_1()
                .child(self.render_loaded_right_empty_message());
        };

        div()
            .id("active-log-tab")
            .flex()
            .flex_col()
            .flex_1()
            .overflow_hidden()
            .child(self.render_log_document_toolbar(tab, context))
            .child(self.render_log_tab_body(tab, context))
    }

    /// 渲染当前日志文档工具条。
    ///
    /// 业务意图：
    /// - 左侧不再放置独立编码控件，避免挤占日志正文起点上方空间。
    /// - 工具条右侧把实际编码直接渲染成编码选择器，同时展示来源、识别方式和行数。
    fn render_log_document_toolbar(
        &self,
        tab: &OpenLogTab,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let source_kind = match &tab.source {
            LogFileSource::LocalFile { .. } => "本地文件",
            LogFileSource::ArchiveMember { .. } => "压缩包内文件",
        };
        let encoding_button_label = match &tab.state {
            LogTabState::Ready { document } => document.encoding.label(),
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => tab.encoding_choice.label(),
        };
        let status = match &tab.state {
            LogTabState::Ready { document } => {
                // 状态栏展示编码来源和行数；实际编码本身由右侧内联编码选择器负责显示和切换。
                let encoding_source = if document.detected_automatically {
                    "自动识别"
                } else {
                    "手动选择"
                };
                let replacement_warning = if document.had_replacements {
                    " · 含替换字符"
                } else {
                    ""
                };
                format!(
                    "{} · {} 行{}",
                    encoding_source,
                    document.line_count(),
                    replacement_warning
                )
            }
            LogTabState::Loading { .. } => "正在处理".to_string(),
            LogTabState::Failed { .. } => "需要选择正确编码或重新加载".to_string(),
        };

        div()
            .id("log-document-toolbar")
            .flex()
            .items_center()
            .justify_end()
            .h(px(LOG_DOCUMENT_TOOLBAR_HEIGHT))
            .flex_none()
            .px_3()
            .gap_2()
            .bg(rgb(0xffffff))
            .border_b_1()
            .border_color(rgb(0xe5e7eb))
            .child(
                div()
                    .id("log-document-status-group")
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(rgb(0x6b7280))
                    .child(div().flex_none().child(source_kind))
                    .child(Self::render_status_separator())
                    .child(self.render_encoding_selector(
                        tab.id,
                        encoding_button_label,
                        tab.raw_bytes.is_some(),
                        context,
                    ))
                    .child(Self::render_status_separator())
                    .child(div().min_w_0().truncate().child(status)),
            )
    }

    /// 渲染编码切换控件。
    ///
    /// 业务意图：
    /// - 编码选项改为紧凑下拉框，避免多个编码按钮平铺占用顶部工具条。
    /// - “自动”重新执行检测流程，手动选项按指定编码重新解码原始字节。
    /// - 原始字节未读取完成前保持禁用，避免用户提前选择编码导致 UI 状态和实际解码结果不一致。
    fn render_encoding_selector(
        &self,
        tab_id: usize,
        display_label: &'static str,
        enabled: bool,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("encoding-selector")
            .flex()
            .items_center()
            .flex_none()
            .child(
                div()
                    .id(SharedString::from(format!(
                        "encoding-dropdown-button-{}",
                        tab_id
                    )))
                    .flex()
                    .items_center()
                    .justify_between()
                    .w(px(ENCODING_DROPDOWN_BUTTON_WIDTH))
                    .h(px(ENCODING_DROPDOWN_BUTTON_HEIGHT))
                    .px_2()
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(0xd0d7de))
                    .bg(rgb(0xffffff))
                    .text_xs()
                    .text_color(rgb(if enabled { 0x24292f } else { 0x8c959f }))
                    .when(enabled, |button| {
                        button
                            .cursor_pointer()
                            .hover(|button| button.border_color(rgb(0x0969da)).bg(rgb(0xf6f8fa)))
                    })
                    .when(!enabled, |button| button.opacity(0.62))
                    .child(display_label)
                    .child(Self::render_lucide_icon(
                        Some(Icon::ChevronDown),
                        12.0,
                        12.0,
                        0x57606a,
                    ))
                    .on_click(context.listener(
                        move |view, event: &ClickEvent, _window, context| {
                            if enabled {
                                let position = event.position();
                                view.toggle_encoding_dropdown(
                                    tab_id,
                                    f32::from(position.x),
                                    f32::from(position.y),
                                    context,
                                );
                            }
                        },
                    )),
            )
    }

    /// 渲染右侧状态栏中的点状分隔符。
    ///
    /// 业务意图：
    /// - 来源、编码选择器、识别方式和行数属于同一组状态信息，用轻量分隔符维持可读性。
    /// - 单独函数可以避免多个位置重复硬编码颜色和文本。
    fn render_status_separator() -> gpui::Div {
        div().flex_none().text_color(rgb(0x8c959f)).child("·")
    }

    /// 切换编码下拉框展开状态。
    ///
    /// 业务意图：
    /// - 同一个 tab 再次点击编码框会收起菜单；点击其它 tab 的编码框会切换到新的菜单。
    /// - 打开编码菜单时收起 tab 右键菜单，避免两个弹层同时争抢点击区域。
    fn toggle_encoding_dropdown(
        &mut self,
        tab_id: usize,
        window_x: f32,
        window_y: f32,
        context: &mut Context<Self>,
    ) {
        if !self
            .open_tabs
            .iter()
            .any(|tab| tab.id == tab_id && tab.raw_bytes.is_some())
        {
            // 编码菜单必须等原始字节可用后才能打开，避免用户在加载过程中选择编码但无法立即解析。
            self.encoding_dropdown_menu = None;
            self.tab_context_menu = None;
            context.notify();
            return;
        }

        let is_same_menu_open = self
            .encoding_dropdown_menu
            .as_ref()
            .is_some_and(|menu| menu.tab_id == tab_id);
        self.encoding_dropdown_menu = if is_same_menu_open {
            None
        } else {
            Some(EncodingDropdownMenu {
                tab_id,
                x: Self::encoding_dropdown_menu_x(self.left_panel_width, window_x),
                y: Self::encoding_dropdown_menu_y(window_y),
            })
        };
        self.tab_context_menu = None;
        context.notify();
    }

    /// 根据点击位置计算编码菜单在右侧工作区内的横坐标。
    ///
    /// 业务意图：
    /// - 编码选择器位于右侧状态栏，状态栏内容会随来源类型、识别状态和行数变化。
    /// - GPUI 当前没有直接暴露按钮布局矩形给点击监听，因此用点击点近似还原按钮左缘，让菜单跟随按钮打开。
    ///
    /// 边界条件：
    /// - 点击按钮文字或箭头会带来几个像素的偏差，但菜单仍紧邻编码按钮，不会回到旧版左侧固定位置。
    fn encoding_dropdown_menu_x(left_panel_width: f32, window_x: f32) -> f32 {
        let panel_x = (window_x - left_panel_width - SPLITTER_VISIBLE_WIDTH).max(0.0);
        (panel_x - ENCODING_DROPDOWN_BUTTON_WIDTH / 2.0).max(0.0)
    }

    /// 根据点击位置计算编码菜单在右侧工作区内的纵坐标。
    ///
    /// 业务意图：
    /// - 菜单应出现在编码按钮下方；点击位置通常位于按钮中部，因此加上半个按钮高度和固定间隔。
    fn encoding_dropdown_menu_y(window_y: f32) -> f32 {
        (window_y - TOOLBAR_HEIGHT
            + ENCODING_DROPDOWN_BUTTON_HEIGHT / 2.0
            + ENCODING_DROPDOWN_MENU_GAP)
            .max(0.0)
    }

    /// 返回编码下拉框的手动编码选项。
    ///
    /// 业务意图：
    /// - 打开日志时仍默认自动识别；下拉菜单只承载用户主动纠正编码的手动选项。
    /// - 用户要求去除菜单中的“自动”，因此这里不再把 `EncodingChoice::Auto` 作为可点击项渲染。
    /// - UI 渲染和点击处理共享同一组选项，避免菜单项和编码解析能力不一致。
    fn encoding_choices() -> Vec<EncodingChoice> {
        LogTextEncoding::manual_options()
            .into_iter()
            .map(EncodingChoice::Manual)
            .collect()
    }

    /// 渲染编码下拉菜单。
    ///
    /// 业务意图：
    /// - 菜单作为右侧工作区内部弹层绘制，视觉上贴在当前 tab 的编码选择框下方。
    /// - 选择任一编码后复用当前 tab 保存的原始字节重新解析，并立即收起菜单。
    fn render_encoding_dropdown_menu(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = &self.encoding_dropdown_menu else {
            return div().id("encoding-dropdown-menu-empty").hidden();
        };
        let Some(tab) = self.open_tabs.iter().find(|tab| tab.id == menu.tab_id) else {
            return div().id("encoding-dropdown-menu-missing").hidden();
        };
        let tab_id = tab.id;
        let selected_choice = tab.encoding_choice;
        let menu_items = Self::encoding_choices()
            .into_iter()
            .map(|choice| {
                self.render_encoding_dropdown_item(
                    tab_id,
                    choice,
                    choice == selected_choice,
                    context,
                )
            })
            .collect::<Vec<_>>();

        div()
            .id("encoding-dropdown-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(ENCODING_DROPDOWN_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(0xd0d7de))
            .bg(rgb(0xffffff))
            .shadow_lg()
            .children(menu_items)
    }

    /// 渲染编码下拉菜单单项。
    ///
    /// 业务意图：
    /// - 当前选中编码通过浅蓝背景标识；点击其它项会触发重新解码。
    fn render_encoding_dropdown_item(
        &self,
        tab_id: usize,
        choice: EncodingChoice,
        selected: bool,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let item = div()
            .id(SharedString::from(format!(
                "encoding-dropdown-item-{}-{}",
                tab_id,
                choice.label()
            )))
            .flex()
            .items_center()
            .justify_between()
            .h(px(ENCODING_DROPDOWN_ITEM_HEIGHT))
            .px_2()
            .text_xs()
            .text_color(rgb(if selected { 0x0969da } else { 0x24292f }))
            .bg(rgb(if selected { 0xddf4ff } else { 0xffffff }))
            .cursor_pointer()
            .hover(|item| item.bg(rgb(0xf6f8fa)).text_color(rgb(0x0969da)))
            .child(choice.label());

        let item = if selected {
            item.child(Self::render_lucide_icon(
                Some(Icon::Check),
                12.0,
                12.0,
                0x0969da,
            ))
        } else {
            item
        };

        item.on_click(
            context.listener(move |view, _event: &ClickEvent, _window, context| {
                view.select_tab_encoding(tab_id, choice, context);
            }),
        )
    }

    /// 渲染当前 tab 正文区域。
    ///
    /// 业务意图：
    /// - 加载中、失败和成功三种状态都在右侧主体区域给出明确反馈。
    fn render_log_tab_body(
        &self,
        tab: &OpenLogTab,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        match &tab.state {
            LogTabState::Loading { message } => {
                self.render_log_tab_state_message(Icon::Loader, 0x57606a, "正在打开日志", message)
            }
            LogTabState::Failed { message } => {
                self.render_log_tab_state_message(Icon::FileX, 0xcf222e, "日志打开失败", message)
            }
            LogTabState::Ready { document } => {
                self.render_log_document_viewer(tab, document, context)
            }
        }
    }

    /// 渲染 tab 状态提示。
    ///
    /// 业务意图：
    /// - 读取失败或自动编码失败时不能只显示空白，必须让用户知道下一步可以切换编码或重新加载。
    fn render_log_tab_state_message(
        &self,
        icon: Icon,
        icon_color: u32,
        title: &str,
        message: &str,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("log-tab-state-message")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .flex_1()
            .size_full()
            .px_4()
            .bg(rgb(0xffffff))
            .child(Self::render_lucide_icon(Some(icon), 32.0, 28.0, icon_color))
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(0x24292f))
                    .child(title.to_string()),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x6b7280))
                    .text_center()
                    .child(message.to_string()),
            )
    }

    /// 渲染只读日志正文查看器。
    ///
    /// 业务意图：
    /// - 使用 GPUI `uniform_list` 只渲染可见日志行，避免大文件滚动时为全部行创建元素。
    /// - 行号和正文分栏显示，正文按日志级别做轻量高亮。
    fn render_log_document_viewer(
        &self,
        tab: &OpenLogTab,
        document: &DecodedLogDocument,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let tab_id = tab.id;
        let line_count = document.line_count();
        let scroll_handle = tab.scroll_handle.clone();
        let line_number_width = Self::log_viewer_line_number_width(line_count);
        let horizontal_measure_line_index = document.longest_line_index;
        let row_scroll_handle = scroll_handle.clone();

        let viewer = div()
            .id(SharedString::from(format!("log-viewer-{}", tab_id)))
            .flex()
            .flex_col()
            .flex_1()
            .overflow_hidden()
            .bg(rgb(0xffffff));

        let viewer = if let Some(warning) = &document.warning {
            viewer.child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .text_xs()
                    .text_color(rgb(0x9a6700))
                    .bg(rgb(0xfff8c5))
                    .border_b_1()
                    .border_color(rgb(0xf0d98c))
                    .child(warning.clone()),
            )
        } else {
            viewer
        };

        viewer.child(
            div()
                .id("log-viewer-body")
                .relative()
                .flex()
                .flex_1()
                .overflow_hidden()
                .bg(rgb(0xffffff))
                .child(
                    div()
                        .absolute()
                        .left(px(0.0))
                        .top(px(0.0))
                        .h_full()
                        .w(px(line_number_width))
                        .bg(rgb(0xf6f8fa))
                        .border_r_1()
                        .border_color(rgb(0xe5e7eb)),
                )
                .child(
                    uniform_list(
                        "log-document-virtual-list",
                        line_count,
                        context.processor(
                            move |view, range: std::ops::Range<usize>, _window, context| {
                                let lines = view
                                    .open_tabs
                                    .iter()
                                    .find(|tab| tab.id == tab_id)
                                    .and_then(|tab| match &tab.state {
                                        LogTabState::Ready { document } => {
                                            Some((
                                                document,
                                                tab.highlighted_search_line,
                                                tab.text_selection.clone(),
                                            ))
                                        }
                                        LogTabState::Loading { .. }
                                        | LogTabState::Failed { .. } => None,
                                    })
                                    .map(|(document, highlighted_search_line, text_selection)| {
                                        range
                                            .filter_map(|index| {
                                                document.lines.get(index).map(|line| {
                                                    let precomputed = document
                                                        .precomputed_highlights
                                                        .as_ref()
                                                        .and_then(|highlights| {
                                                            highlights.lines.get(index)
                                                        });
                                                    let mut line_highlights = highlight_line(
                                                        document.highlight_mode,
                                                        line,
                                                        precomputed,
                                                    );
                                                    if let Some(selection) = &text_selection
                                                        && let Some(range) =
                                                            Self::selected_byte_range_for_line(
                                                                selection, index, line,
                                                            )
                                                    {
                                                        // `StyledText::with_highlights` 要求传入的高亮范围有序且不重叠。
                                                        // 日志语法高亮和选区高亮经常覆盖同一段时间戳、等级或线程名，因此必须先拆分合并，
                                                        // 让选区背景和原有文字颜色同时保留，避免选中文本时渲染错位。
                                                        line_highlights = gpui::combine_highlights(
                                                            line_highlights,
                                                            [(
                                                                range,
                                                                Self::log_text_selection_highlight_style(),
                                                            )],
                                                        )
                                                        .collect();
                                                    }
                                                    (
                                                        index,
                                                        line.clone(),
                                                        line_highlights,
                                                        highlighted_search_line == Some(index),
                                                    )
                                                })
                                            })
                                            .collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default();

                                lines
                                    .into_iter()
                                    .map(|(index, line, highlights, search_highlighted)| {
                                        let horizontal_line_number_offset =
                                            -row_scroll_handle.0.borrow().base_handle.offset().x;
                                        Self::render_log_line(LogLineRenderData {
                                            tab_id,
                                            line_index: index,
                                            line,
                                            highlights,
                                            line_number_width,
                                            horizontal_line_number_offset,
                                            search_highlighted,
                                        }, context)
                                    })
                                    .collect::<Vec<_>>()
                            },
                        ),
                    )
                    .with_width_from_item(Some(horizontal_measure_line_index))
                    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                    .size_full()
                    .track_scroll(scroll_handle.clone()),
                )
                .child(self.render_log_vertical_scrollbar(
                    tab_id,
                    &scroll_handle,
                    line_count,
                    context,
                ))
                .child(self.render_log_horizontal_scrollbar(
                    tab_id,
                    &scroll_handle,
                    line_number_width,
                    context,
                )),
        )
    }

    /// 根据总行数计算行号列宽度。
    ///
    /// 业务意图：
    /// - 小文件不需要占用上一版 68px 宽度；行数增长时再按数字位数适度增加。
    /// - 该宽度同时用于行号背景和每一行的行号单元格，保证未填满高度时背景也能对齐。
    fn log_viewer_line_number_width(line_count: usize) -> f32 {
        let digit_count = line_count.max(1).to_string().len() as f32;
        (digit_count * LOG_VIEWER_LINE_NUMBER_DIGIT_WIDTH + LOG_VIEWER_LINE_NUMBER_PADDING_WIDTH)
            .clamp(
                LOG_VIEWER_LINE_NUMBER_MIN_WIDTH,
                LOG_VIEWER_LINE_NUMBER_MAX_WIDTH,
            )
    }

    /// 渲染日志正文的纵向可见滚动条。
    ///
    /// 业务意图：
    /// - GPUI `uniform_list` 已经支持滚轮滚动，但长日志需要稳定可见的滚动位置提示和鼠标拖动入口。
    /// - 滑块与列表共享同一个 `UniformListScrollHandle`，拖动时直接写入列表底层滚动偏移，不复制日志行数据。
    fn render_log_vertical_scrollbar(
        &self,
        tab_id: usize,
        scroll_handle: &UniformListScrollHandle,
        line_count: usize,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) = Self::log_vertical_scrollbar_metrics(scroll_handle)
            .or_else(|| Self::fallback_log_vertical_scrollbar_metrics(line_count))
        else {
            return div().id("log-vertical-scrollbar-empty").hidden();
        };

        div()
            .id(SharedString::from(format!(
                "log-vertical-scrollbar-{}",
                tab_id
            )))
            .absolute()
            .top(metrics.thumb_start)
            .right(px(LOG_VIEWER_SCROLLBAR_PADDING))
            .w(px(LOG_VIEWER_SCROLLBAR_WIDTH))
            .h(metrics.thumb_length)
            .rounded(px(LOG_VIEWER_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(0xc9d1d9))
            .cursor_pointer()
            .hover(|thumb| thumb.bg(rgb(0x8c959f)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.start_log_scrollbar_drag(tab_id, LogScrollbarAxis::Vertical, event);
                    context.notify();
                }),
            )
    }

    /// 渲染日志正文的横向可见滚动条。
    ///
    /// 业务意图：
    /// - 日志行可能包含长 JSON、堆栈或配置片段，正文宽度超过视口时必须提供横向滚动提示。
    /// - 横向滚动条只在实际测量到内容超宽后显示，避免普通短日志底部出现无效控件。
    fn render_log_horizontal_scrollbar(
        &self,
        tab_id: usize,
        scroll_handle: &UniformListScrollHandle,
        line_number_width: f32,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) =
            Self::log_horizontal_scrollbar_metrics(scroll_handle, line_number_width)
        else {
            return div().id("log-horizontal-scrollbar-empty").hidden();
        };

        div()
            .id(SharedString::from(format!(
                "log-horizontal-scrollbar-{}",
                tab_id
            )))
            .absolute()
            .left(metrics.thumb_start)
            .bottom(px(LOG_VIEWER_SCROLLBAR_PADDING))
            .w(metrics.thumb_length)
            .h(px(LOG_VIEWER_SCROLLBAR_WIDTH))
            .rounded(px(LOG_VIEWER_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(0xc9d1d9))
            .cursor_pointer()
            .hover(|thumb| thumb.bg(rgb(0x8c959f)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.start_log_scrollbar_drag(tab_id, LogScrollbarAxis::Horizontal, event);
                    context.notify();
                }),
            )
    }

    /// 计算日志纵向滚动条滑块位置和高度。
    ///
    /// 业务意图：
    /// - 使用 `UniformListScrollHandle` 上一次布局记录的视口高度、内容高度和滚动偏移估算滑块。
    /// - 如果内容没有超过视口，则不显示滚动条，避免空文件或短日志出现无意义控件。
    fn log_vertical_scrollbar_metrics(
        scroll_handle: &UniformListScrollHandle,
    ) -> Option<LogScrollbarMetrics> {
        let state = scroll_handle.0.borrow();
        let size = state.last_item_size?;
        let viewport_height = size.item.height;
        let content_height = size.contents.height;
        if viewport_height <= px(0.0) || content_height <= viewport_height {
            return None;
        }

        let max_scroll = content_height - viewport_height;
        let scroll_top = (-state.base_handle.offset().y).clamp(px(0.0), max_scroll);
        let track_start = px(LOG_VIEWER_SCROLLBAR_PADDING);
        let track_length = (viewport_height - track_start * 2.0).max(px(1.0));
        let min_thumb_length = px(LOG_VIEWER_SCROLLBAR_MIN_THUMB_HEIGHT).min(track_length);
        let thumb_length = (viewport_height * (viewport_height / content_height))
            .clamp(min_thumb_length, track_length);
        let movable_length = (track_length - thumb_length).max(px(0.0));
        let thumb_start = track_start + movable_length * (scroll_top / max_scroll);

        Some(LogScrollbarMetrics {
            thumb_start,
            thumb_length,
            track_start,
            track_length,
            max_scroll,
        })
    }

    /// 计算日志横向滚动条滑块位置和宽度。
    ///
    /// 业务意图：
    /// - 横向滚动基于 `uniform_list` 的非受限宽度测量结果，内容宽于视口时才显示自绘滚动条。
    /// - 轨道从正文区域开始，避开左侧行号列，视觉上更接近日志正文的实际可滚动内容。
    fn log_horizontal_scrollbar_metrics(
        scroll_handle: &UniformListScrollHandle,
        line_number_width: f32,
    ) -> Option<LogScrollbarMetrics> {
        let state = scroll_handle.0.borrow();
        let size = state.last_item_size?;
        let viewport_width = size.item.width;
        let measured_content_width = size.contents.width;
        let measured_max_scroll = (measured_content_width - viewport_width).max(px(0.0));
        let handle_max_scroll = state.base_handle.max_offset().width;
        let max_scroll = handle_max_scroll.max(measured_max_scroll);
        if viewport_width <= px(0.0) || max_scroll <= px(0.0) {
            return None;
        }

        let content_width = viewport_width + max_scroll;
        let scroll_left = (-state.base_handle.offset().x).clamp(px(0.0), max_scroll);
        let track_start = px(line_number_width + LOG_VIEWER_SCROLLBAR_PADDING);
        let track_right_padding =
            px(LOG_VIEWER_SCROLLBAR_WIDTH + LOG_VIEWER_SCROLLBAR_PADDING * 2.0);
        let track_length = (viewport_width - track_start - track_right_padding).max(px(1.0));
        let min_thumb_length = px(LOG_VIEWER_SCROLLBAR_MIN_THUMB_HEIGHT).min(track_length);
        let thumb_length =
            (track_length * (viewport_width / content_width)).clamp(min_thumb_length, track_length);
        let movable_length = (track_length - thumb_length).max(px(0.0));
        let thumb_start = track_start + movable_length * (scroll_left / max_scroll);

        Some(LogScrollbarMetrics {
            thumb_start,
            thumb_length,
            track_start,
            track_length,
            max_scroll,
        })
    }

    /// 在列表首帧尚未写入布局测量时，给明显较长的日志提供一个临时纵向滚动条提示。
    ///
    /// 业务意图：
    /// - `UniformListScrollHandle` 的内容高度需要等布局完成后才有值，首帧如果完全不显示滚动条会让长日志看起来不可滚动。
    /// - 这里仅对超过常见单屏行数的日志显示顶部最小滑块，下一次滚动或重绘会被真实测量值替换。
    fn fallback_log_vertical_scrollbar_metrics(line_count: usize) -> Option<LogScrollbarMetrics> {
        if line_count <= 40 {
            return None;
        }

        Some(LogScrollbarMetrics {
            thumb_start: px(LOG_VIEWER_SCROLLBAR_PADDING),
            thumb_length: px(LOG_VIEWER_SCROLLBAR_MIN_THUMB_HEIGHT),
            track_start: px(LOG_VIEWER_SCROLLBAR_PADDING),
            track_length: px(LOG_VIEWER_SCROLLBAR_MIN_THUMB_HEIGHT),
            max_scroll: px(0.0),
        })
    }

    /// 开始拖动日志正文滚动条滑块。
    ///
    /// 业务意图：
    /// - 鼠标按下滑块时记录当前 tab、方向和按下点在滑块内的偏移，后续移动事件按这些信息换算滚动偏移。
    /// - 拖动滚动条属于正文操作，开始拖动时同步关闭 tab 右键菜单和编码下拉框，避免弹层遮挡拖动区域。
    ///
    /// 边界条件：
    /// - 如果当前 tab 已关闭、滚动条测量尚未完成或日志内容不足以滚动，则忽略本次按下。
    fn start_log_scrollbar_drag(
        &mut self,
        tab_id: usize,
        axis: LogScrollbarAxis,
        event: &MouseDownEvent,
    ) {
        let Some(tab) = self.open_tabs.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        let Some(metrics) = Self::log_scrollbar_metrics_for_tab(tab, axis) else {
            return;
        };
        if metrics.max_scroll <= px(0.0) {
            return;
        }
        let Some(viewport_origin) =
            Self::uniform_list_viewport_axis_origin(&tab.scroll_handle, axis)
        else {
            return;
        };

        let pointer_position = match axis {
            LogScrollbarAxis::Vertical => event.position.y,
            LogScrollbarAxis::Horizontal => event.position.x,
        };
        let absolute_thumb_start = viewport_origin + metrics.thumb_start;
        self.log_scrollbar_drag = Some(LogScrollbarDrag {
            tab_id,
            axis,
            cursor_offset: pointer_position - absolute_thumb_start,
        });
        self.tab_context_menu = None;
        self.encoding_dropdown_menu = None;
    }

    /// 根据鼠标移动更新日志正文滚动条拖动结果。
    ///
    /// 业务意图：
    /// - 自绘滚动条拖动必须反向写入 `uniform_list` 底层滚动偏移，才能让虚拟列表、滚轮滚动和滑块位置保持同源。
    /// - 只在鼠标左键仍处于按下状态时更新；如果系统报告左键已释放，则立即清理拖动状态。
    fn update_log_scrollbar_drag(&mut self, event: &MouseMoveEvent, context: &mut Context<Self>) {
        let Some(drag) = self.log_scrollbar_drag else {
            return;
        };
        if !event.dragging() {
            self.log_scrollbar_drag = None;
            context.notify();
            return;
        }

        let Some(tab) = self.open_tabs.iter().find(|tab| tab.id == drag.tab_id) else {
            self.log_scrollbar_drag = None;
            context.notify();
            return;
        };
        let Some(metrics) = Self::log_scrollbar_metrics_for_tab(tab, drag.axis) else {
            self.log_scrollbar_drag = None;
            context.notify();
            return;
        };
        let Some(viewport_origin) =
            Self::uniform_list_viewport_axis_origin(&tab.scroll_handle, drag.axis)
        else {
            self.log_scrollbar_drag = None;
            context.notify();
            return;
        };

        let pointer_position = match drag.axis {
            LogScrollbarAxis::Vertical => event.position.y,
            LogScrollbarAxis::Horizontal => event.position.x,
        };
        let movable_length = (metrics.track_length - metrics.thumb_length).max(px(0.0));
        if metrics.max_scroll <= px(0.0) || movable_length <= px(0.0) {
            return;
        }

        let requested_thumb_start = pointer_position - viewport_origin - drag.cursor_offset;
        let thumb_start =
            requested_thumb_start.clamp(metrics.track_start, metrics.track_start + movable_length);
        let scroll_offset =
            metrics.max_scroll * ((thumb_start - metrics.track_start) / movable_length);
        let base_scroll_handle = {
            // `UniformListScrollHandle` 包装了真正的 `ScrollHandle`；这里克隆句柄后释放借用，再写入偏移。
            // 这样可以避免在 RefCell 借用仍存活时触发内部可变借用，保持滚动同步逻辑清晰。
            tab.scroll_handle.0.borrow().base_handle.clone()
        };
        let current_offset = base_scroll_handle.offset();

        match drag.axis {
            LogScrollbarAxis::Vertical => {
                base_scroll_handle.set_offset(point(current_offset.x, -scroll_offset));
            }
            LogScrollbarAxis::Horizontal => {
                base_scroll_handle.set_offset(point(-scroll_offset, current_offset.y));
            }
        }
        context.notify();
    }

    /// 结束日志正文滚动条拖动。
    ///
    /// 业务意图：
    /// - 鼠标释放后清空拖动状态，避免下一次普通鼠标移动继续改变日志滚动位置。
    fn stop_log_scrollbar_drag(&mut self, context: &mut Context<Self>) {
        if self.log_scrollbar_drag.is_some() {
            self.log_scrollbar_drag = None;
            context.notify();
        }
    }

    /// 开始选择日志正文文本。
    ///
    /// 业务意图：
    /// - 当前日志查看器是虚拟列表自绘，鼠标按下时需要主动记录选择锚点，后续拖动才能跨行扩展选区。
    /// - 点击日志正文同时关闭浮层菜单，避免选区操作和 tab 菜单、编码菜单叠加造成误操作。
    ///
    /// 边界条件：
    /// - 只响应当前仍存在且已解码的 tab；加载中或失败状态没有可选择的正文。
    /// - 单击会形成空选择，视觉上不高亮，但会清理上一次选择，符合常见文本查看器行为。
    fn start_log_text_selection(
        &mut self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let Some(position) =
            self.log_text_position_from_pointer(tab_id, line_index, line, event.position.x)
        else {
            return;
        };
        let Some(tab) = self.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        if !matches!(tab.state, LogTabState::Ready { .. }) {
            return;
        }

        tab.text_selection = Some(LogTextSelection {
            anchor: position,
            focus: position,
        });
        tab.selection_drag_anchor = Some(position);
        self.tab_context_menu = None;
        self.encoding_dropdown_menu = None;
        context.notify();
    }

    /// 根据鼠标拖动更新日志正文选区。
    ///
    /// 业务意图：
    /// - 拖动过程中锚点保持不变，只更新焦点位置，从而支持任意方向选择。
    /// - 该函数只更新可见行上的拖动结果；虚拟列表外自动滚动选择后续需要单独定义交互规则。
    fn update_log_text_selection(
        &mut self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        if !event.dragging() {
            self.stop_log_text_selection(context);
            return;
        }

        let Some(anchor) = self
            .open_tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| tab.selection_drag_anchor)
        else {
            return;
        };
        let Some(position) =
            self.log_text_position_from_pointer(tab_id, line_index, line, event.position.x)
        else {
            return;
        };
        let Some(tab) = self.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };

        tab.text_selection = Some(LogTextSelection {
            anchor,
            focus: position,
        });
        context.notify();
    }

    /// 结束日志正文选区拖动。
    ///
    /// 业务意图：
    /// - 鼠标释放后保留最终选区用于复制和搜索预填，但清理拖动锚点，避免下一次鼠标移动继续扩展旧选区。
    fn stop_log_text_selection(&mut self, context: &mut Context<Self>) {
        let mut changed = false;
        for tab in &mut self.open_tabs {
            if tab.selection_drag_anchor.take().is_some() {
                changed = true;
            }
        }
        if changed {
            context.notify();
        }
    }

    /// 将鼠标横坐标换算为日志正文字符位置。
    ///
    /// 业务意图：
    /// - GPUI 鼠标事件提供窗口坐标，而日志正文在虚拟列表内会随横向滚动平移；命中测试必须把行号列、正文内边距和滚动偏移统一扣除。
    /// - 输出仍是字符列，复制和高亮时再按真实字符串转换为 UTF-8 字节范围。
    ///
    /// 边界条件：
    /// - 指针落在正文起点左侧时归到第 0 列，落在行尾右侧时归到最后一列，避免越界。
    /// - 制表符和全角字符的视觉宽度可能与单字符估算不同，这是当前自绘查看器的已知限制。
    fn log_text_position_from_pointer(
        &self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        pointer_x: Pixels,
    ) -> Option<LogTextPosition> {
        let tab = self.open_tabs.iter().find(|tab| tab.id == tab_id)?;
        let LogTabState::Ready { document } = &tab.state else {
            return None;
        };

        let line_number_width = Self::log_viewer_line_number_width(document.line_count());
        let scroll_state = tab.scroll_handle.0.borrow();
        let bounds = scroll_state.base_handle.bounds();
        if bounds.size.width <= px(0.0) {
            return Some(LogTextPosition {
                line_index,
                column: 0,
            });
        }

        let text_origin_x = bounds.left()
            + scroll_state.base_handle.offset().x
            + px(line_number_width + LOG_VIEWER_TEXT_LEFT_PADDING);
        let raw_column = f32::from(pointer_x - text_origin_x) / LOG_VIEWER_TEXT_CHAR_WIDTH;
        let column = raw_column.round().max(0.0) as usize;
        let column = column.min(line.chars().count());

        Some(LogTextPosition { line_index, column })
    }

    /// 构造日志正文选区高亮样式。
    ///
    /// 业务意图：
    /// - 语法高亮按文字颜色表达，选区属于交互反馈，需要使用浅色背景以接近系统文本选择体验。
    /// - 字体颜色保持默认，避免复制选区时影响日志级别、时间戳等已有高亮的可读性。
    fn log_text_selection_highlight_style() -> gpui::HighlightStyle {
        gpui::HighlightStyle {
            background_color: Some(rgb(0xcfe8ff).into()),
            ..Default::default()
        }
    }

    /// 按 tab 和方向取得当前滚动条的真实测量数据。
    ///
    /// 业务意图：
    /// - 渲染、按下和拖动都复用同一套测量函数；横向滚动条需要根据当前日志行数计算行号列宽。
    /// - 只有处于已解码状态的 tab 才可能产生横向滚动条，因为加载和失败状态没有正文列表。
    fn log_scrollbar_metrics_for_tab(
        tab: &OpenLogTab,
        axis: LogScrollbarAxis,
    ) -> Option<LogScrollbarMetrics> {
        match axis {
            LogScrollbarAxis::Vertical => Self::log_vertical_scrollbar_metrics(&tab.scroll_handle),
            LogScrollbarAxis::Horizontal => match &tab.state {
                LogTabState::Ready { document } => Self::log_horizontal_scrollbar_metrics(
                    &tab.scroll_handle,
                    Self::log_viewer_line_number_width(document.line_count()),
                ),
                LogTabState::Loading { .. } | LogTabState::Failed { .. } => None,
            },
        }
    }

    /// 取得虚拟列表视口在当前轴向上的窗口坐标起点。
    ///
    /// 业务意图：
    /// - 鼠标事件给出的是窗口坐标，而滚动条滑块位置是列表内部局部坐标，拖动换算前必须统一坐标系。
    /// - 左侧目录树和右侧日志正文都使用 `UniformListScrollHandle`，统一函数可以避免两个滚动条坐标换算出现偏差。
    fn uniform_list_viewport_axis_origin(
        scroll_handle: &UniformListScrollHandle,
        axis: LogScrollbarAxis,
    ) -> Option<Pixels> {
        let state = scroll_handle.0.borrow();
        let bounds = state.base_handle.bounds();
        match axis {
            LogScrollbarAxis::Vertical if bounds.size.height > px(0.0) => Some(bounds.top()),
            LogScrollbarAxis::Horizontal if bounds.size.width > px(0.0) => Some(bounds.left()),
            LogScrollbarAxis::Vertical | LogScrollbarAxis::Horizontal => None,
        }
    }

    /// 渲染日志正文中的单行。
    ///
    /// 业务意图：
    /// - 行号固定宽度，正文使用等宽字体并保持不换行，符合日志查看器常见阅读习惯。
    /// - 横向滚动时整行会被 GPUI 列表整体平移，因此行号单元格需要用反向偏移补偿，确保行号视觉固定。
    /// - 日志级别高亮只作用于等级关键字，不改变整行背景，避免大面积颜色干扰扫描。
    /// - 搜索结果跳转的目标行允许使用整行背景提示，这是定位反馈，不属于语法高亮规则。
    fn render_log_line(
        input: LogLineRenderData,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let LogLineRenderData {
            tab_id,
            line_index,
            line,
            highlights,
            line_number_width,
            horizontal_line_number_offset,
            search_highlighted,
        } = input;
        let line_for_mouse_down = line.clone();
        let line_for_mouse_move = line.clone();

        div()
            .id(SharedString::from(format!(
                "log-line-{}-{}",
                tab_id, line_index
            )))
            .relative()
            .h(px(LOG_VIEWER_ROW_HEIGHT))
            .text_size(px(LOG_VIEWER_FONT_SIZE))
            .line_height(px(LOG_VIEWER_ROW_HEIGHT))
            .font_family(LOG_VIEWER_FONT_FAMILY)
            .when(search_highlighted, |row| row.bg(rgb(0xfff8c5)))
            .when(!search_highlighted, |row| {
                row.hover(|row| row.bg(rgb(0xf6f8fa)))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(LOG_VIEWER_ROW_HEIGHT))
                    .pl(px(line_number_width + LOG_VIEWER_TEXT_LEFT_PADDING))
                    .pr_2()
                    .whitespace_nowrap()
                    .text_color(rgb(0x24292f))
                    .child(StyledText::new(line).with_highlights(highlights)),
            )
            .child(
                div()
                    .absolute()
                    .left(horizontal_line_number_offset)
                    .top(px(0.0))
                    .flex()
                    .items_center()
                    .justify_end()
                    .h(px(LOG_VIEWER_ROW_HEIGHT))
                    .w(px(line_number_width))
                    .pr_2()
                    .text_right()
                    .text_color(rgb(0x8c959f))
                    .bg(rgb(if search_highlighted {
                        0xfff8c5
                    } else {
                        0xf6f8fa
                    }))
                    .border_r_1()
                    .border_color(rgb(0xe5e7eb))
                    .child((line_index + 1).to_string()),
            )
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.start_log_text_selection(
                        tab_id,
                        line_index,
                        &line_for_mouse_down,
                        event,
                        context,
                    );
                }),
            )
            .on_mouse_move(context.listener(
                move |view, event: &MouseMoveEvent, _window, context| {
                    view.update_log_text_selection(
                        tab_id,
                        line_index,
                        &line_for_mouse_move,
                        event,
                        context,
                    );
                },
            ))
    }

    /// 打开 tab 右键菜单。
    ///
    /// 业务意图：
    /// - 菜单位置使用鼠标释放位置，贴近用户右键的 tab。
    fn open_tab_context_menu(
        &mut self,
        tab_id: usize,
        window_x: f32,
        window_y: f32,
        context: &mut Context<Self>,
    ) {
        // GPUI 鼠标事件给出的是窗口内容坐标，而右键菜单作为右侧面板内部的绝对定位元素渲染。
        // 这里把坐标转换到右侧面板局部坐标，避免菜单因为左侧目录树和顶部工具栏的偏移而显示到错误位置。
        let panel_x = (window_x - self.left_panel_width - SPLITTER_VISIBLE_WIDTH).max(0.0);
        let panel_y = (window_y - TOOLBAR_HEIGHT).max(0.0);
        self.activate_tab(tab_id);
        self.tab_context_menu = Some(TabContextMenu {
            tab_id,
            x: panel_x,
            y: panel_y,
        });
        context.notify();
    }

    /// 渲染 tab 右键菜单。
    ///
    /// 业务意图：
    /// - 自绘菜单保证 macOS 和 Windows 的 tab 关闭命令行为一致，不依赖平台窗口系统菜单。
    fn render_tab_context_menu(&self, context: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = &self.tab_context_menu else {
            return div().id("tab-context-menu-empty").hidden();
        };
        let tab_id = menu.tab_id;

        div()
            .id("tab-context-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(TAB_CONTEXT_MENU_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(0xd0d7de))
            .bg(rgb(0xffffff))
            .shadow_lg()
            .child(self.render_tab_context_menu_item(
                tab_id,
                TabContextMenuAction::Current,
                "关闭当前",
                context,
            ))
            .child(self.render_tab_context_menu_item(
                tab_id,
                TabContextMenuAction::OtherTabs,
                "关闭其他",
                context,
            ))
            .child(self.render_tab_context_menu_item(
                tab_id,
                TabContextMenuAction::AllTabs,
                "关闭所有",
                context,
            ))
    }

    /// 渲染 tab 右键菜单单项。
    ///
    /// 业务意图：
    /// - 菜单项点击后立即执行关闭命令并收起菜单。
    fn render_tab_context_menu_item(
        &self,
        tab_id: usize,
        action: TabContextMenuAction,
        label: &'static str,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("tab-menu-{}-{}", tab_id, label)))
            .flex()
            .items_center()
            .h(px(TAB_CONTEXT_MENU_ITEM_HEIGHT))
            .px_3()
            .text_sm()
            .text_color(rgb(0x24292f))
            .cursor_pointer()
            .hover(|item| item.bg(rgb(0xf6f8fa)))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.handle_tab_context_menu_action(tab_id, action, context);
                }),
            )
    }

    /// 执行 tab 右键菜单命令。
    ///
    /// 业务意图：
    /// - 三个关闭命令集中处理，保证关闭后 active tab 和菜单状态一致。
    fn handle_tab_context_menu_action(
        &mut self,
        tab_id: usize,
        action: TabContextMenuAction,
        context: &mut Context<Self>,
    ) {
        match action {
            TabContextMenuAction::Current => self.close_tab(tab_id),
            TabContextMenuAction::OtherTabs => self.close_other_tabs(tab_id),
            TabContextMenuAction::AllTabs => self.close_all_tabs(),
        }
        self.tab_context_menu = None;
        self.encoding_dropdown_menu = None;
        context.notify();
    }

    /// 关闭指定 tab。
    ///
    /// 边界条件：
    /// - 如果关闭的是当前激活 tab，则优先激活当前位置后面的 tab，否则激活前一个 tab。
    fn close_tab(&mut self, tab_id: usize) {
        let Some(index) = self.open_tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        self.open_tabs.remove(index);

        if self.active_tab_id == Some(tab_id) {
            self.active_tab_id = self
                .open_tabs
                .get(index)
                .or_else(|| {
                    index
                        .checked_sub(1)
                        .and_then(|previous| self.open_tabs.get(previous))
                })
                .map(|tab| tab.id);
        }
        if let Some(active_tab_id) = self.active_tab_id {
            self.scroll_tab_bar_to_tab(active_tab_id);
        }
        if self
            .encoding_dropdown_menu
            .as_ref()
            .is_some_and(|menu| menu.tab_id == tab_id)
        {
            self.encoding_dropdown_menu = None;
        }
        if self
            .log_scrollbar_drag
            .is_some_and(|drag| drag.tab_id == tab_id)
        {
            self.log_scrollbar_drag = None;
        }
    }

    /// 关闭指定 tab 之外的所有 tab。
    ///
    /// 业务意图：
    /// - 保留右键点击的 tab，并把它设为当前激活 tab。
    fn close_other_tabs(&mut self, tab_id: usize) {
        self.open_tabs.retain(|tab| tab.id == tab_id);
        self.active_tab_id = self.open_tabs.first().map(|tab| tab.id);
        self.tab_bar_scroll_handle
            .set_offset(point(px(0.0), px(0.0)));
        if self
            .encoding_dropdown_menu
            .as_ref()
            .is_some_and(|menu| menu.tab_id != tab_id)
        {
            self.encoding_dropdown_menu = None;
        }
        if self
            .log_scrollbar_drag
            .is_some_and(|drag| drag.tab_id != tab_id)
        {
            self.log_scrollbar_drag = None;
        }
    }

    /// 关闭所有日志 tab。
    ///
    /// 业务意图：
    /// - 清空右侧工作区后回到“点击左侧日志文件查看内容”的友好提示。
    fn close_all_tabs(&mut self) {
        self.open_tabs.clear();
        self.active_tab_id = None;
        self.encoding_dropdown_menu = None;
        self.log_scrollbar_drag = None;
        self.tab_bar_scroll_handle = ScrollHandle::new();
    }

    /// 渲染左右分栏内容区域。
    ///
    /// 业务意图：
    /// - 未加载日志时主内容区占满剩余空间并显示友好提示，不出现空的左侧目录区域。
    /// - 加载完成后切换为左右两栏，左侧展示目录树，右侧展示日志 tab 工作区或点击文件提示。
    ///
    /// 边界条件：
    /// - 当前只有加载完成状态显示可拖动分割线；未加载、加载中或失败状态不显示分割线。
    /// - 没有打开任何日志 tab 时，右侧只显示下一步提示，不创建空白占位 tab。
    /// - 当前分割宽度仅在内存中生效，不跨启动保存。
    fn render_content(&self, context: &mut Context<Self>) -> impl IntoElement {
        if !matches!(self.load_state, LogTreeLoadState::Loaded(_)) {
            return div()
                .id("log-content-empty-state")
                .flex()
                .flex_1()
                .size_full()
                .bg(rgb(0xffffff))
                .child(self.render_primary_content_message());
        }

        div()
            .id("log-content-split-view")
            .flex()
            .flex_1()
            .size_full()
            .bg(rgb(0xffffff))
            .on_mouse_move(context.listener(Self::handle_content_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(Self::handle_content_mouse_up),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(Self::handle_content_mouse_up),
            )
            .child(
                div()
                    .id("left-log-panel")
                    .h_full()
                    .w(px(self.left_panel_width))
                    .flex_none()
                    .overflow_hidden()
                    .child(self.render_log_tree_panel(context)),
            )
            .child(self.render_splitter(context))
            .child(
                div()
                    .id("right-log-panel")
                    .relative()
                    .h_full()
                    .flex_1()
                    .bg(rgb(0xffffff))
                    .overflow_hidden()
                    .child(self.render_right_log_panel(context))
                    .child(self.render_splitter_hit_overlay(context)),
            )
    }

    /// 渲染左右两栏之间的可拖动分割线。
    ///
    /// 业务意图：
    /// - 分割线提供明确的拖动命中区域，让用户可以动态调整左右区域宽度。
    /// - 可见部分保持 1px 细线，透明命中区由右侧内容覆盖层提供，避免右侧出现白色拖动间隔。
    ///
    /// 边界条件：
    /// - 当前只支持水平拖动，不能双击重置，也不能通过键盘调整。
    /// - 该元素只承担视觉线条和左侧边界拖动；右侧扩展命中区由 `render_splitter_hit_overlay` 提供。
    fn render_splitter(&self, context: &mut Context<Self>) -> impl IntoElement {
        // 拖动中使用略深的线条颜色，提供清晰反馈；非拖动状态保持轻量，避免抢占内容注意力。
        let line_color = if self.is_resizing_splitter {
            0x94a3b8
        } else {
            0xd0d7de
        };

        div()
            .id("content-splitter")
            .flex()
            .items_center()
            .justify_center()
            .h_full()
            .w(px(SPLITTER_VISIBLE_WIDTH))
            .flex_none()
            .cursor_col_resize()
            .bg(rgb(line_color))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(Self::start_resizing_splitter),
            )
    }
}

impl EntityInputHandler for MainView {
    /// 返回指定 UTF-16 范围内的搜索框文本。
    ///
    /// 业务意图：
    /// - 平台输入法需要查询当前文本片段以管理候选词、组合文本和替换范围。
    /// - 搜索框内部保存 UTF-8 字符串，因此这里必须进行 UTF-16 到 UTF-8 的安全转换。
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<String> {
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search_dialog.as_ref()?;
        let (text, _, _) = Self::search_text_state(dialog, input_kind);
        let range = Self::search_input_range_from_utf16(text, range_utf16);
        adjusted_range.replace(Self::search_input_range_to_utf16(text, range.clone()));
        Some(text[range].to_string())
    }

    /// 返回搜索框当前选择范围。
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search_dialog.as_ref()?;
        let (text, selection_range, _) = Self::search_text_state(dialog, input_kind);
        Some(UTF16Selection {
            range: Self::search_input_range_to_utf16(text, selection_range),
            reversed: false,
        })
    }

    /// 返回输入法当前组合文本范围。
    fn marked_text_range(
        &self,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search_dialog.as_ref()?;
        let (text, _, marked_range) = Self::search_text_state(dialog, input_kind);
        marked_range.map(|range| Self::search_input_range_to_utf16(text, range))
    }

    /// 清除输入法组合文本状态。
    fn unmark_text(&mut self, window: &mut Window, context: &mut Context<Self>) {
        let input_kind = self.active_search_text_input_kind(window);
        if let Some(dialog) = self.search_dialog.as_mut() {
            let (_, _, marked_range) = Self::search_text_state_mut(dialog, input_kind);
            *marked_range = None;
        }
        context.notify();
    }

    /// 用平台提交文本替换搜索框中的指定范围。
    ///
    /// 业务意图：
    /// - 中文 IME 候选词确认后会通过该入口提交最终文本，不能再依赖按键字符。
    /// - 替换范围优先采用平台指定范围，其次采用组合文本范围，最后使用普通选择范围。
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let input_kind = self.active_search_text_input_kind(window);
        let Some(dialog) = self.search_dialog.as_mut() else {
            return;
        };
        let replacement = Self::sanitize_search_input_text(text);
        let (target_text, selection_range, marked_range) =
            Self::search_text_state_mut(dialog, input_kind);
        let range = range_utf16
            .map(|range| Self::search_input_range_from_utf16(target_text, range))
            .or_else(|| marked_range.clone())
            .unwrap_or_else(|| selection_range.clone());
        let range = range.start.min(target_text.len())..range.end.min(target_text.len());
        target_text.replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        *selection_range = cursor..cursor;
        *marked_range = None;
        context.notify();
    }

    /// 用平台组合文本替换搜索框中的指定范围，并保留组合状态。
    ///
    /// 边界条件：
    /// - 输入法可能多次更新同一段组合文本，必须优先替换旧 `marked_range`，避免拼音或候选词重复追加。
    /// - 搜索框只支持单行文本，因此组合文本中的换行会被移除。
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let input_kind = self.active_search_text_input_kind(window);
        let Some(dialog) = self.search_dialog.as_mut() else {
            return;
        };
        let replacement = Self::sanitize_search_input_text(new_text);
        let (target_text, selection_range, marked_range) =
            Self::search_text_state_mut(dialog, input_kind);
        let range = range_utf16
            .map(|range| Self::search_input_range_from_utf16(target_text, range))
            .or_else(|| marked_range.clone())
            .unwrap_or_else(|| selection_range.clone());
        let range = range.start.min(target_text.len())..range.end.min(target_text.len());
        target_text.replace_range(range.clone(), &replacement);

        if replacement.is_empty() {
            *marked_range = None;
        } else {
            *marked_range = Some(range.start..range.start + replacement.len());
        }

        let selected_range = new_selected_range_utf16
            .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
            .map(|relative_range| {
                range.start + relative_range.start..range.start + relative_range.end
            })
            .unwrap_or_else(|| {
                let cursor = range.start + replacement.len();
                cursor..cursor
            });
        *selection_range = selected_range;
        context.notify();
    }

    /// 返回指定文本范围在屏幕上的近似边界，用于放置 IME 候选窗口。
    ///
    /// 实现原因：
    /// - 当前搜索框使用普通 `div` 绘制文本，没有保存精确字形布局。
    /// - 返回输入框整体边界可保证候选窗口贴近搜索框，优先满足跨平台可用性。
    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(element_bounds)
    }

    /// 根据鼠标位置返回文本插入点。
    ///
    /// 边界条件：
    /// - 第一版搜索框不实现鼠标选择和精确点击定位，因此始终返回文本末尾。
    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<usize> {
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search_dialog.as_ref()?;
        let (text, _, _) = Self::search_text_state(dialog, input_kind);
        Some(Self::search_input_utf16_offset_from_byte(text, text.len()))
    }
}

impl Render for MainView {
    /// 渲染主窗口内容。
    ///
    /// 实现原因：
    /// - 顶部工具栏提供全局入口，内容区提供左右分栏和左侧日志目录树。
    /// - 右侧主内容区仍不放占位文案，避免用户误以为日志正文、诊断或设置功能已经完成。
    fn render(&mut self, window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0xffffff))
            .track_focus(&self.root_focus_handle)
            .on_action(
                context.listener(|view, _: &OpenSearchDialog, window, context| {
                    view.open_search_dialog(window, context);
                }),
            )
            .on_mouse_move(context.listener(Self::handle_root_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(Self::handle_root_mouse_up),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(Self::handle_root_mouse_up),
            )
            .child(self.render_toolbar(context))
            .child(self.render_content(context))
            .child(self.render_search_dialog(window, context))
    }
}

/// 程序入口。
///
/// 业务意图：
/// - 初始化 GPUI 应用并创建唯一主窗口。
/// - 当前阶段挂载窗口、工具栏、目录树、日志 tab 工作区和编码切换入口，暂不挂载菜单栏或配置系统。
///
/// 错误处理：
/// - 主窗口创建失败意味着桌面应用无法进入可交互状态，属于启动期不可恢复错误。
/// - 这里使用 `expect` 并配套中文错误说明，是为了让开发期和测试期能直接暴露
///   窗口系统、图形环境或 GPUI 初始化问题；普通业务错误后续不得采用这种处理方式。
fn main() {
    Application::new().run(|app| {
        app.bind_keys([
            KeyBinding::new("ctrl-f", OpenSearchDialog, None),
            KeyBinding::new("cmd-f", OpenSearchDialog, None),
        ]);
        // 注册 Lucide 图标字体和内置 JetBrains Mono 正文字体，确保 macOS 和 Windows 上的图标与日志等宽字体
        // 不依赖运行环境预装字体；如果注册失败，核心界面视觉无法可靠渲染，启动期应直接暴露错误。
        app.text_system()
            .add_fonts(vec![
                Cow::Borrowed(LUCIDE_FONT_BYTES),
                Cow::Borrowed(JETBRAINS_MONO_REGULAR_FONT_BYTES),
            ])
            .expect("注册内置字体失败，工具栏图标或日志正文等宽字体无法可靠渲染");

        let window_options = WindowOptions {
            // 显式设置系统标题栏标题，保证 macOS 和 Windows 的原生窗口标题都使用产品名。
            // 后续如果标题需要包含文件名或状态，应在业务规则明确后统一修改这里的标题策略。
            titlebar: Some(TitlebarOptions {
                title: Some(MAIN_WINDOW_TITLE.into()),
                ..Default::default()
            }),
            // 使用 GPUI 提供的居中窗口边界 API，由框架根据主显示器计算平台窗口坐标。
            // 当前不持久化位置，避免在配置目录和权限规则未定义前写入用户环境。
            window_bounds: Some(WindowBounds::centered(
                size(px(MAIN_WINDOW_WIDTH), px(MAIN_WINDOW_HEIGHT)),
                app,
            )),
            ..Default::default()
        };

        let main_view = app
            .open_window(window_options, |window, app| {
                let view = app.new(MainView::new);
                window.focus(&view.read(app).root_focus_handle);
                view
            })
            .expect("创建 LogClinic 主窗口失败，应用无法继续启动");
        let main_view_for_keys = main_view;
        let subscription = app.observe_keystrokes(move |event, window, app| {
            main_view_for_keys
                .update(app, |view, _window, context| {
                    view.handle_global_keystroke(event.keystroke.clone(), window, context);
                })
                .ok();
        });
        main_view
            .update(app, |view, _window, _context| {
                view.global_keystroke_subscription = Some(subscription);
            })
            .ok();
    });
}

#[cfg(test)]
mod tests {
    //! 主界面纯状态逻辑测试。
    //!
    //! 业务意图：
    //! - GPUI 渲染交互主要依赖手动验收，但目录树状态这类纯数据规则可以通过单元测试锁定。
    //! - 本模块只验证不需要窗口系统的行为，避免测试环境依赖 macOS 或 Windows 图形能力。

    use super::*;

    /// 构造测试用目录树行。
    ///
    /// 业务意图：
    /// - 测试只关心节点层级、类型和来源，统一构造函数可以避免每个用例重复填充无关展示字段。
    fn test_tree_row(
        id: usize,
        depth: usize,
        kind: LogTreeEntryKind,
        has_children: bool,
        source: Option<LogFileSource>,
    ) -> LoadedLogTreeRow {
        LoadedLogTreeRow {
            id,
            depth,
            label: format!("node-{}", id),
            kind,
            has_children,
            meta: None,
            error_message: None,
            source,
        }
    }

    /// 构造测试用压缩包成员来源。
    ///
    /// 边界条件：
    /// - 路径只用于来源相等性判断，不会在测试中访问真实文件系统。
    fn test_archive_member(member_path: &str) -> LogFileSource {
        LogFileSource::ArchiveMember {
            archive_path: PathBuf::from("logs.zip"),
            archive_format: log_loader::ArchiveFormat::Zip,
            member_path: member_path.to_string(),
        }
    }

    /// 验证日志选区的字节范围不会截断中文字符。
    ///
    /// 业务意图：
    /// - 只读复制和选区高亮都依赖 `selected_byte_range_for_line`，如果该函数返回非 UTF-8 边界会在渲染或复制时崩溃。
    #[test]
    fn 日志选区范围保持_utf8_边界() {
        let line = "abc中文def";
        let selection = LogTextSelection {
            anchor: LogTextPosition {
                line_index: 0,
                column: 3,
            },
            focus: LogTextPosition {
                line_index: 0,
                column: 5,
            },
        };

        let range = MainView::selected_byte_range_for_line(&selection, 0, line)
            .expect("中文选区应返回有效范围");
        assert_eq!(&line[range], "中文");
    }

    /// 验证反向拖选时仍能按文档顺序生成选区范围。
    ///
    /// 业务意图：
    /// - 用户可以从右往左拖选，复制和高亮必须使用规范化后的范围，而不是假设鼠标按下位置永远在前。
    #[test]
    fn 日志反向选区会规范化() {
        let line = "abcdef";
        let selection = LogTextSelection {
            anchor: LogTextPosition {
                line_index: 0,
                column: 5,
            },
            focus: LogTextPosition {
                line_index: 0,
                column: 2,
            },
        };

        let range = MainView::selected_byte_range_for_line(&selection, 0, line)
            .expect("反向选区应返回有效范围");
        assert_eq!(&line[range], "cde");
    }

    /// 验证单文件压缩包会返回内部成员来源，供 UI 点击压缩包根节点时直接打开。
    #[test]
    fn 单文件压缩包返回唯一成员来源() {
        let source = test_archive_member("access.log");
        let tree = LoadedLogTree {
            summary: "2 个节点".to_string(),
            rows: vec![
                test_tree_row(0, 0, LogTreeEntryKind::Archive, true, None),
                test_tree_row(1, 1, LogTreeEntryKind::File, false, Some(source.clone())),
            ],
            error_count: 0,
        };
        let state = LoadedLogTreeState::new(tree);

        assert_eq!(state.single_file_source_for_archive(0), Some(source));
    }

    /// 验证多文件压缩包不会被绑定到单个成员，仍应保留展开/收起目录树的行为。
    #[test]
    fn 多文件压缩包不返回直接打开来源() {
        let tree = LoadedLogTree {
            summary: "3 个节点".to_string(),
            rows: vec![
                test_tree_row(0, 0, LogTreeEntryKind::Archive, true, None),
                test_tree_row(
                    1,
                    1,
                    LogTreeEntryKind::File,
                    false,
                    Some(test_archive_member("access.log")),
                ),
                test_tree_row(
                    2,
                    1,
                    LogTreeEntryKind::File,
                    false,
                    Some(test_archive_member("error.log")),
                ),
            ],
            error_count: 0,
        };
        let state = LoadedLogTreeState::new(tree);

        assert_eq!(state.single_file_source_for_archive(0), None);
    }
}
