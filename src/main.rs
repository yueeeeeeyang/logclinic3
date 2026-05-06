//! LogClinic 桌面客户端的程序入口。
//!
//! 当前阶段负责创建 GPUI 主窗口、顶部工具栏、可拖动左右分栏，以及左侧日志目录树。
//! “加载日志”已经接入真实路径选择和来源扫描，支持文件、目录和压缩包目录项展示；
//! 后续日志正文读取、编码识别、搜索过滤和节点点击等业务功能必须在明确业务规则和
//! 验收标准后再接入，避免入口层提前固化日志查看行为。
//!
//! 跨平台约束：
//! - macOS 和 Windows 都需要从同一个入口启动主窗口，因此这里不写平台专属逻辑。
//! - 窗口尺寸使用 GPUI 的逻辑像素表达，由 GPUI 负责映射到具体平台窗口系统。
//! - 当前不持久化窗口位置或尺寸，避免在尚未定义配置目录和权限规则前写入用户文件。

use std::{borrow::Cow, collections::HashSet, path::PathBuf};

use gpui::{
    AppContext, Application, ClickEvent, Context, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, PathPromptOptions, Render,
    SharedString, StatefulInteractiveElement, Styled as _, TitlebarOptions,
    UniformListScrollHandle, Window, WindowBounds, WindowOptions, div, px, rgb, size, uniform_list,
};
use lucide_icons::{Icon, LUCIDE_FONT_BYTES};

mod log_loader;

use log_loader::{
    LoadedLogTree, LogTreeEntryKind, LogTreeRow as LoadedLogTreeRow, load_log_sources,
};

/// 应用主窗口的标题。
///
/// 业务意图：
/// - 第一阶段只验证窗口创建，因此标题使用稳定的产品名 `LogClinic`。
/// - 后续如果需要显示当前打开文件名或工作区状态，应在明确标题格式规则后再扩展。
const MAIN_WINDOW_TITLE: &str = "LogClinic";

/// 主窗口的默认宽度。
///
/// 业务约束：
/// - 用户明确要求默认窗口宽度保持为 1200px。
/// - 这里使用浮点字面量是为了匹配 GPUI `px` 的尺寸 API，避免在调用处反复转换。
const MAIN_WINDOW_WIDTH: f32 = 1200.0;

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
/// - 用户要求工具栏高度降低 1/3，因此从上一版 48px 调整为 32px。
/// - 固定高度可以避免后续内容区域因为按钮文本、图标或平台字体差异产生首屏布局抖动。
///
/// 边界条件：
/// - 当前不实现可换行工具栏；如果后续窗口宽度允许缩小，需要再定义窄宽度下的折叠策略。
const TOOLBAR_HEIGHT: f32 = 32.0;

/// 主界面工具栏按钮的水平内边距。
///
/// 实现原因：
/// - 当前按钮是文字按钮，不再使用边框和实体背景，因此水平内边距需要更紧凑。
/// - 统一内边距可以保证三个入口在 macOS 和 Windows 字体渲染差异下仍保持一致点击区域。
const TOOLBAR_BUTTON_HORIZONTAL_PADDING: f32 = 8.0;

/// 主界面工具栏按钮的垂直内边距。
///
/// 边界说明：
/// - 工具栏高度降低到 32px 后，垂直内边距必须同步收紧，避免文字和图标被裁切。
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
/// - 当前图标尺寸与工具栏 32px 高度配套，避免在 macOS 和 Windows 字体度量差异下发生裁切。
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
/// - 因此这里保留 6px 透明命中区域，让鼠标更容易命中；真正可见线条由
///   `SPLITTER_VISIBLE_WIDTH` 单独控制。
///
/// 边界条件：
/// - 当前仅支持鼠标拖动，不支持键盘调整、双击重置或触控手势。
/// - 命中区域会占用布局宽度，因此拖动边界计算必须使用该值，而不是可见线条宽度。
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

/// 分割线命中区内可见线条两侧的填充宽度。
///
/// 业务意图：
/// - 分割线必须在 6px 拖动命中区内居中绘制，同时不能在左侧面板和线条之间露出白色缝隙。
/// - 因此将命中区拆成“左侧填充 + 1px 线条 + 右侧填充”，左右填充分别使用相邻面板背景色。
///
/// 边界条件：
/// - 该值由命中区宽度和可见线条宽度推导，保持拖动命中范围和视觉线条宽度解耦。
/// - 如果后续调整 `SPLITTER_HIT_WIDTH` 或 `SPLITTER_VISIBLE_WIDTH`，这里会自动保持居中。
const SPLITTER_SIDE_FILL_WIDTH: f32 = (SPLITTER_HIT_WIDTH - SPLITTER_VISIBLE_WIDTH) / 2.0;

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
}

impl MainView {
    /// 创建主窗口根视图。
    ///
    /// 业务意图：
    /// - 集中初始化所有首屏 UI 状态，避免在 `main` 的窗口创建回调中散落默认值。
    /// - 左侧栏默认 300px 是用户明确要求，必须从这里作为唯一入口初始化。
    fn new() -> Self {
        Self {
            left_panel_width: LEFT_PANEL_DEFAULT_WIDTH,
            is_resizing_splitter: false,
            load_state: LogTreeLoadState::Empty,
            log_tree_scroll_handle: UniformListScrollHandle::new(),
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
    /// - 扫描完成后只更新左侧目录树状态，右侧日志正文保持空白，符合当前阶段范围。
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
    /// - 当前不读取日志正文、不监听文件变化。
    /// - 真实节点支持展开/收起，行渲染交给虚拟列表处理；后续仍需定义节点选择和正文读取规则。
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
    /// - 标题用于说明左侧面板当前展示的是日志目录结构，而不是日志正文或诊断结果。
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
                    ))
                    .child("日志目录"),
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
    }

    /// 渲染真实加载结果中的单行节点。
    ///
    /// 业务意图：
    /// - 根据加载模块提供的节点类型选择图标、颜色和展开占位，保持 UI 逻辑不反向解析文件名。
    /// - 文件夹和压缩包节点支持点击展开/收起，普通文件、符号链接和错误节点当前只展示。
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
        let is_expanded = self.is_log_tree_node_expanded(row.id);
        let expand_icon = if row.has_children {
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
            row.id,
            row.depth,
            expand_icon,
            item_icon,
            icon_color,
            row.has_children,
            row.label.clone(),
            row.meta.as_deref(),
            context,
        )
    }

    /// 渲染左侧日志目录树中的通用单行节点。
    ///
    /// 业务意图：
    /// - 真实节点共享同一行模板，避免文本截断、缩进和元信息样式分叉。
    /// - 可展开节点在这里绑定点击事件，保证虚拟列表只为当前可见行注册交互。
    ///
    /// 边界条件：
    /// - 当前点击只控制展开/收起，不读取日志正文；后续接入文件节点点击前必须定义来源定位规则。
    fn render_log_tree_row(
        node_id: usize,
        depth: usize,
        expand_icon: Option<Icon>,
        item_icon: Icon,
        icon_color: u32,
        can_toggle: bool,
        label: String,
        meta: Option<&str>,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let left_padding = LOG_TREE_ROW_HORIZONTAL_PADDING + depth as f32 * LOG_TREE_ROW_INDENT;

        let row = div()
            .id(SharedString::from(format!("log-tree-row-{}", node_id)))
            .flex()
            .items_center()
            .gap_1()
            .h(px(LOG_TREE_ROW_HEIGHT))
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
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(Self::render_log_tree_meta(meta));

        if can_toggle {
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
    fn render_log_tree_meta(meta: Option<&str>) -> impl IntoElement {
        div()
            .flex_none()
            .text_xs()
            .text_color(rgb(0x8c959f))
            .child(meta.unwrap_or_default().to_string())
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
        let max_left_width =
            (window_width - SPLITTER_HIT_WIDTH - RIGHT_PANEL_MIN_WIDTH).max(LEFT_PANEL_MIN_WIDTH);
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

    /// 渲染左右分栏内容区域。
    ///
    /// 业务意图：
    /// - 未加载日志时主内容区占满剩余空间并显示友好提示，不出现空的左侧目录区域。
    /// - 加载完成后切换为左右两栏，左侧展示目录树，右侧为后续日志正文主视图预留空间。
    ///
    /// 边界条件：
    /// - 当前只有加载完成状态显示可拖动分割线；未加载、加载中或失败状态不显示分割线。
    /// - 当前右侧仍保持空白，避免用户误认为日志正文已完成。
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
            .on_mouse_move(context.listener(Self::resize_splitter))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(Self::stop_resizing_splitter),
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
                    .h_full()
                    .flex_1()
                    .bg(rgb(0xffffff))
                    .overflow_hidden(),
            )
    }

    /// 渲染左右两栏之间的可拖动分割线。
    ///
    /// 业务意图：
    /// - 分割线提供明确的拖动命中区域，让用户可以动态调整左右区域宽度。
    /// - 可见部分保持 1px 细线，外层透明区域负责鼠标命中，满足“细线但可拖动”的要求。
    /// - 细线在命中区内居中绘制，两侧填充分别匹配左右面板背景，避免拖动范围露出独立白色间隔。
    ///
    /// 边界条件：
    /// - 当前只支持水平拖动，不能双击重置，也不能通过键盘调整。
    /// - 命中区域使用 `SPLITTER_HIT_WIDTH`，视觉线条使用 `SPLITTER_VISIBLE_WIDTH`。
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
            .w(px(SPLITTER_HIT_WIDTH))
            .flex_none()
            .cursor_col_resize()
            .child(
                div()
                    .h_full()
                    .w(px(SPLITTER_SIDE_FILL_WIDTH))
                    .bg(rgb(0xf8fafc)),
            )
            .child(
                div()
                    .h_full()
                    .w(px(SPLITTER_VISIBLE_WIDTH))
                    .bg(rgb(line_color)),
            )
            .child(
                div()
                    .h_full()
                    .w(px(SPLITTER_SIDE_FILL_WIDTH))
                    .bg(rgb(0xffffff)),
            )
            .on_mouse_down(
                MouseButton::Left,
                context.listener(Self::start_resizing_splitter),
            )
    }
}

impl Render for MainView {
    /// 渲染主窗口内容。
    ///
    /// 实现原因：
    /// - 顶部工具栏提供全局入口，内容区提供左右分栏和左侧日志目录树。
    /// - 右侧主内容区仍不放占位文案，避免用户误以为日志正文、诊断或设置功能已经完成。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0xffffff))
            .child(self.render_toolbar(context))
            .child(self.render_content(context))
    }
}

/// 程序入口。
///
/// 业务意图：
/// - 初始化 GPUI 应用并创建唯一主窗口。
/// - 当前阶段负责验证窗口、工具栏、目录树和日志来源加载入口，暂不挂载菜单栏、日志正文读取或配置系统。
///
/// 错误处理：
/// - 主窗口创建失败意味着桌面应用无法进入可交互状态，属于启动期不可恢复错误。
/// - 这里使用 `expect` 并配套中文错误说明，是为了让开发期和测试期能直接暴露
///   窗口系统、图形环境或 GPUI 初始化问题；普通业务错误后续不得采用这种处理方式。
fn main() {
    Application::new().run(|app| {
        // 注册 Lucide 第三方图标字体，确保工具栏图标在 macOS 和 Windows 上使用同一套字形。
        // 这里在窗口创建前注册字体；如果注册失败，工具栏图标无法可靠渲染，应用启动应直接暴露错误。
        app.text_system()
            .add_fonts(vec![Cow::Borrowed(LUCIDE_FONT_BYTES)])
            .expect("注册 Lucide 图标字体失败，工具栏图标无法可靠渲染");

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

        app.open_window(window_options, |_window, app| {
            app.new(|_app| MainView::new())
        })
        .expect("创建 LogClinic 主窗口失败，应用无法继续启动");
    });
}
