// 主窗口 UI 常量。
//
// 业务意图：
// - 集中维护 GPUI 壳层的尺寸、间距、菜单宽度和快捷键控制字符，避免根模块继续承载大量静态配置。
// - 常量数值保持迁移前完全一致，确保窗口布局、菜单定位、滚动条和输入行为零变化。

/// 应用主窗口的标题。
///
/// 业务意图：
/// - 第一阶段只验证窗口创建，因此标题使用稳定的产品名 `LogClinic`。
/// - 后续如果需要显示当前打开文件名或工作区状态，应在明确标题格式规则后再扩展。
pub(in crate::app) const MAIN_WINDOW_TITLE: &str = "LogClinic";

/// 主窗口的默认宽度。
///
/// 业务约束：
/// - 用户明确要求大屏默认窗口宽度固定为 1600px，以便日志 tab 和正文区域在首次打开时有更多横向空间。
/// - 这里使用浮点字面量是为了匹配 GPUI `px` 的尺寸 API，避免在调用处反复转换。
pub(in crate::app) const MAIN_WINDOW_WIDTH: f32 = 1600.0;

/// 主窗口的默认高度。
///
/// 边界说明：
/// - 用户明确要求大屏默认窗口高度固定为 900px，保证日志正文首屏拥有稳定的可视行数。
/// - 当前不设置最小尺寸和最大尺寸，因为本阶段只要求默认大小和用户手动调整后的尺寸记忆。
pub(in crate::app) const MAIN_WINDOW_HEIGHT: f32 = 900.0;

/// 小屏电脑的主显示器宽度阈值。
///
/// 业务意图：
/// - 用户明确要求按逻辑像素宽度 `< 1440px` 判定小屏电脑。
/// - 这里使用 GPUI 暴露的显示器逻辑像素，由框架负责处理 macOS Retina 和 Windows 缩放比例差异。
pub(in crate::app) const SMALL_SCREEN_MAXIMIZED_WIDTH_THRESHOLD: f32 = 1440.0;

/// 历史窗口宽度接近显示器宽度时判定为“最大化残留”的比例阈值。
///
/// 业务意图：
/// - 旧版本会在最大化关闭时保存接近屏幕宽度的窗口尺寸，导致大屏下次启动看起来仍是最大化。
/// - 只使用宽度判定是因为 macOS 标题栏、菜单栏和 Windows 任务栏会让高度存在平台差异；宽度是更稳定的最大化信号。
///
/// 边界条件：
/// - 阈值保留少量平台装饰误差，避免最大化窗口因为边框、缩放或系统保留区域略小于显示器宽度而漏判。
pub(in crate::app) const MAXIMIZED_RESTORED_WIDTH_RATIO: f32 = 0.96;

/// 左侧目录树右键菜单宽度。
///
/// 业务意图：
/// - 菜单承载“另存为”和“线程日志分析”两个文件操作，宽度需要足够展示中文命令且不挤压左侧树。
pub(in crate::app) const LOG_TREE_CONTEXT_MENU_WIDTH: f32 = 176.0;

/// 左侧目录树右键菜单单项高度。
///
/// 业务意图：
/// - 与 tab 右键菜单保持相同操作密度，保证 macOS 和 Windows 鼠标命中体验一致。
pub(in crate::app) const LOG_TREE_CONTEXT_MENU_ITEM_HEIGHT: f32 = 34.0;

/// 日志正文右键菜单宽度。
///
/// 业务意图：
/// - 菜单目前承载“复制”和“另存为”两个正文相关操作，宽度需要兼顾中文文案和鼠标命中面积。
pub(in crate::app) const LOG_VIEWER_CONTEXT_MENU_WIDTH: f32 = 152.0;

/// 日志正文右键菜单单项高度。
///
/// 业务意图：
/// - 与左侧树和 tab 右键菜单保持一致密度，避免同一应用内菜单命中体验不一致。
pub(in crate::app) const LOG_VIEWER_CONTEXT_MENU_ITEM_HEIGHT: f32 = 34.0;

/// 线程日志分析窗口默认宽度。
///
/// 业务意图：
/// - 时间线需要同时展示线程名和多个快照列，因此使用比设置窗口更宽的独立窗口。
pub(in crate::app) const THREAD_ANALYSIS_WINDOW_WIDTH: f32 = 1040.0;

/// 线程日志分析窗口默认高度。
///
/// 业务意图：
/// - Java thread dump 往往包含大量线程，较高窗口可以减少初次打开后的滚动成本。
pub(in crate::app) const THREAD_ANALYSIS_WINDOW_HEIGHT: f32 = 720.0;

/// 线程分析图中线程名列宽度。
///
/// 业务意图：
/// - Java 线程名经常包含业务前缀、线程池编号和连接信息，保留固定宽度便于和右侧时间线对齐。
pub(in crate::app) const THREAD_ANALYSIS_NAME_COLUMN_WIDTH: f32 = 260.0;

/// 线程分析图中单个时间快照列宽度。
///
/// 业务意图：
/// - 横轴不再展示时间文本后，列宽只需要容纳正方形状态色块和少量间距，避免大量快照时横向滚动过长。
pub(in crate::app) const THREAD_ANALYSIS_SNAPSHOT_COLUMN_WIDTH: f32 = 24.0;

/// 线程分析图中状态色块边长。
///
/// 业务意图：
/// - 用户要求色块高度保持不变且宽度与高度一致，因此使用固定 18px 正方形。
pub(in crate::app) const THREAD_ANALYSIS_STATE_BLOCK_SIZE: f32 = 18.0;

/// 线程分析图中最近一次点击跳转色块的强调色。
///
/// 业务意图：
/// - 点击跳转后的色块需要和 Java 线程状态色区分开，帮助用户回到分析窗口时快速确认刚才定位过哪一段日志。
/// - 这里使用玫红色，避开当前状态色中的绿色、红色、橙色、青色、紫色和灰色；明暗主题下都保持可辨识。
pub(in crate::app) const THREAD_ANALYSIS_JUMPED_CELL_COLOR: u32 = 0xec4899;

/// 线程分析色块悬浮气泡宽度。
///
/// 业务意图：
/// - 气泡需要容纳完整线程名和最多 5 行日志预览；固定宽度便于根据窗口边界计算弹出方向。
pub(in crate::app) const THREAD_ANALYSIS_POPUP_WIDTH: f32 = 520.0;

/// 线程分析色块悬浮气泡预估高度。
///
/// 业务意图：
/// - GPUI 在悬浮事件阶段尚未布局气泡，不能读取真实高度；这里按三行信息和五行预览估算，
///   用于选择向上或向下弹出，避免靠近窗口底部时被遮挡。
pub(in crate::app) const THREAD_ANALYSIS_POPUP_ESTIMATED_HEIGHT: f32 = 190.0;

/// 线程分析色块悬浮气泡与鼠标悬浮点的间距。
///
/// 业务意图：
/// - 保留少量间距，避免气泡刚出现就盖住当前悬浮的状态色块。
pub(in crate::app) const THREAD_ANALYSIS_POPUP_OFFSET: f32 = 12.0;

/// 线程分析色块悬浮气泡与窗口边缘的最小间距。
///
/// 业务意图：
/// - 气泡贴边会影响阴影和边框识别，保留边距也能减少被系统标题栏或窗口边框裁切的风险。
pub(in crate::app) const THREAD_ANALYSIS_POPUP_MARGIN: f32 = 8.0;

/// 日志功能页顶部操作栏的固定高度。
///
/// 业务意图：
/// - 根级文字工具栏已经迁移到左侧大导航；日志页仍需要加载日志和搜索入口，因此保留页内操作栏。
/// - 操作栏高度和 tab 页签高度一致，可以让日志页顶部控件和右侧 tab 栏的垂直节奏一致，减少首屏跳变感。
///
/// 边界条件：
/// - 当前不实现可换行操作栏；如果后续窗口宽度允许缩小，需要再定义窄宽度下的折叠策略。
pub(in crate::app) const TOOLBAR_HEIGHT: f32 = LOG_TAB_BAR_HEIGHT;

/// 主窗口左侧大导航竖条宽度。
///
/// 业务意图：
/// - 用户要求最左侧固定一个只显示图标的大导航竖条，用于在日志分析、HPROF 解析和 AI 对话之间切换。
/// - 固定 56px 可以在 macOS 和 Windows 上容纳 40px 命中按钮，同时不明显挤压日志内容区。
pub(in crate::app) const MAIN_NAV_WIDTH: f32 = 56.0;

/// 主导航按钮固定命中尺寸。
///
/// 业务意图：
/// - 导航按钮只显示图标，命中区域必须比图标更大，保证鼠标点击稳定且 hover 气泡容易触发。
pub(in crate::app) const MAIN_NAV_BUTTON_SIZE: f32 = 40.0;

/// 主导航图标字号。
///
/// 业务意图：
/// - 20px 图标在 56px 竖条内足够清晰，同时不会显得比日志页操作图标过重。
pub(in crate::app) const MAIN_NAV_ICON_SIZE: f32 = 20.0;

/// 主导航图标可视宽度。
///
/// 业务意图：
/// - Lucide 图标不同字形的宽度略有差异，固定可视宽度可以让所有入口在竖条中严格居中。
pub(in crate::app) const MAIN_NAV_ICON_WIDTH: f32 = 20.0;

/// 主导航按钮之间的垂直间距。
///
/// 业务意图：
/// - 顶部三个主功能和底部通用入口都使用同一间距，保持竖向节奏稳定。
pub(in crate::app) const MAIN_NAV_BUTTON_GAP: f32 = 8.0;

/// 主导航内边距。
///
/// 边界条件：
/// - 顶部和底部都保留同样内缩，避免导航按钮贴住系统标题栏或窗口底边。
pub(in crate::app) const MAIN_NAV_PADDING: f32 = 8.0;

/// 主导航悬浮气泡宽度。
///
/// 业务意图：
/// - 气泡只展示功能名称，固定宽度可以避免不同中文名称长度导致 hover 时布局抖动。
pub(in crate::app) const MAIN_NAV_TOOLTIP_WIDTH: f32 = 84.0;

/// 主导航悬浮气泡高度。
///
/// 业务意图：
/// - 气泡高度小于图标按钮命中区，既能和按钮垂直居中，也不会遮住相邻导航入口。
pub(in crate::app) const MAIN_NAV_TOOLTIP_HEIGHT: f32 = 30.0;

/// 主导航悬浮气泡相对竖条右侧的间距。
///
/// 边界条件：
/// - 气泡需要离开图标命中区一点距离，避免鼠标在图标和气泡之间移动时频繁闪烁。
pub(in crate::app) const MAIN_NAV_TOOLTIP_GAP: f32 = 8.0;

/// 主导航悬浮气泡相对按钮顶部或底部的垂直内缩。
///
/// 实现原因：
/// - 气泡由主窗口根节点覆盖绘制，不再是按钮子元素；独立常量能保证顶部入口和底部入口都仍与按钮视觉居中。
pub(in crate::app) const MAIN_NAV_TOOLTIP_BUTTON_INSET: f32 =
    (MAIN_NAV_BUTTON_SIZE - MAIN_NAV_TOOLTIP_HEIGHT) / 2.0;

/// 主界面工具栏按钮的水平内边距。
///
/// 实现原因：
/// - 当前按钮是文字按钮，不再使用边框和实体背景，因此水平内边距需要更紧凑。
/// - 统一内边距可以保证三个入口在 macOS 和 Windows 字体渲染差异下仍保持一致点击区域。
pub(in crate::app) const TOOLBAR_BUTTON_HORIZONTAL_PADDING: f32 = 8.0;

/// 主界面工具栏按钮的垂直内边距。
///
/// 边界说明：
/// - 工具栏高度和 tab 栏一致后，垂直内边距仍保持紧凑，避免文字和图标被裁切。
/// - 当前按钮不承载多行文本，也不展示快捷键提示，因此不需要动态高度。
pub(in crate::app) const TOOLBAR_BUTTON_VERTICAL_PADDING: f32 = 4.0;

/// Lucide 图标字体在 GPUI 中使用的字体族名称。
///
/// 业务意图：
/// - `lucide-icons` 通过字体字形暴露图标，必须在渲染图标字符时指定该字体族。
/// - 显式定义字体族名称可以避免图标字符被系统默认字体渲染成方块或错误符号。
///
/// 边界条件：
/// - 字体数据来自 `lucide-icons` 依赖内置的 `LUCIDE_FONT_BYTES`，启动时注册到 GPUI 文本系统。
/// - 如果未来改为 SVG 图标或其它图标库，应同步替换字体注册和图标渲染逻辑。
pub(in crate::app) const LUCIDE_FONT_FAMILY: &str = "lucide";

/// 工具栏按钮图标的固定宽度。
///
/// 业务意图：
/// - 三个 Lucide 图标字形宽度可能不同，固定图标区域可以让按钮文字起点更稳定。
/// - 该宽度只约束图标可视区域，不改变按钮整体点击区域。
///
/// 边界条件：
/// - 当前图标尺寸与工具栏高度配套，避免在 macOS 和 Windows 字体度量差异下发生裁切。
/// - 固定宽度只约束单个图标，不处理多个图标、徽标或加载状态。
pub(in crate::app) const TOOLBAR_BUTTON_ICON_WIDTH: f32 = 16.0;

/// 工具栏按钮图标字号。
///
/// 业务意图：
/// - Lucide 字体图标需要使用接近 16px 的字号才能保持清晰线条和按钮视觉平衡。
/// - 单独定义字号便于后续统一调整工具栏密度。
///
/// 边界条件：
/// - 字号不能超过工具栏高度，否则部分平台的字体上升/下降空间可能导致图标裁切。
pub(in crate::app) const TOOLBAR_BUTTON_ICON_SIZE: f32 = 16.0;

/// 左侧内容区域的默认宽度。
///
/// 业务意图：
/// - 用户明确要求左右两栏布局默认左侧区域宽度为 300px。
/// - 当前不持久化用户拖动后的宽度，避免在配置存储位置和权限未定义前写入用户环境。
pub(in crate::app) const LEFT_PANEL_DEFAULT_WIDTH: f32 = 300.0;

/// 左侧内容区域可拖动到的最小宽度。
///
/// 业务意图：
/// - 左侧区域后续预计承载日志来源、文件列表或过滤条件，过窄会导致基础交互不可用。
/// - 用户未定义最小宽度，因此采用保守的桌面客户端默认值，避免拖动后面板完全消失。
///
/// 边界条件：
/// - 该值只约束当前进程内拖动行为，不代表后续设置持久化或布局配置规则。
pub(in crate::app) const LEFT_PANEL_MIN_WIDTH: f32 = 180.0;

/// 右侧内容区域可保留的最小宽度。
///
/// 业务意图：
/// - 右侧区域后续预计承载日志内容主视图，需要保留足够宽度避免拖动左栏挤占主体区域。
/// - 用户未定义右侧最小宽度，因此当前用 320px 作为临时保护边界。
///
/// 边界条件：
/// - 如果后续右侧加入表格、详情面板或诊断视图，应重新定义最小宽度和窄屏折叠规则。
pub(in crate::app) const RIGHT_PANEL_MIN_WIDTH: f32 = 320.0;

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
pub(in crate::app) const SPLITTER_HIT_WIDTH: f32 = 6.0;

/// 左右内容区域之间分割线的可见宽度。
///
/// 业务意图：
/// - 用户反馈上一版分隔条太粗，因此可见线条收敛为 1px。
/// - 视觉宽度与拖拽命中宽度分离，保证界面轻量的同时不牺牲可用性。
///
/// 边界条件：
/// - 当前线条颜色固定；如果后续加入主题系统，应把颜色纳入主题令牌而不是继续硬编码。
pub(in crate::app) const SPLITTER_VISIBLE_WIDTH: f32 = 1.0;

/// 覆盖在右侧内容上的透明拖动命中宽度。
///
/// 业务意图：
/// - 可见分割线只占 1px，剩余命中区域放到右侧内容之上，让右侧内容视觉上紧贴分割线。
pub(in crate::app) const SPLITTER_OVERLAY_HIT_WIDTH: f32 =
    SPLITTER_HIT_WIDTH - SPLITTER_VISIBLE_WIDTH;

/// 左侧目录树标题区域高度。
///
/// 业务意图：
/// - 左侧区域用于展示真实加载后的日志目录树，标题区域帮助用户识别当前面板语义。
/// - 当前标题不展示真实绝对路径，避免路径脱敏和窄面板排版规则未定义前暴露过多信息。
pub(in crate::app) const LOG_TREE_HEADER_HEIGHT: f32 = 34.0;

/// 左侧目录树文本字号。
///
/// 业务意图：
/// - 目录树用于快速扫描文件层级、压缩包成员和错误节点，12px 可以在固定侧栏宽度内提高信息密度。
/// - 字号独立于右侧日志正文的用户设置，避免用户调大正文后影响目录树和 tab 页签的导航密度。
///
/// 边界条件：
/// - 行高仍由 `LOG_TREE_ROW_HEIGHT` 控制，字号变化不能破坏 `uniform_list` 的等高行假设。
/// - macOS 和 Windows 字体度量不同，因此使用显式像素字号，避免主题级 `text_sm` 映射差异。
pub(in crate::app) const LOG_TREE_FONT_SIZE: f32 = 12.0;

/// 左侧目录树每行固定高度。
///
/// 业务意图：
/// - 固定行高可以让树结构在 macOS 和 Windows 字体渲染差异下保持稳定密度。
/// - 后续接入虚拟列表或大目录时，也可以基于固定行高计算滚动区域。
///
/// 边界条件：
/// - 当前未实现超长目录名的多行展示；长文本统一截断，避免挤压布局。
pub(in crate::app) const LOG_TREE_ROW_HEIGHT: f32 = 28.0;

/// 左侧目录树首次加载后的默认展开深度。
///
/// 业务意图：
/// - 用户要求默认展开 2 级目录，因此深度 0 和深度 1 的可展开节点默认处于展开状态。
/// - 深度 2 的节点默认可见但不继续展开，避免大目录首次渲染时一次性暴露过多文件行。
///
/// 边界条件：
/// - 这里使用加载层提供的 `depth`，只影响初始 UI 展开状态，不裁剪真实扫描结果。
/// - 用户手动展开或收起后只在当前进程内生效，重新加载日志会重新套用默认展开规则。
pub(in crate::app) const LOG_TREE_DEFAULT_EXPANDED_DEPTH: usize = 2;

/// 左侧目录树每一级层级缩进。
///
/// 业务意图：
/// - 树形结构依赖稳定缩进表达父子关系，16px 可以容纳展开箭头和文件夹图标。
/// - 缩进值固定，避免不同节点因为文本长度不同导致层级难以扫描。
pub(in crate::app) const LOG_TREE_ROW_INDENT: f32 = 16.0;

/// 左侧目录树行内容的基础水平内边距。
///
/// 业务意图：
/// - 给树节点与面板边缘之间保留基础留白，避免图标贴边影响可读性。
/// - 缩进会叠加在该基础内边距之上，因此根节点和子节点都保持一致起点规则。
pub(in crate::app) const LOG_TREE_ROW_HORIZONTAL_PADDING: f32 = 10.0;

/// 左侧目录树展开箭头占位宽度。
///
/// 业务意图：
/// - 文件节点没有展开箭头，但仍需要占用同等宽度，保证文件夹和文件名称左边缘对齐。
/// - 该值只影响视觉布局，不代表当前节点支持真实展开或收起。
pub(in crate::app) const LOG_TREE_CHEVRON_WIDTH: f32 = 14.0;

/// 左侧目录树展开箭头字号。
///
/// 边界条件：
/// - 箭头图标必须小于行高，避免在低 DPI 或 Windows 字体度量差异下被裁切。
pub(in crate::app) const LOG_TREE_CHEVRON_SIZE: f32 = 13.0;

/// 左侧目录树文件夹和文件图标占位宽度。
///
/// 业务意图：
/// - 固定图标宽度可以让节点文字起点稳定，提升目录树快速扫描体验。
pub(in crate::app) const LOG_TREE_ITEM_ICON_WIDTH: f32 = 16.0;

/// 左侧目录树文件夹和文件图标字号。
///
/// 边界条件：
/// - 图标字号与行高配套，避免树节点在不同平台字体度量下出现垂直裁切。
pub(in crate::app) const LOG_TREE_ITEM_ICON_SIZE: f32 = 15.0;

/// 左侧目录树滚动条可见宽度。
///
/// 业务意图：
/// - 左侧目录树也使用虚拟列表，文件较多时需要稳定显示滚动位置，避免用户误以为列表只展示当前屏内容。
/// - 宽度和右侧日志正文滚动条保持一致，降低左右区域的视觉差异。
pub(in crate::app) const LOG_TREE_SCROLLBAR_WIDTH: f32 = 6.0;

/// 左侧目录树滚动条最小滑块高度。
///
/// 边界条件：
/// - 大目录中按比例计算出的滑块可能过小，最小高度保证鼠标仍能命中并拖动。
pub(in crate::app) const LOG_TREE_SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 36.0;

/// 左侧目录树滚动条距离面板边缘的内缩。
///
/// 业务意图：
/// - 滚动条贴近右侧但不压住分割线，避免目录树和右侧内容边界混在一起。
pub(in crate::app) const LOG_TREE_SCROLLBAR_PADDING: f32 = 3.0;

/// 右侧日志 tab 栏高度。
///
/// 业务意图：
/// - tab 栏需要容纳多个已打开日志文件的切换入口，同时不抢占日志正文可视高度。
/// - 固定高度便于右侧主体区域继续使用等高虚拟列表渲染日志行。
pub(in crate::app) const LOG_TAB_BAR_HEIGHT: f32 = 34.0;

/// 右侧日志 tab 页签文本字号。
///
/// 业务意图：
/// - tab 页签主要承担多文件导航，12px 可以容纳更多文件名，减少横向滚动频率。
/// - 该字号只作用于 tab 页签本身，不影响日志正文、编码工具条或右键菜单。
///
/// 边界条件：
/// - tab 高度继续由 `LOG_TAB_BAR_HEIGHT` 决定，避免字号调整引发顶部布局抖动。
/// - 显式像素字号可以让 macOS 和 Windows 上页签标题接近一致。
pub(in crate::app) const LOG_TAB_FONT_SIZE: f32 = 12.0;

/// tab 栏两侧滚动箭头按钮宽度。
///
/// 业务意图：
/// - 用户要求打开日志较多时 tab 可以横向滚动，并在两边显示滚动箭头。
/// - 按钮固定宽度可以让 tab 可视区域在滚动前后保持稳定。
pub(in crate::app) const LOG_TAB_SCROLL_BUTTON_WIDTH: f32 = 28.0;

/// tab 栏每次点击滚动箭头时移动的距离。
///
/// 业务意图：
/// - 该值约等于两个中等长度 tab 的宽度，点击次数和定位速度比较平衡。
pub(in crate::app) const LOG_TAB_SCROLL_STEP: f32 = 180.0;

/// tab 内关闭图标按钮宽度。
///
/// 业务意图：
/// - 关闭按钮需要稳定命中区，避免用户只能点到很小的 `X` 字形。
pub(in crate::app) const LOG_TAB_CLOSE_BUTTON_WIDTH: f32 = 18.0;

/// 右侧日志文档工具条高度。
///
/// 业务意图：
/// - 当前工具条只承载编码切换和当前文件状态，后续如果加入搜索或跳转行号也应从这里扩展。
pub(in crate::app) const LOG_DOCUMENT_TOOLBAR_HEIGHT: f32 = 36.0;

/// 右侧日志正文每行固定高度。
///
/// 业务意图：
/// - 日志正文使用 `uniform_list` 进行虚拟渲染，必须保持每行高度一致，才能在大文件滚动时稳定计算可见区间。
pub(in crate::app) const LOG_VIEWER_ROW_HEIGHT: f32 = 22.0;

/// 分页日志每帧额外渲染的缓冲行数。
///
/// 业务意图：
/// - 分页模式不再把完整行数交给 GPUI 做绝对像素滚动，而是只渲染视口附近行。
/// - 缓冲行用于覆盖小幅滚动和首帧视口尺寸尚未回填的情况，避免边缘出现空白。
pub(in crate::app) const PAGED_LOG_RENDER_BUFFER_ROWS: usize = 8;

/// 分页日志首帧视口高度未知时使用的保守行数。
///
/// 边界条件：
/// - `ScrollHandle::bounds` 需要经过一帧布局才会写入；首帧用固定行数可以先展示内容，下一帧再按真实视口收敛。
pub(in crate::app) const PAGED_LOG_FALLBACK_VISIBLE_ROWS: usize = 180;

/// 分页日志横向宽度估算的等宽字符比例。
///
/// 业务意图：
/// - 分页模式不能再依赖 `uniform_list` 测量完整最长行，否则会重新引入超大虚拟坐标。
/// - JetBrains Mono 的常见字符宽度约为字号的六成；这里只用于横向滚动范围估算，真实文本命中仍由 GPUI shaping 计算。
pub(in crate::app) const PAGED_LOG_MONOSPACE_WIDTH_RATIO: f32 = 0.62;

/// 日志查看器行号列固定宽度。
///
/// 业务意图：
/// - 行号和正文分开渲染，便于用户定位日志上下文，同时避免行号挤压正文起点。
pub(in crate::app) const LOG_VIEWER_LINE_NUMBER_MIN_WIDTH: f32 = 46.0;

/// 日志查看器行号列最大宽度。
///
/// 业务意图：
/// - 超大分页日志可能有数千万行，行号列必须允许 8 位以上数字完整显示，避免左侧高位被裁切。
/// - 仍保留上限，防止异常索引行数把正文区域完全挤出可视范围。
pub(in crate::app) const LOG_VIEWER_LINE_NUMBER_MAX_WIDTH: f32 = 128.0;

/// 日志查看器行号单个数字的估算宽度。
///
/// 业务意图：
/// - GPUI 当前没有在渲染前测量等宽行号文本的必要性，使用稳定估算即可让常见行数下列宽更紧凑。
pub(in crate::app) const LOG_VIEWER_LINE_NUMBER_DIGIT_WIDTH: f32 = 7.0;

/// 日志查看器行号列右侧留白和边框的估算宽度。
///
/// 业务意图：
/// - 行号需要和边框保持一点距离，避免数字贴线影响扫描。
pub(in crate::app) const LOG_VIEWER_LINE_NUMBER_PADDING_WIDTH: f32 = 18.0;

/// 日志正文相对行号列右侧的水平内边距。
///
/// 业务意图：
/// - 行号列固定在左侧，正文需要留出少量间距，避免文本贴着行号分割线显示。
/// - 文本选择命中测试也必须使用同一个偏移，保证鼠标选中的列和实际渲染起点一致。
pub(in crate::app) const LOG_VIEWER_TEXT_LEFT_PADDING: f32 = 8.0;

/// 日志正文使用的等宽字体族。
///
/// 业务意图：
/// - 日志通常包含时间戳、线程名和列对齐字段，等宽字体能提升扫描和对比效率。
/// - 用户指定使用 JetBrains Mono，并要求内置到程序中，因此启动时会注册随项目打包的字体字节。
pub(in crate::app) const LOG_VIEWER_FONT_FAMILY: &str = "JetBrains Mono";

/// 日志查看器中制表符按固定 4 列展开。
///
/// 业务意图：
/// - 一些日志用 `\t` 分隔字段，VS Code 等编辑器会按 tab stop 展开，所以列之间看起来有稳定间隔。
/// - GPUI 当前直接渲染 `\t` 时宽度不符合日志阅读预期，字段会贴在一起；这里在显示层展开为空格。
///
/// 边界条件：
/// - 这里只改变视觉显示，不改原始日志文本；复制、搜索、另存为仍保留文件里的真实 `\t`。
/// - tab stop 按用户要求固定为 4 列，暂不做设置项，避免影响现有信息密度。
pub(in crate::app) const LOG_VIEWER_TAB_WIDTH: usize = 4;

/// 内置 JetBrains Mono Regular 字体数据。
///
/// 业务意图：
/// - 日志正文必须使用 JetBrains Mono，不能依赖用户系统是否安装该字体。
/// - `include_bytes!` 会把字体文件编译进可执行产物，macOS 和 Windows 启动时通过同一套 GPUI 字体注册流程加载。
///
/// 边界条件：
/// - 当前只内置 Regular 字重；日志正文高亮仍通过字体系统模拟粗体，后续若要求更高字重质量可追加 Bold 字体文件。
pub(in crate::app) const JETBRAINS_MONO_REGULAR_FONT_BYTES: &[u8] =
    include_bytes!("../../../../assets/fonts/JetBrainsMono-Regular.ttf");

/// 日志字号设置的单次调整步长。
///
/// 业务意图：
/// - 1px 步进足够细，用户可以在不改变布局密度的前提下微调阅读舒适度。
pub(in crate::app) const LOG_VIEWER_FONT_SIZE_STEP: f32 = 1.0;

/// 日志正文滚动条可见宽度。
///
/// 业务意图：
/// - GPUI `uniform_list` 默认只处理滚动交互，不预留可见滚动条；这里自绘轻量滚动条提示当前位置。
pub(in crate::app) const LOG_VIEWER_SCROLLBAR_WIDTH: f32 = 6.0;

/// 日志正文滚动条最小滑块长度。
///
/// 边界条件：
/// - 超长或超宽日志中按比例计算出的滑块可能过小，最小长度保证用户仍能看到并拖动滚动位置。
pub(in crate::app) const LOG_VIEWER_SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 36.0;

/// 日志正文滚动条距离内容边缘的内缩。
///
/// 业务意图：
/// - 让滚动条不贴到窗口边缘和 tab 内容边界，视觉上更轻量。
pub(in crate::app) const LOG_VIEWER_SCROLLBAR_PADDING: f32 = 3.0;

/// 编码下拉框按钮宽度。
///
/// 业务意图：
/// - 编码选择器现在嵌入右侧状态信息中，宽度需要比旧版左侧独立控件更紧凑。
/// - 该宽度仍需容纳最长的 “UTF-8 BOM” 文案和下拉箭头。
pub(in crate::app) const ENCODING_DROPDOWN_BUTTON_WIDTH: f32 = 94.0;

/// 编码下拉框弹层宽度。
///
/// 业务意图：
/// - 所有编码选项文案长度固定，使用稳定宽度避免菜单在不同平台字体下抖动。
pub(in crate::app) const ENCODING_DROPDOWN_WIDTH: f32 = 116.0;

/// 编码下拉按钮高度。
///
/// 业务意图：
/// - 菜单弹层位置需要和按钮底边对齐，因此按钮高度集中成常量，避免渲染处和定位处写两份数字。
pub(in crate::app) const ENCODING_DROPDOWN_BUTTON_HEIGHT: f32 = 24.0;

/// 编码下拉菜单距离按钮底边的垂直间隔。
///
/// 边界条件：
/// - 保留 2px 间隔可以让菜单边框和按钮边框不粘连。
pub(in crate::app) const ENCODING_DROPDOWN_MENU_GAP: f32 = 2.0;

/// 编码下拉框单项高度。
///
/// 边界条件：
/// - 当前每项只包含单行中文或 ASCII 编码名，不需要多行布局。
pub(in crate::app) const ENCODING_DROPDOWN_ITEM_HEIGHT: f32 = 28.0;

/// tab 右键菜单宽度。
///
/// 业务意图：
/// - 三个菜单项文案长度固定，使用稳定宽度可以避免菜单因为字体差异跳动。
pub(in crate::app) const TAB_CONTEXT_MENU_WIDTH: f32 = 132.0;

/// tab 右键菜单单项高度。
///
/// 边界条件：
/// - 菜单项只显示单行中文命令，不承载图标或快捷键。
pub(in crate::app) const TAB_CONTEXT_MENU_ITEM_HEIGHT: f32 = 30.0;

/// 搜索结果右键菜单宽度。
///
/// 业务意图：
/// - 搜索结果菜单只承载展开/收起两类批量操作，固定宽度可以和 tab 菜单保持一致的视觉节奏。
pub(in crate::app) const SEARCH_RESULTS_CONTEXT_MENU_WIDTH: f32 = 132.0;

/// 搜索结果右键菜单单项高度。
///
/// 边界条件：
/// - 菜单项只显示单行中文命令，不承载二级菜单。
pub(in crate::app) const SEARCH_RESULTS_CONTEXT_MENU_ITEM_HEIGHT: f32 = 30.0;

/// 加载日志来源菜单宽度。
///
/// 业务意图：
/// - Windows 原生选择器不能混选文件和目录，因此工具栏“加载日志”需要先给出两个明确入口。
/// - 菜单宽度要容纳“文件/压缩包”文案和图标，同时不遮挡后续工具栏按钮。
pub(in crate::app) const LOAD_SOURCE_MENU_WIDTH: f32 = 164.0;

/// 加载日志来源菜单单项高度。
///
/// 业务意图：
/// - 与目录树右键菜单保持接近的点击高度，让鼠标操作体验一致。
pub(in crate::app) const LOAD_SOURCE_MENU_ITEM_HEIGHT: f32 = 34.0;

/// 加载日志来源菜单到工具栏底部的间隔。
pub(in crate::app) const LOAD_SOURCE_MENU_TOP_GAP: f32 = 4.0;

/// 加载日志来源菜单相对点击位置的横向回退。
///
/// 业务意图：
/// - 点击通常落在按钮文字或图标中部，菜单向左回退后能更自然地贴住“加载日志”按钮。
pub(in crate::app) const LOAD_SOURCE_MENU_POINTER_BACKTRACK: f32 = 24.0;

/// 搜索对话框默认宽度。
///
/// 业务意图：
/// - 对话框需要同时容纳查询输入、范围切换和大小写开关，宽度固定可以让独立窗口尺寸稳定。
/// - 搜索对话框已经从主窗口浮层迁移为独立浮动窗口，因此不再参与右侧日志正文布局。
pub(in crate::app) const SEARCH_DIALOG_WIDTH: f32 = 430.0;

/// 搜索对话框独立窗口默认高度。
///
/// 业务意图：
/// - 独立窗口需要一次性容纳“当前文件”和“当前目录”两种搜索模式下的控件，避免切换范围时窗口高度跳变。
/// - 当前目录模式会额外显示目标目录输入框，因此高度按较高形态预留。
///
/// 边界条件：
/// - 该窗口不可调整大小；如果后续增加搜索历史、正则等更多控件，应同步重新定义窗口高度策略。
pub(in crate::app) const SEARCH_DIALOG_WINDOW_HEIGHT: f32 = 300.0;

/// 搜索关键字历史记录上限。
///
/// 业务意图：
/// - 搜索窗口再次打开时需要能恢复最近一次关键字，减少重复输入。
/// - 只保留最近 10 条，避免当前会话内频繁搜索导致状态无限增长；当前不持久化到磁盘，避免隐私规则未定义前保存用户日志关键字。
pub(in crate::app) const SEARCH_QUERY_HISTORY_LIMIT: usize = 10;

/// 搜索关键字历史下拉按钮宽度。
///
/// 业务意图：
/// - 搜索框右侧需要一个独立、稳定的点击区域展开历史关键字，避免和文本输入区的光标定位互相干扰。
pub(in crate::app) const SEARCH_HISTORY_DROPDOWN_BUTTON_WIDTH: f32 = 26.0;

/// 搜索关键字历史下拉菜单与搜索框的间距。
///
/// 业务意图：
/// - 菜单贴近搜索框但留出细微间隔，用户能明确它属于关键字输入框而不是下面的范围控件。
pub(in crate::app) const SEARCH_HISTORY_DROPDOWN_GAP: f32 = 4.0;

/// 搜索关键字历史下拉菜单单项高度。
///
/// 业务意图：
/// - 历史关键字是短文本选择项，固定高度让最多 10 条历史在滚动容器里保持稳定命中区域。
pub(in crate::app) const SEARCH_HISTORY_DROPDOWN_ITEM_HEIGHT: f32 = 28.0;

/// 搜索关键字历史下拉菜单最大高度。
///
/// 业务意图：
/// - 历史最多保存 10 条，但搜索窗口高度有限；菜单超过该高度后滚动，避免遮住整个窗口内容。
pub(in crate::app) const SEARCH_HISTORY_DROPDOWN_MAX_HEIGHT: f32 = 176.0;

/// 设置窗口默认宽度。
///
/// 业务意图：
/// - 设置窗口需要容纳左侧页签和右侧统一表单布局；日志页包含多行线程堆栈过滤输入区，因此需要比早期设置窗口更宽。
/// - 模型页是“配置列表 + 表单 + 四个操作按钮”的两栏布局，宽度不足会导致按钮越过详情卡片边框，因此按该页的最小可用宽度取值。
/// - 固定宽度可以让独立窗口在 macOS 和 Windows 上保持稳定布局，不受系统字体度量差异影响。
pub(in crate::app) const SETTINGS_WINDOW_WIDTH: f32 = 900.0;

/// 设置窗口默认高度。
///
/// 业务意图：
/// - 日志页需要直接粘贴线程堆栈，较高窗口可以减少输入区滚动，同时保留通用页紧凑布局。
/// - 模型页签需要容纳配置列表、表单和测试状态；固定高度配合页内滚动，避免不同页签切换时窗口跳动。
pub(in crate::app) const SETTINGS_WINDOW_HEIGHT: f32 = 520.0;

/// 设置窗口左侧页签栏宽度。
///
/// 业务意图：
/// - 页签名称为中文短文本，固定宽度可以让右侧内容区宽度稳定，后续增加更多设置项时仍易于扫描。
pub(in crate::app) const SETTINGS_TAB_SIDEBAR_WIDTH: f32 = 132.0;

/// 设置页线程过滤多行输入区高度。
///
/// 业务意图：
/// - 线程堆栈通常包含多行调用栈，输入区需要在设置窗口内提供足够预览空间，同时不能挤掉标题和说明。
pub(in crate::app) const THREAD_ANALYSIS_FILTER_TEXTAREA_HEIGHT: f32 = 300.0;

/// 设置页线程过滤输入区单行高度。
///
/// 业务意图：
/// - 多行输入区使用等宽字体展示堆栈，固定行高便于鼠标命中、选区绘制和滚动内容高度计算保持一致。
pub(in crate::app) const THREAD_ANALYSIS_FILTER_TEXT_LINE_HEIGHT: f32 = 18.0;

/// 搜索输入框高度。
///
/// 业务意图：
/// - 搜索框只承载单行查询词，不支持多行输入，因此固定高度可以简化键盘和点击焦点处理。
pub(in crate::app) const SEARCH_INPUT_HEIGHT: f32 = 30.0;

/// 搜索窗口内容区内边距。
///
/// 业务意图：
/// - 搜索历史下拉菜单作为内容区绝对定位弹层，需要和输入框左、右边缘对齐；该值和搜索窗口内容容器的 `p_3` 保持一致。
pub(in crate::app) const SEARCH_DIALOG_CONTENT_PADDING: f32 = 16.0;

/// 搜索结果面板默认高度。
///
/// 业务意图：
/// - 面板从右侧内容区底部弹出，默认高度需要能展示多条结果，同时保留上方日志上下文。
pub(in crate::app) const SEARCH_RESULTS_PANEL_DEFAULT_HEIGHT: f32 = 260.0;

/// 搜索结果面板最小高度。
///
/// 边界条件：
/// - 继续允许用户拖小结果面板，但不能小到关闭按钮、摘要和至少一条结果不可见。
pub(in crate::app) const SEARCH_RESULTS_PANEL_MIN_HEIGHT: f32 = 140.0;

/// 搜索结果面板相对右侧内容区的最大高度比例。
///
/// 业务意图：
/// - 用户要求结果面板可以上下拖动高度，但不能完全覆盖日志正文。
pub(in crate::app) const SEARCH_RESULTS_PANEL_MAX_RATIO: f32 = 0.6;

/// 搜索结果面板拖拽条高度。
pub(in crate::app) const SEARCH_RESULTS_PANEL_RESIZER_HEIGHT: f32 = 8.0;

/// 搜索结果标题栏顶部视觉留白。
///
/// 业务意图：
/// - 拖拽命中区需要保持足够高度，方便用户调整面板；但标题内容不应因此显得上边距过大。
/// - 将视觉留白和拖拽命中区拆开，标题栏能保持紧凑，同时顶部仍保留可拖动热区。
pub(in crate::app) const SEARCH_RESULTS_PANEL_HEADER_TOP_PADDING: f32 = 3.0;

/// 搜索结果行固定高度。
///
/// 业务意图：
/// - 结果列表使用虚拟列表，固定行高可以避免大量命中时创建全部行元素。
pub(in crate::app) const SEARCH_RESULT_ROW_HEIGHT: f32 = 34.0;

/// 搜索结果面板滚动条可见宽度。
///
/// 业务意图：
/// - 搜索结果可能包含大量历史记录、文件分组和命中明细，面板也需要像日志正文一样提供稳定可见的滚动位置提示。
/// - 宽度沿用日志正文滚动条的视觉尺度，避免底部面板看起来像另一套控件体系。
pub(in crate::app) const SEARCH_RESULTS_SCROLLBAR_WIDTH: f32 = 6.0;

/// 搜索结果面板滚动条最小滑块长度。
///
/// 边界条件：
/// - 大量结果会让按比例计算的滑块非常短，最小长度保证用户仍能稳定点击和拖动。
pub(in crate::app) const SEARCH_RESULTS_SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 36.0;

/// 搜索结果面板滚动条距离列表边缘的内缩。
///
/// 业务意图：
/// - 右侧保留轻微内缩，让滚动条不贴边，并和日志正文、左侧目录树滚动条的视觉位置保持一致。
pub(in crate::app) const SEARCH_RESULTS_SCROLLBAR_PADDING: f32 = 3.0;

/// `Ctrl+C` 在部分 macOS 输入路径下对应的 ASCII 控制字符。
///
/// 业务意图：
/// - 日志正文复制和搜索快捷键使用同一套全局键盘入口；复制也需要兼容 Control 字母键被平台编码成控制字符的情况。
pub(in crate::app) const CONTROL_C_CODE: &str = "\u{3}";

/// `Ctrl+V` 在部分 macOS 输入路径下对应的 ASCII 控制字符。
///
/// 业务意图：
/// - 搜索关键字和目录输入框需要支持粘贴；日志查看器只读，因此在查看器中粘贴会打开搜索窗口并填入剪贴板文本。
pub(in crate::app) const CONTROL_V_CODE: &str = "\u{16}";

/// `Ctrl+X` 在部分平台输入路径下对应的 ASCII 控制字符。
///
/// 业务意图：
/// - 线程过滤多行输入区需要支持剪切；不同系统可能把 `Ctrl+X` 表示为字母 `x` 或控制字符。
pub(in crate::app) const CONTROL_X_CODE: &str = "\u{18}";

/// `Ctrl+A` 在部分平台输入路径下对应的 ASCII 控制字符。
///
/// 业务意图：
/// - 搜索关键字和目录输入框需要支持全选；不同系统可能把 `Ctrl+A` 表示为字母 `a` 或控制字符。
/// - 与复制快捷键保持同一套兼容策略，避免 Windows/macOS 键盘路径出现只在某个平台生效的问题。
pub(in crate::app) const CONTROL_A_CODE: &str = "\u{1}";
