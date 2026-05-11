// 主窗口 UI 状态类型。
//
// 业务意图：
// - 集中维护 MainView 周边的纯 UI 状态、菜单状态、滚动状态、输入状态和后台任务结果类型。
// - 这些类型只在 app 功能域内部使用，不暴露给日志读取、搜索、压缩包、HPROF 或 AI 业务域。

use super::*;

/// 日志页操作栏按钮的声明式配置。
///
/// 业务意图：
/// - 将按钮标签和图标绑定在一起，避免日志页操作栏渲染代码里出现分散的硬编码。
/// - 后续如果按钮需要权限、禁用态或快捷键，可以在这个结构上扩展字段。
///
/// 边界条件：
/// - 当前只包含静态字符串，所有按钮在进程生命周期内固定不变。
/// - “加载日志”已绑定真实系统选择器和路径扫描流程；“搜索”复用现有独立搜索窗口逻辑。
pub(in crate::app) struct ToolbarAction {
    /// 按钮前置图标。
    ///
    /// 业务意图：
    /// - 使用 Lucide 图标库提供的类型安全枚举，替换上一版难看的纯文本符号。
    /// - 图标语义需要和按钮动作一致，帮助用户快速识别入口类别。
    pub(in crate::app) icon: Icon,

    /// 按钮显示文案。
    ///
    /// 业务意图：
    /// - 文案直接来自用户当前需求，后续如需国际化或快捷键提示再统一抽象。
    pub(in crate::app) label: &'static str,
}

/// 日志页操作栏按钮列表。
///
/// 业务意图：
/// - “加载日志”使用 `FileText`，表达日志文本文件入口。
/// - “搜索”使用 `Search`，提供鼠标入口打开搜索窗口，避免快捷键异常时用户无法触达搜索能力。
///
/// 边界条件：
/// - 当前图标依赖启动时注册的 Lucide 字体；如果字体注册失败，启动阶段会直接暴露错误。
pub(in crate::app) const TOOLBAR_ACTIONS: &[ToolbarAction] = &[
    ToolbarAction {
        icon: Icon::FileText,
        label: "加载日志",
    },
    ToolbarAction {
        icon: Icon::Search,
        label: "搜索",
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
pub(in crate::app) struct LoadedLogTreeState {
    /// 加载层生成的完整目录树。
    ///
    /// 业务意图：
    /// - 完整树用于在用户切换展开状态时重新计算可见行。
    /// - 后续接入节点点击时，也应从完整树继续补充来源定位信息，而不是只依赖可见行。
    pub(in crate::app) tree: LoadedLogTree,

    /// 当前处于展开状态的节点 ID 集合。
    ///
    /// 业务意图：
    /// - 使用集合可以让展开/收起切换保持 O(1) 查询，避免滚动渲染时反复线性查找。
    /// - 只记录展开节点；未出现的可展开节点视为收起。
    pub(in crate::app) expanded_node_ids: HashSet<usize>,

    /// 当前需要展示给目录树虚拟列表的可见行。
    ///
    /// 业务意图：
    /// - 文件很多时不能把完整树每一行都渲染成 GPUI 元素，否则滚动会卡顿。
    /// - 先按展开状态算出可见行，再交给 `uniform_list` 按可视区间懒渲染。
    pub(in crate::app) visible_rows: Vec<LoadedLogTreeRow>,

    /// 当前加载结果是否只包含一个可打开日志来源。
    ///
    /// 业务意图：
    /// - 用户加载单个日志时应直接进入右侧浏览，不再显示只有一个文件的左侧树。
    /// - 目录或压缩包内部如果最终只有一个可读日志，也按同一规则处理；多文件仍保留树用于选择。
    ///
    /// 边界条件：
    /// - 加载过程中出现错误时不隐藏树，避免错误节点被自动打开流程吞掉，用户仍能看到失败原因。
    /// - 只统计 `File` 节点且必须带有 `LogFileSource`，目录、压缩包容器、符号链接和错误节点不算可打开日志。
    pub(in crate::app) single_log_source: Option<LogFileSource>,
}

impl LoadedLogTreeState {
    /// 根据完整加载结果创建 UI 交互状态。
    ///
    /// 业务意图：
    /// - 首次加载时按用户要求默认展开 2 级目录，避免大目录一次性完全展开导致首屏过载。
    /// - 初始化后立即构建可见行缓存，保证标题摘要、虚拟列表数量和点击状态同步。
    pub(in crate::app) fn new(tree: LoadedLogTree) -> Self {
        let expanded_node_ids = tree
            .rows
            .iter()
            .filter(|row| row.has_children && row.depth < LOG_TREE_DEFAULT_EXPANDED_DEPTH)
            .map(|row| row.id)
            .collect();
        let single_log_source = Self::single_log_source_for_tree(&tree);

        let mut state = Self {
            tree,
            expanded_node_ids,
            visible_rows: Vec::new(),
            single_log_source,
        };
        state.rebuild_visible_rows();
        state
    }

    /// 返回整棵加载树中唯一可打开日志来源。
    ///
    /// 业务意图：
    /// - 加载单个普通文件、只含一个日志的目录、只含一个日志成员的压缩包时，主界面可以跳过左侧树并自动打开日志。
    ///
    /// 边界条件：
    /// - 如果存在扫描错误，保持 `None`，让左侧树继续展示错误行，避免用户误以为目录已完整加载。
    /// - 如果发现两个及以上可打开文件，立即返回 `None`，避免自动打开其中任意一个造成选择歧义。
    pub(in crate::app) fn single_log_source_for_tree(
        tree: &LoadedLogTree,
    ) -> Option<LogFileSource> {
        if tree.error_count > 0 {
            return None;
        }

        let mut single_source: Option<LogFileSource> = None;
        for row in &tree.rows {
            if row.kind != LogTreeEntryKind::File {
                continue;
            }

            let Some(source) = row.source.clone() else {
                continue;
            };
            if single_source.replace(source).is_some() {
                return None;
            }
        }

        single_source
    }

    /// 返回当前加载结果中唯一可打开日志来源。
    ///
    /// 业务意图：
    /// - 该值在构建 UI 状态时缓存，渲染每一帧不需要重新遍历大目录树。
    pub(in crate::app) fn single_log_source(&self) -> Option<LogFileSource> {
        self.single_log_source.clone()
    }

    /// 清理加载树持有的临时物化路径。
    ///
    /// 业务意图：
    /// - 7Z 在加载阶段会把内部成员物化成本地文件，重新加载日志后旧树不再可见，应释放对应磁盘空间。
    /// - 清理目录失败不影响 UI 状态切换，异常退出残留由下次启动的过期清理兜底。
    pub(in crate::app) fn cleanup_temporary_paths(&self) {
        for path in &self.tree.temporary_paths {
            if path.is_dir() {
                let _ = fs::remove_dir_all(path);
            } else {
                cleanup_materialized_file(path);
            }
        }
    }

    /// 返回标题区域展示的加载摘要。
    ///
    /// 边界条件：
    /// - 摘要来自加载层统计，表达完整树节点数量，而不是当前可见行数量。
    pub(in crate::app) fn summary(&self) -> &str {
        &self.tree.summary
    }

    /// 判断指定节点是否处于展开状态。
    ///
    /// 业务意图：
    /// - 渲染可见行时需要根据该状态选择向右或向下的展开箭头。
    pub(in crate::app) fn is_expanded(&self, node_id: usize) -> bool {
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
    pub(in crate::app) fn single_file_source_for_archive(
        &self,
        node_id: usize,
    ) -> Option<LogFileSource> {
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
    pub(in crate::app) fn toggle_node(&mut self, node_id: usize) {
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
    pub(in crate::app) fn rebuild_visible_rows(&mut self) {
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
/// - `Loaded` 变体通过装箱保存完整树状态，避免加载状态枚举在 UI 状态中被大 payload 放大。
pub(in crate::app) enum LogTreeLoadState {
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
    Loaded(Box<LoadedLogTreeState>),
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
pub(in crate::app) struct OpenLogTab {
    /// 当前进程内唯一 tab ID。
    pub(in crate::app) id: usize,
    /// 日志文件来源。
    pub(in crate::app) source: LogFileSource,
    /// 用于去重的稳定来源键。
    pub(in crate::app) source_key: String,
    /// tab 上展示的短标题。
    pub(in crate::app) title: String,
    /// 用户当前选择的编码策略。
    pub(in crate::app) encoding_choice: EncodingChoice,
    /// 已读取的原始字节。
    ///
    /// 业务意图：
    /// - 自动检测失败时仍保留原始字节，用户手动切换编码可以直接重新解码。
    /// - 读取失败时保持 `None`，因为没有可复用的正文数据。
    pub(in crate::app) raw_bytes: Option<Arc<Vec<u8>>>,
    /// tab 当前正文状态。
    pub(in crate::app) state: LogTabState,
    /// 当前 tab 的日志正文虚拟列表滚动句柄。
    ///
    /// 业务意图：
    /// - 每个 tab 独立保存滚动上下文，切换 tab 时不会把其它文件的滚动位置混进来。
    /// - 普通内存日志继续把该句柄绑定到 `uniform_list`；分页日志只复用它的横向兼容语义，真实纵向滚动由 `paged_scroll` 保存。
    pub(in crate::app) scroll_handle: UniformListScrollHandle,
    /// 分页日志正文视口测量句柄。
    ///
    /// 业务意图：
    /// - 分页模式不能再让 GPUI 布局完整行数，因此需要单独跟踪正文视口 bounds，用于滚动条尺寸和鼠标命中坐标换算。
    /// - 该句柄不承载真实滚动偏移，避免 GPUI 在超大内容高度上使用 `f32` 坐标。
    pub(in crate::app) paged_viewport_handle: ScrollHandle,
    /// 分页日志的逻辑滚动位置。
    ///
    /// 业务意图：
    /// - 超大日志的真实滚动距离可能达到数亿像素，必须用 `f64` 保存在应用状态中，渲染时再映射到视口内的小坐标。
    /// - 普通内存日志不读取该字段；切换编码或重新加载时必须重置。
    pub(in crate::app) paged_scroll: PagedLogScrollState,
    /// 打开后需要滚动定位的目标行。
    ///
    /// 业务意图：
    /// - 搜索结果点击可能打开一个尚未读取完成的新 tab，目标行必须暂存到 tab 上，等后台解码成功后再滚动。
    /// - 使用 0 基行号与 `DecodedLogDocument.lines` 下标保持一致，避免 UI 展示行号和数据下标混用。
    pub(in crate::app) pending_scroll_to_line: Option<usize>,
    /// 最近一次通过搜索结果跳转的命中行。
    ///
    /// 业务意图：
    /// - 点击搜索结果后不仅要滚动到目标位置，还要用背景色标记命中行，避免用户在密集日志中丢失上下文。
    /// - 该状态只属于当前 tab 的临时视觉反馈，不持久化，也不影响日志语法高亮。
    pub(in crate::app) highlighted_search_line: Option<usize>,
    /// 当前 tab 内由用户手动打标的日志行。
    ///
    /// 业务意图：
    /// - 用户排查日志时经常需要在多个关键位置之间来回跳转，标记行只属于当前打开的 tab。
    /// - 使用 `BTreeSet` 保存 0 基行号，可以天然按行号排序，便于 `F2` 查找下一个标记并在末尾循环回到第一个。
    ///
    /// 边界条件：
    /// - 标记不写入配置、不跨会话保存；关闭 tab 或重新加载日志后自然清空。
    /// - 切换编码时保留标记，因为同一份原始日志只是重新解码，用户已经标出的行号仍有参考价值。
    pub(in crate::app) marked_lines: BTreeSet<usize>,
    /// 最近一次通过 `F2` 跳转到的标记行。
    ///
    /// 业务意图：
    /// - 连续按 `F2` 时应从上一次跳转目标之后继续寻找，而不是每次都从当前视口顶部开始。
    /// - 当用户手动滚动到其它位置后，如果该行不再可见，下一次 `F2` 会重新以当前可视顶部为起点。
    pub(in crate::app) last_marker_jump_line: Option<usize>,
    /// 当前 tab 内日志正文的只读文本选择范围。
    ///
    /// 业务意图：
    /// - 日志正文采用虚拟列表自绘，不是系统文本控件，GPUI 不会自动保存跨行选择状态。
    /// - 将选择范围绑定到 tab，可以让用户切换 tab 后仍保留当前文件的选择上下文，并支持复制和填入搜索框。
    ///
    /// 边界条件：
    /// - 选择范围使用行号和字符列，不使用 UTF-8 字节下标；复制时再转换为安全字节边界，避免中文被截断。
    /// - 重新解码、重新加载或打开失败时必须清理该字段，因为旧行列可能不再对应新文档。
    pub(in crate::app) text_selection: Option<LogTextSelection>,
    /// 当前正在拖拽选择时的固定起点。
    ///
    /// 业务意图：
    /// - 鼠标按下确定锚点，后续移动只更新焦点，才能正确支持从下往上或从右往左反向选择。
    /// - 鼠标释放后清空该临时字段，但保留 `text_selection` 供复制和搜索预填使用。
    pub(in crate::app) selection_drag_anchor: Option<LogTextPosition>,
}

/// 分页日志的逻辑滚动位置。
///
/// 业务意图：
/// - `top_px` 和 `left_px` 都以日志正文内容坐标表示，不直接交给 GPUI 布局系统。
/// - 渲染时只根据这两个值计算“当前视口附近有哪些真实行号”，从而避开几千万行带来的 `f32` 精度损失。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(in crate::app) struct PagedLogScrollState {
    /// 当前视口顶部对应的纵向内容偏移，单位为逻辑像素。
    pub(in crate::app) top_px: f64,
    /// 当前视口左侧对应的横向内容偏移，单位为逻辑像素。
    pub(in crate::app) left_px: f64,
}

/// 日志正文中的文本位置。
///
/// 业务意图：
/// - 自绘日志行没有系统 selection，因此需要一个轻量位置类型表达“第几行第几个字符”。
/// - `column` 使用 Unicode 字符序号而不是字节序号，保证中文、全角符号和 emoji 不会被拆成非法 UTF-8。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::app) struct LogTextPosition {
    /// 0 基日志行号，对应 `DecodedLogDocument.lines` 下标。
    pub(in crate::app) line_index: usize,
    /// 0 基字符列，对应该行 `chars()` 序号。
    pub(in crate::app) column: usize,
}

/// 日志正文只读选择范围。
///
/// 业务意图：
/// - `anchor` 表示鼠标按下位置，`focus` 表示当前拖拽位置；二者顺序不固定。
/// - 复制、渲染选区和填充搜索框前都通过 `normalized` 统一为从前到后的范围。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct LogTextSelection {
    /// 选择起始锚点。
    pub(in crate::app) anchor: LogTextPosition,
    /// 选择当前焦点。
    pub(in crate::app) focus: LogTextPosition,
}

impl LogTextSelection {
    /// 返回按文档顺序排列后的选择范围端点。
    ///
    /// 边界条件：
    /// - 用户可能从后往前拖选，因此不能假设 `anchor <= focus`。
    pub(in crate::app) fn normalized(&self) -> (LogTextPosition, LogTextPosition) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// 判断当前选择是否为空。
    pub(in crate::app) fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }
}

/// 单行日志渲染所需的输入数据。
///
/// 业务意图：
/// - 日志行渲染需要同时知道 tab、行号、正文、高亮、行号列和搜索定位状态；集中成结构体可以避免函数参数膨胀。
/// - 该结构只在当前 UI 帧内短暂使用，不保存到 tab 状态，避免复制日志正文或高亮状态的生命周期变复杂。
pub(in crate::app) struct LogLineRenderData {
    /// 所属 tab ID，用于鼠标选择事件回写到正确 tab。
    pub(in crate::app) tab_id: usize,
    /// 0 基日志行号。
    pub(in crate::app) line_index: usize,
    /// 当前行原始正文。
    ///
    /// 业务意图：
    /// - 鼠标选择、复制和搜索都必须继续使用日志文件里的真实文本，不能因为显示层展开 `\t` 而改写内容。
    pub(in crate::app) line: String,
    /// 当前行用于视觉渲染的正文。
    ///
    /// 业务意图：
    /// - `\t` 会在显示层按固定 tab stop 展开为空格，让日志列间距与常见编辑器一致。
    /// - 该字段只用于 `StyledText`，不参与复制或保存。
    pub(in crate::app) display_line: String,
    /// 当前行语法高亮与选区高亮范围，范围基于 `display_line` 的 UTF-8 字节边界。
    pub(in crate::app) highlights: Vec<(Range<usize>, gpui::HighlightStyle)>,
    /// 行号列宽度。
    pub(in crate::app) line_number_width: f32,
    /// 当前日志显示字号。
    ///
    /// 业务意图：
    /// - 日志正文渲染由静态辅助函数完成，必须显式传入当前设置字号，避免继续读取旧的固定常量。
    pub(in crate::app) font_size: f32,
    /// 横向滚动时行号列的反向补偿偏移。
    pub(in crate::app) horizontal_line_number_offset: Pixels,
    /// 横向滚动时正文内容的局部偏移。
    ///
    /// 业务意图：
    /// - 普通 `uniform_list` 路径由 GPUI 整行平移，正文不需要额外偏移。
    /// - 分页窗口化路径只在视口内绝对定位行，必须单独平移正文，同时保持行号列固定。
    pub(in crate::app) horizontal_content_offset: Pixels,
    /// 当前行是否是搜索结果跳转后的目标行。
    pub(in crate::app) search_highlighted: bool,
    /// 当前行是否被用户手动标记。
    ///
    /// 业务意图：
    /// - 标记只显示在行号 gutter 内，避免改变正文背景后和搜索跳转高亮、选区高亮互相干扰。
    pub(in crate::app) marked: bool,
    /// 是否临时禁用行 hover 样式。
    ///
    /// 业务意图：
    /// - 调整搜索结果面板高度或拖动日志正文滚动条时，鼠标可能经过底层日志行；即使不应触发选区，GPUI hover 仍可能命中行。
    /// - 在渲染数据中显式携带禁用标记，可以让日志行完全不注册 hover 样式，避免拖动控件时底层正文闪动。
    pub(in crate::app) suppress_hover: bool,
    /// 当前主题调色板。
    pub(in crate::app) palette: AppThemePalette,
}

/// 日志行显示层展开结果。
///
/// 业务意图：
/// - 日志原文里的 `\t` 需要按固定 4 列 tab stop 展示为空格，但选区、搜索高亮和鼠标命中仍要回到原始文本。
/// - 该结构同时保存原始字节下标和显示字节下标的双向映射，避免中文、多字节字符和 tab 混合时出现高亮错位。
///
/// 边界条件：
/// - 映射数组长度分别为原始文本和显示文本的 `len + 1`，保证行尾位置也能安全换算。
/// - 对于 UTF-8 多字节字符的内部字节，映射会回退到字符起点；正常业务范围都应落在字符边界。
#[derive(Debug, PartialEq, Eq)]
pub(in crate::app) struct ExpandedLogLine {
    /// 展开 tab 后用于渲染的文本。
    pub(in crate::app) text: String,
    /// 原始文本字节下标到显示文本字节下标的映射。
    pub(in crate::app) original_to_display_bytes: Vec<usize>,
    /// 显示文本字节下标到原始文本字节下标的映射。
    pub(in crate::app) display_to_original_bytes: Vec<usize>,
}

/// 日志正文自绘滚动条的方向。
///
/// 业务意图：
/// - 右侧日志查看器需要同时支持纵向和横向滚动条拖动，两者共享拖动生命周期，但坐标轴和偏移字段不同。
/// - 使用枚举明确区分方向，避免在拖动计算中通过布尔值表达业务含义。
#[derive(Clone, Copy)]
pub(in crate::app) enum LogScrollbarAxis {
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
pub(in crate::app) struct LogScrollbarDrag {
    /// 正在拖动滚动条的 tab ID。
    pub(in crate::app) tab_id: usize,
    /// 当前拖动的滚动条方向。
    pub(in crate::app) axis: LogScrollbarAxis,
    /// 鼠标按下点相对滑块起点的偏移。
    pub(in crate::app) cursor_offset: Pixels,
}

/// 日志正文自绘滚动条的布局测量结果。
///
/// 业务意图：
/// - 滑块渲染、按下命中和拖动换算都必须使用同一套轨道与滑块尺寸，避免视觉位置和实际滚动位置不一致。
/// - 坐标均为相对日志列表视口左上角的局部坐标，调用方再根据方向映射到窗口坐标。
#[derive(Clone, Copy)]
pub(in crate::app) struct LogScrollbarMetrics {
    /// 滑块起点在当前轴向上的局部坐标。
    pub(in crate::app) thumb_start: Pixels,
    /// 滑块在当前轴向上的长度。
    pub(in crate::app) thumb_length: Pixels,
    /// 轨道起点在当前轴向上的局部坐标。
    pub(in crate::app) track_start: Pixels,
    /// 轨道在当前轴向上的总长度。
    pub(in crate::app) track_length: Pixels,
    /// 当前轴向可滚动的最大距离。
    pub(in crate::app) max_scroll: Pixels,
    /// 当前轴向可滚动的最大距离，使用 `f64` 保留超大分页日志的逻辑滚动精度。
    ///
    /// 业务意图：
    /// - 滑块自身仍以 `Pixels` 渲染，但分页日志的真实内容高度可能达到数亿像素。
    /// - 拖动换算回逻辑滚动位置时使用该字段，避免再次把深处行号压回 `f32` 精度。
    pub(in crate::app) max_scroll_px: f64,
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
pub(in crate::app) struct LogTreeScrollbarDrag {
    /// 鼠标按下点相对滑块顶部的偏移。
    pub(in crate::app) cursor_offset: Pixels,
}

/// 搜索结果面板滚动条正在被拖动时的临时状态。
///
/// 业务意图：
/// - 搜索结果面板使用虚拟列表承载历史记录和命中明细，滚动条拖动需要保存鼠标按下点在滑块内的偏移。
/// - 只保存偏移而不保存列表内容，避免拖动过程和搜索结果数据生命周期耦合。
///
/// 边界条件：
/// - 该状态只在鼠标左键拖动期间有效；关闭结果面板、释放鼠标或列表测量失效时都会清空。
#[derive(Clone, Copy)]
pub(in crate::app) struct SearchResultsScrollbarDrag {
    /// 鼠标按下点相对滑块顶部的偏移。
    pub(in crate::app) cursor_offset: Pixels,
}

/// 搜索对话框中的文本输入槽位。
///
/// 业务意图：
/// - 搜索对话框现在包含“关键字”和“目录目标”两个可编辑文本框，二者都需要走 GPUI 平台输入协议以支持中文 IME。
/// - 用枚举标识当前编辑槽位，可以复用同一套 UTF-16/UTF-8 范围转换和组合文本替换逻辑。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum SearchTextInputKind {
    /// 搜索关键字输入框。
    Query,
    /// 当前目录搜索的目标目录输入框。
    DirectoryTarget,
}

/// 设置页模型配置表单中的单行输入槽位。
///
/// 业务意图：
/// - “模型”页包含配置名称、Base URL、API Key 和模型 ID 四个自绘输入框，平台 IME 和鼠标命中需要知道当前焦点属于哪个字段。
/// - 使用枚举集中区分字段，可以复用同一套复制、粘贴、全选、删除和组合文本处理逻辑。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum ModelConfigInputKind {
    /// 用户可读的配置名称。
    Name,
    /// OpenAI 兼容 API 根路径。
    BaseUrl,
    /// OpenAI 兼容 API Key，可为空以兼容本地 Ollama/vLLM。
    ApiKey,
    /// Chat Completions 请求中的模型 ID。
    Model,
}

/// 单行文本输入框的通用编辑状态。
///
/// 业务意图：
/// - 搜索关键字、目录目标、快搜关键字和模型配置字段都是单行自绘输入框，核心状态都由 UTF-8 文本、
///   UTF-8 选择范围和 IME 组合范围组成。
/// - 抽出该状态后，后续按键处理、UTF-16/UTF-8 转换和组合文本替换可以逐步收敛到同一套工具函数。
///
/// 边界条件：
/// - 该结构不包含焦点句柄、字形布局或鼠标拖拽锚点；这些数据受窗口、渲染元素和具体交互区域约束，
///   第一轮仍保留在原功能域内，避免搜索窗口和设置窗口的焦点生命周期发生变化。
#[derive(Clone)]
pub(in crate::app) struct SingleLineTextInputState {
    /// 当前字段文本，使用 UTF-8 保存，平台输入协议回调时再和 UTF-16 范围互转。
    pub(in crate::app) text: String,
    /// 当前选择范围，按 UTF-8 字节下标保存，必须始终夹到字符边界。
    pub(in crate::app) selection_range: Range<usize>,
    /// 中文等输入法正在组合的文本范围，提交或取消组合时清空。
    pub(in crate::app) marked_range: Option<Range<usize>>,
}

impl SingleLineTextInputState {
    /// 创建空单行输入状态。
    pub(in crate::app) fn empty() -> Self {
        Self::from_text(String::new())
    }

    /// 使用指定文本创建单行输入状态并把光标放到末尾。
    pub(in crate::app) fn from_text(text: String) -> Self {
        let cursor = text.len();
        Self {
            text,
            selection_range: cursor..cursor,
            marked_range: None,
        }
    }

    /// 用新文本替换输入框内容并把光标放到末尾。
    ///
    /// 业务意图：
    /// - 切换搜索预填文本、模型配置或设置草稿时，输入框必须一次性切换到目标文本，不能保留旧选区或 IME 组合状态。
    pub(in crate::app) fn set_text(&mut self, text: String) {
        let cursor = text.len();
        self.text = text;
        self.selection_range = cursor..cursor;
        self.marked_range = None;
    }
}

/// 将平台 UTF-16 范围转换为单行输入内部可安全切片的 UTF-8 字节范围。
///
/// 业务意图：
/// - GPUI 平台输入协议按 UTF-16 计数，Rust `String` 必须按 UTF-8 字节边界切片。
/// - 搜索框、目录输入框、快搜关键字和模型配置字段共享该工具，保证中文、emoji 和其它非 ASCII 输入行为一致。
pub(in crate::app) fn single_line_range_from_utf16(
    text: &str,
    range_utf16: Range<usize>,
) -> Range<usize> {
    let start = single_line_byte_index_from_utf16(text, range_utf16.start);
    let end = single_line_byte_index_from_utf16(text, range_utf16.end);
    start.min(end)..end.max(start)
}

/// 将单行输入内部 UTF-8 字节范围转换为平台输入协议需要的 UTF-16 范围。
pub(in crate::app) fn single_line_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    let start = single_line_utf16_offset_from_byte(text, range.start);
    let end = single_line_utf16_offset_from_byte(text, range.end);
    start..end
}

/// 把 UTF-16 偏移映射到 UTF-8 字节边界。
///
/// 边界条件：
/// - 如果平台给出超过文本长度的偏移，统一夹到字符串末尾。
/// - 如果偏移落在代理对或多字节字符内部，返回该字符起点，保证后续 `replace_range` 安全。
pub(in crate::app) fn single_line_byte_index_from_utf16(text: &str, target_utf16: usize) -> usize {
    let mut utf16_cursor = 0usize;
    for (byte_index, character) in text.char_indices() {
        if utf16_cursor >= target_utf16 {
            return byte_index;
        }
        utf16_cursor += character.len_utf16();
    }
    text.len()
}

/// 把 UTF-8 字节边界映射到 UTF-16 偏移。
pub(in crate::app) fn single_line_utf16_offset_from_byte(text: &str, byte_index: usize) -> usize {
    text.char_indices()
        .take_while(|(index, _)| *index < byte_index)
        .map(|(_, character)| character.len_utf16())
        .sum()
}

/// 模型配置单行输入框的窗口相关状态。
///
/// 业务意图：
/// - GPUI 当前版本没有现成文本输入控件，设置页每个模型字段都需要保存文本、选区、IME 组合态和鼠标命中布局。
/// - 该状态只服务当前设置窗口会话；保存时才会转换成 `ModelProfile` 并写入配置文件。
pub(in crate::app) struct ModelConfigTextFieldState {
    /// 单行输入框的通用编辑状态。
    pub(in crate::app) input: SingleLineTextInputState,
    /// 当前字段焦点句柄，用于 GPUI 平台输入路由和光标绘制判断。
    pub(in crate::app) focus: gpui::FocusHandle,
    /// 最近一次绘制的单行字形布局，用于鼠标点击和拖拽反推出字符位置。
    pub(in crate::app) last_layout: Option<ShapedLine>,
    /// 最近一次绘制的输入框窗口坐标边界，用于 IME 候选窗口定位。
    pub(in crate::app) last_bounds: Option<Bounds<Pixels>>,
    /// 鼠标拖拽选择时的固定锚点，释放鼠标后清空。
    pub(in crate::app) selection_drag: Option<usize>,
}

impl ModelConfigTextFieldState {
    /// 创建一个空模型配置输入状态。
    pub(in crate::app) fn new(context: &mut Context<MainView>) -> Self {
        Self {
            input: SingleLineTextInputState::empty(),
            focus: context.focus_handle(),
            last_layout: None,
            last_bounds: None,
            selection_drag: None,
        }
    }

    /// 用新文本替换输入框内容并把光标放到末尾。
    ///
    /// 业务意图：
    /// - 切换模型配置或点击新增时，表单字段必须一次性切换到目标配置，不能保留旧选区或 IME 组合状态。
    pub(in crate::app) fn set_text(&mut self, text: String) {
        self.input.set_text(text);
        self.selection_drag = None;
    }

    /// 清空排版缓存。
    ///
    /// 边界条件：
    /// - 字段内容、掩码显示或窗口尺寸变化后，旧布局不再代表当前可见文本；清空后鼠标命中会安全回退到文本末尾。
    pub(in crate::app) fn clear_layout(&mut self) {
        self.last_layout = None;
        self.last_bounds = None;
    }
}

impl Deref for ModelConfigTextFieldState {
    type Target = SingleLineTextInputState;

    /// 让模型配置字段继续像普通单行输入状态一样访问文本、选区和组合范围。
    fn deref(&self) -> &Self::Target {
        &self.input
    }
}

impl DerefMut for ModelConfigTextFieldState {
    /// 让既有 IME 和按键处理逻辑可以在第一轮重构中继续就地修改通用输入状态。
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.input
    }
}

/// 模型测试按钮的 UI 状态。
///
/// 业务意图：
/// - 测试请求在后台执行，状态需要区分空闲、进行中、成功和失败，避免旧请求返回后覆盖新请求结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum ModelTestStatus {
    /// 尚未测试或结果被用户编辑操作清空。
    Idle,
    /// 当前正在执行的测试任务 ID。
    Testing { job_id: usize },
    /// 最近一次测试成功。
    Success(String),
    /// 最近一次测试失败，字符串为用户可见中文原因。
    Failed(String),
}

impl ModelTestStatus {
    /// 返回用户可见状态文案。
    pub(in crate::app) fn message(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::Testing { .. } => Some("测试中..."),
            Self::Success(message) | Self::Failed(message) => Some(message.as_str()),
        }
    }

    /// 返回是否处于正在测试状态。
    pub(in crate::app) fn is_testing(&self) -> bool {
        matches!(self, Self::Testing { .. })
    }
}

/// 线程日志分析过滤输入区中的单行排版缓存。
///
/// 业务意图：
/// - 多行输入区需要根据用户点击的窗口坐标反推出 UTF-8 字节下标；保存每行真实字形布局可以复用 GPUI 文本系统的命中算法。
/// - `byte_range` 不包含行尾换行符，光标落在换行符前后时分别映射到上一行末尾或下一行开头。
pub(in crate::app) struct ThreadAnalysisFilterLineLayout {
    /// 当前可视行对应的原始文本 UTF-8 字节范围。
    pub(in crate::app) byte_range: Range<usize>,
    /// 当前行的 GPUI 字形布局。
    pub(in crate::app) line: ShapedLine,
    /// 当前行在窗口中的绘制边界。
    pub(in crate::app) bounds: Bounds<Pixels>,
}

/// 主窗口当前展示的大功能页。
///
/// 业务意图：
/// - 主窗口左侧固定大导航只负责在日志分析、HPROF 解析和 AI 对话三个主要工作区之间切换。
/// - 状态只保存在当前会话，不写入配置文件，避免后续调整默认入口或恢复策略时被旧配置约束。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum MainFeature {
    /// 日志分析页，承载日志加载、目录树、日志 tab、搜索和线程日志分析入口。
    LogAnalysis,
    /// HPROF 解析页，承载 heap dump 文件选择、解析进度和 dominator tree 结果。
    HprofAnalysis,
    /// AI 对话页，承载本地会话历史、模型选择和 OpenAI 兼容流式对话。
    AiChat,
}

impl Default for MainFeature {
    /// 默认进入日志分析页。
    fn default() -> Self {
        Self::LogAnalysis
    }
}

impl MainFeature {
    /// 返回主功能在左侧大导航中的固定展示顺序。
    pub(in crate::app) fn all() -> &'static [Self] {
        &[Self::LogAnalysis, Self::HprofAnalysis, Self::AiChat]
    }

    /// 返回主功能中文名称。
    pub(in crate::app) fn label(self) -> &'static str {
        match self {
            Self::LogAnalysis => "日志分析",
            Self::HprofAnalysis => "HPROF解析",
            Self::AiChat => "AI对话",
        }
    }

    /// 返回主功能导航图标。
    pub(in crate::app) fn icon(self) -> Icon {
        match self {
            Self::LogAnalysis => Icon::Search,
            Self::HprofAnalysis => Icon::ChartNoAxesCombined,
            Self::AiChat => Icon::BotMessageSquare,
        }
    }
}

/// 左侧大导航中的可悬浮入口。
///
/// 业务意图：
/// - 导航栏只显示图标，hover 气泡需要知道当前入口名称和纵向位置；设置属于通用入口，不计入主功能状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum MainNavigationItem {
    /// 三个主功能页入口。
    Feature(MainFeature),
    /// 设置独立窗口入口。
    Settings,
}

impl MainNavigationItem {
    /// 返回导航入口中文名称。
    pub(in crate::app) fn label(self) -> &'static str {
        match self {
            Self::Feature(feature) => feature.label(),
            Self::Settings => "设置",
        }
    }

    /// 返回导航入口图标。
    pub(in crate::app) fn icon(self) -> Icon {
        match self {
            Self::Feature(feature) => feature.icon(),
            Self::Settings => Icon::Settings,
        }
    }

    /// 返回 hover 气泡在主窗口根节点中的垂直锚点。
    ///
    /// 业务意图：
    /// - 气泡必须绘制在右侧功能页之上，不能作为导航按钮子元素被后续兄弟节点盖住；因此这里用纯函数复刻导航按钮的垂直位置。
    /// - 顶部三个主功能从上向下定位，底部两个通用入口从下向上定位，避免依赖运行时窗口高度。
    pub(in crate::app) fn tooltip_anchor(self) -> MainNavigationTooltipAnchor {
        match self {
            Self::Feature(MainFeature::LogAnalysis) => {
                MainNavigationTooltipAnchor::Top(MAIN_NAV_PADDING + MAIN_NAV_TOOLTIP_BUTTON_INSET)
            }
            Self::Feature(MainFeature::HprofAnalysis) => MainNavigationTooltipAnchor::Top(
                MAIN_NAV_PADDING
                    + MAIN_NAV_BUTTON_SIZE
                    + MAIN_NAV_BUTTON_GAP
                    + MAIN_NAV_TOOLTIP_BUTTON_INSET,
            ),
            Self::Feature(MainFeature::AiChat) => MainNavigationTooltipAnchor::Top(
                MAIN_NAV_PADDING
                    + (MAIN_NAV_BUTTON_SIZE + MAIN_NAV_BUTTON_GAP) * 2.0
                    + MAIN_NAV_TOOLTIP_BUTTON_INSET,
            ),
            Self::Settings => MainNavigationTooltipAnchor::Bottom(
                MAIN_NAV_PADDING + MAIN_NAV_TOOLTIP_BUTTON_INSET,
            ),
        }
    }
}

/// 左侧大导航 hover 气泡的垂直锚点。
///
/// 业务意图：
/// - 主功能入口贴近导航顶部，通用入口贴近导航底部；气泡提到根节点覆盖绘制后，必须保留这两类入口的原始视觉位置。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::app) enum MainNavigationTooltipAnchor {
    /// 从主窗口顶部向下定位。
    Top(f32),
    /// 从主窗口底部向上定位。
    Bottom(f32),
}

/// 设置窗口当前激活的页签。
///
/// 业务意图：
/// - 设置窗口按用户要求拆成“通用 / 日志 / 模型 / 关于”页签；状态放在主视图中，避免关闭窗口后当前会话选择丢失。
/// - 当前页签状态只存在于进程内，不写入配置文件；后续若需要记忆页签，应先定义设置持久化策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum SettingsTab {
    /// 通用设置页签，当前承载主题和日志显示字号设置。
    General,
    /// 日志设置页签，当前承载线程日志分析过滤配置。
    Log,
    /// 模型设置页签，当前承载 OpenAI 兼容模型配置管理。
    Model,
    /// 关于页签，承载软件版本、作者和特色功能说明。
    About,
}

impl SettingsTab {
    /// 返回设置页签固定展示顺序。
    ///
    /// 业务意图：
    /// - 页签顺序集中定义，避免渲染和测试出现顺序分歧；关于收入口设置后放在最后，保留配置类页签优先级。
    pub(in crate::app) fn all() -> &'static [Self] {
        &[Self::General, Self::Log, Self::Model, Self::About]
    }

    /// 返回页签中文标签。
    pub(in crate::app) fn label(self) -> &'static str {
        match self {
            Self::General => "通用",
            Self::Log => "日志",
            Self::Model => "模型",
            Self::About => "关于",
        }
    }

    /// 返回页签图标。
    ///
    /// 业务意图：
    /// - 独立设置窗口的页签入口使用图标加文本，帮助用户快速区分通用配置和模型配置。
    pub(in crate::app) fn icon(self) -> Icon {
        match self {
            Self::General => Icon::Settings,
            Self::Log => Icon::FileText,
            Self::Model => Icon::MonitorCog,
            Self::About => Icon::Info,
        }
    }
}

/// 搜索对话框级键盘命令。
///
/// 业务意图：
/// - `Enter` 和 `Escape` 在搜索窗口里分别代表执行搜索和关闭窗口，但设置窗口也有自绘文本框；
///   先把这两个控制键归类，便于全局快捷键在焦点位于设置输入框时明确放行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum SearchDialogControlKey {
    /// 执行当前搜索条件。
    Submit,
    /// 关闭搜索对话框。
    Close,
}

/// 键盘滚动快捷键的目标区域。
///
/// 业务意图：
/// - 日志正文、搜索结果和左侧目录树都有独立滚动上下文，键盘滚动必须知道用户最近关注的是哪一块。
/// - 该枚举只记录主窗口内的只读滚动区域；搜索框、设置文本框等可编辑控件聚焦时会直接放行按键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum KeyboardScrollRegion {
    /// 右侧当前日志正文区域。
    LogContent,
    /// 右侧底部搜索结果面板。
    SearchResults,
    /// 左侧日志目录树。
    LogTree,
}

/// 键盘滚动命令。
///
/// 业务意图：
/// - 把平台按键字符串转换为有限命令后，滚动计算和 UI 事件处理都可以复用同一套逻辑。
/// - 本次只支持垂直滚动，不改变横向滚动、日志选区或目录树展开规则。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum KeyboardScrollCommand {
    /// 向上滚动一页。
    PageUp,
    /// 向下滚动一页。
    PageDown,
    /// 滚动到顶部，对应 `Ctrl+Home`。
    Top,
    /// 滚动到底部，对应 `Ctrl+End`。
    Bottom,
}

/// 当前会话内的搜索关键字历史项。
///
/// 业务意图：
/// - 普通搜索历史需要同时恢复查询词和匹配模式，避免用户从历史选择正则表达式后仍按普通文本执行。
/// - 历史只保存在内存中，不写入磁盘，降低日志关键字或敏感正则被持久化的风险。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct SearchQueryHistoryItem {
    /// 用户输入的查询词，已经去掉首尾空白。
    pub(in crate::app) query: String,
    /// 查询词对应的匹配模式。
    pub(in crate::app) match_mode: SearchMatchMode,
}

impl SearchQueryHistoryItem {
    /// 构造历史项；空白查询词不会生成历史。
    pub(in crate::app) fn new(query: &str, match_mode: SearchMatchMode) -> Option<Self> {
        let query = query.trim();
        (!query.is_empty()).then(|| Self {
            query: query.to_string(),
            match_mode,
        })
    }
}

/// 一次搜索任务启动时从 UI 表单快照得到的参数对象。
///
/// 业务意图：
/// - 普通搜索和快搜共享任务启动链路，使用参数对象可以把表单快照作为一个整体传递，避免搜索编排函数长期膨胀。
/// - 该结构只在主线程启动任务前使用，不持久化，也不改变搜索范围、大小写或匹配模式语义。
pub(in crate::app) struct SearchJobRequest {
    /// 结果面板和历史记录中展示的查询标题。
    pub(in crate::app) record_query: String,
    /// 后台搜索实际使用的匹配选项。
    pub(in crate::app) options: SearchOptions,
    /// 搜索范围。
    pub(in crate::app) scope: SearchScope,
    /// 是否大小写敏感。
    pub(in crate::app) case_sensitive: bool,
    /// 用户选择的匹配模式，用于历史记录和输入恢复。
    pub(in crate::app) match_mode: SearchMatchMode,
    /// 当前目录搜索时的目录输入框快照。
    pub(in crate::app) directory_target: String,
    /// 启动新任务前仍在运行的旧任务 ID。
    pub(in crate::app) previous_running_job_id: Option<usize>,
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
pub(in crate::app) struct SearchDialogState {
    /// 查询词输入框状态。
    ///
    /// 业务意图：
    /// - 普通搜索支持单行普通文本或正则表达式，输入中的换行会被忽略。
    /// - GPUI 输入协议对外使用 UTF-16 下标，通用单行状态负责保存内部 UTF-8 范围。
    pub(in crate::app) query_input: SingleLineTextInputState,
    /// 搜索关键字历史下拉菜单是否展开。
    ///
    /// 业务意图：
    /// - 历史记录本身保存在 `MainView`，展开状态属于当前搜索窗口交互，随搜索窗口关闭一起丢弃。
    ///
    /// 边界条件：
    /// - 当历史为空、用户选择历史项、编辑关键字或开始搜索时都应关闭，避免显示过期选项。
    pub(in crate::app) query_history_menu_open: bool,
    /// 搜索范围。
    pub(in crate::app) scope: SearchScope,
    /// 当前目录搜索的目标目录输入框状态。
    ///
    /// 业务意图：
    /// - 选择“当前目录”时，需要把实际搜索目标展示给用户，允许用户缩小到子目录或修改为加载树中的其它目录片段。
    /// - 该字段只用于过滤已经加载树中收集到的可搜索来源，不会扩大文件系统访问范围。
    pub(in crate::app) directory_input: SingleLineTextInputState,
    /// 是否区分大小写。
    pub(in crate::app) case_sensitive: bool,
    /// 当前普通搜索的匹配模式。
    ///
    /// 业务意图：
    /// - 普通搜索可以切换为正则；快搜不读取该字段，始终按普通文本 OR 执行。
    /// - 正则模式下 `case_sensitive` 仍保留但 UI 置灰，便于用户切回普通文本时恢复之前选择。
    pub(in crate::app) match_mode: SearchMatchMode,
    /// 当前关键字在当前激活文件中的出现次数。
    ///
    /// 业务意图：
    /// - 搜索窗口需要即时反馈关键字在当前文件中的命中次数，帮助用户决定是否继续执行完整搜索。
    /// - 该字段是缓存值，避免光标闪烁导致窗口频繁重绘时反复扫描大日志。
    ///
    /// 边界条件：
    /// - `None` 表示没有关键字、没有当前文件、当前文件仍在加载或打开失败；UI 应展示占位而不是误报 0。
    pub(in crate::app) current_file_match_count: Option<usize>,
    /// 当前是否有后台搜索任务仍在运行。
    pub(in crate::app) is_searching: bool,
    /// 当前搜索任务的进度快照。
    pub(in crate::app) progress: SearchProgress,
    /// 搜索状态提示。
    pub(in crate::app) message: String,
    /// 对话框绑定的任务 ID。
    ///
    /// 业务意图：
    /// - 用户快速连续搜索时，旧后台任务可能晚于新任务返回；任务 ID 用于丢弃过期更新。
    pub(in crate::app) job_id: usize,
}

/// 单次搜索历史记录。
///
/// 业务意图：
/// - 用户可能连续尝试多个关键字或范围，底部面板需要保留每一次搜索作为可展开记录，避免新搜索覆盖旧上下文。
/// - 每条记录独立保存进度、命中和错误，后台任务回调只更新对应 `job_id` 的记录。
#[derive(Clone)]
pub(in crate::app) struct SearchHistoryRecord {
    /// 后台任务 ID。
    pub(in crate::app) job_id: usize,
    /// 本次搜索使用的查询词。
    pub(in crate::app) query: String,
    /// 本次搜索范围。
    pub(in crate::app) scope: SearchScope,
    /// 当前目录搜索的目标目录；当前文件搜索为 `None`。
    pub(in crate::app) directory_target: Option<String>,
    /// 本次搜索是否区分大小写。
    pub(in crate::app) case_sensitive: bool,
    /// 本次搜索的匹配模式。
    ///
    /// 业务意图：
    /// - 结果面板摘要必须准确说明这条记录按普通文本还是正则执行，避免历史记录混淆。
    pub(in crate::app) match_mode: SearchMatchMode,
    /// 搜索进度终态或当前进度。
    pub(in crate::app) progress: SearchProgress,
    /// 搜索命中结果。
    pub(in crate::app) results: Vec<SearchResultItem>,
    /// 单文件读取或解码失败列表。
    pub(in crate::app) errors: Vec<SearchFileError>,
    /// 当前记录是否来自被用户取消的搜索任务。
    pub(in crate::app) canceled: bool,
    /// 记录是否展开显示明细。
    pub(in crate::app) expanded: bool,
    /// 当前记录中已经展开的文件分组来源键。
    ///
    /// 业务意图：
    /// - 历史记录只负责“这次搜索是否展开”，文件分组负责“某个文件内结果是否展开”。
    /// - 使用来源稳定键而不是文件名，避免同名文件或压缩包内同名成员互相影响展开状态。
    pub(in crate::app) expanded_file_keys: HashSet<String>,
}

impl SearchHistoryRecord {
    /// 返回当前记录的状态文案。
    ///
    /// 边界条件：
    /// - 取消状态优先于进度判断，否则关闭对话框后旧任务回调被丢弃，记录会长期误显示为“搜索中”。
    pub(in crate::app) fn state_label(&self) -> &'static str {
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
pub(in crate::app) enum SearchResultsPanelRow {
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
pub(in crate::app) struct SearchResultFileGroup {
    /// 文件来源稳定键。
    pub(in crate::app) source_key: String,
    /// 文件完整展示路径。
    ///
    /// 业务意图：
    /// - 结果面板按文件分组时，用户需要直接看到完整路径判断命中来源，避免文件名相同但目录不同造成误判。
    pub(in crate::app) full_path: String,
    /// 当前文件内的命中结果下标。
    pub(in crate::app) result_indices: Vec<usize>,
}

/// 搜索结果面板状态。
///
/// 业务意图：
/// - 面板从右侧内容区底部弹出，保存搜索历史记录和高度。
/// - 结果列表使用虚拟列表滚动句柄，避免大量命中时滚动卡顿。
#[derive(Clone)]
pub(in crate::app) struct SearchResultsPanelState {
    /// 搜索历史记录，按发起时间从旧到新排列。
    pub(in crate::app) records: Vec<SearchHistoryRecord>,
    /// 已展平的虚拟列表行缓存。
    ///
    /// 业务意图：
    /// - 搜索结果可能包含成千上万条命中，滚动时不能每一帧都重新展平全部历史记录和文件分组。
    /// - 该缓存只在记录增删、命中追加或展开状态改变时重建，渲染时按可见 range 直接取片段。
    pub(in crate::app) rows: Vec<SearchResultsPanelRow>,
    /// 面板当前高度。
    pub(in crate::app) height: f32,
    /// 结果虚拟列表滚动句柄。
    pub(in crate::app) scroll_handle: UniformListScrollHandle,
}

/// 搜索结果面板右键菜单状态。
///
/// 业务意图：
/// - 结果面板可能包含多次搜索和大量文件分组，用户需要批量展开或收起以快速调整信息密度。
/// - GPUI 0.2.2 没有适合该场景的现成上下文菜单，因此保存右键位置并自绘菜单。
pub(in crate::app) struct SearchResultsContextMenu {
    /// 菜单左上角相对右侧日志工作区的横坐标。
    pub(in crate::app) x: f32,
    /// 菜单左上角相对右侧日志工作区的纵坐标。
    pub(in crate::app) y: f32,
}

/// 搜索结果面板右键菜单命令。
#[derive(Clone, Copy)]
pub(in crate::app) enum SearchResultsContextMenuAction {
    /// 展开所有搜索历史记录和文件分组。
    ExpandAll,
    /// 收起所有搜索历史记录和文件分组。
    CollapseAll,
}

/// 业务意图：
/// - `Ready` 保存当前日志文档指针，方便 UI 渲染和后台回调按同一状态枚举合并，同时控制枚举大小。
pub(in crate::app) enum LogTabState {
    /// 正在读取或重新解码。
    Loading {
        /// 展示给用户的中文状态。
        message: String,
    },
    /// 已成功解码。
    Ready {
        /// 可供右侧虚拟列表渲染的日志文档。
        ///
        /// 业务意图：
        /// - 小文件保存完整解码文档，超大文件保存分页文档；UI 渲染层通过统一枚举读取行数和可见文本。
        document: Box<LogTabDocument>,
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
/// - 后台读取结果只在线程完成时短暂传回 UI，保持直接携带文档可以减少合并阶段的状态拆解。
pub(in crate::app) enum LogTabLoadResult {
    /// 读取和自动解码都成功。
    Ready {
        /// 原始字节，用于后续手动切换编码。
        raw_bytes: Option<Arc<Vec<u8>>>,
        /// 自动解码后的文档。
        document: Box<LogTabDocument>,
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
/// - 手动切换编码的结果同样只作为后台任务返回值，暂不引入装箱以免扩大解码合并路径改动。
pub(in crate::app) enum LogTabDecodeResult {
    /// 解码成功。
    Ready {
        /// 触发该后台任务时用户选择的编码策略。
        ///
        /// 业务意图：
        /// - 用户快速连续切换编码时，较早的后台任务可能晚返回；合并阶段需要用该字段识别并丢弃过期结果。
        encoding_choice: EncodingChoice,
        /// 新编码下的日志文档。
        document: Box<LogTabDocument>,
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
pub(in crate::app) struct TabContextMenu {
    /// 菜单针对的 tab ID。
    pub(in crate::app) tab_id: usize,
    /// 菜单左上角的窗口内容区横坐标。
    pub(in crate::app) x: f32,
    /// 菜单左上角的窗口内容区纵坐标。
    pub(in crate::app) y: f32,
}

/// 加载日志来源菜单状态。
///
/// 业务意图：
/// - Windows 不支持文件和目录混选时，工具栏按钮先展示自绘菜单，让用户明确进入文件/压缩包选择器或目录选择器。
/// - 菜单坐标使用窗口内容区坐标，作为主窗口根节点的绝对定位元素渲染，避免受左右分栏是否显示影响。
pub(in crate::app) struct LoadSourceMenu {
    /// 菜单左上角的窗口内容区横坐标。
    pub(in crate::app) x: f32,
    /// 菜单左上角的窗口内容区纵坐标。
    pub(in crate::app) y: f32,
}

/// 编码选择下拉框状态。
///
/// 业务意图：
/// - 用户要求编码选择不要平铺在顶部，因此这里把当前打开的编码菜单作为独立弹层状态保存。
/// - 菜单只绑定一个 tab，切换 tab、关闭 tab 或重新加载日志时都会收起，避免对旧 tab 继续操作。
pub(in crate::app) struct EncodingDropdownMenu {
    /// 下拉框所属的 tab ID。
    pub(in crate::app) tab_id: usize,
    /// 菜单左上角相对右侧日志工作区的横坐标。
    ///
    /// 业务意图：
    /// - 编码选择器已经移动到右侧状态栏，按钮位置会随状态文本宽度变化，不能继续使用固定左边距。
    pub(in crate::app) x: f32,
    /// 菜单左上角相对右侧日志工作区的纵坐标。
    pub(in crate::app) y: f32,
}

/// tab 右键菜单命令。
///
/// 业务意图：
/// - 菜单项直接对应用户要求的三个关闭操作，避免 UI 文案和处理逻辑分散。
#[derive(Clone, Copy)]
pub(in crate::app) enum TabContextMenuAction {
    /// 关闭被右键点击的 tab。
    Current,
    /// 关闭除被右键点击 tab 之外的所有 tab。
    OtherTabs,
    /// 关闭所有 tab。
    AllTabs,
}

/// 日志正文右键菜单状态。
///
/// 业务意图：
/// - 日志正文使用自绘虚拟列表，不能依赖系统文本控件菜单；这里保存菜单所需的 tab 和右侧面板局部坐标。
/// - 菜单只作用于当前右键所在 tab，避免用户切换 tab 后复制或保存到错误文件。
pub(in crate::app) struct LogViewerContextMenu {
    /// 右键打开菜单时对应的日志 tab。
    pub(in crate::app) tab_id: usize,
    /// 菜单左上角相对右侧日志工作区的横坐标。
    pub(in crate::app) x: f32,
    /// 菜单左上角相对右侧日志工作区的纵坐标。
    pub(in crate::app) y: f32,
}

/// 日志正文右键菜单命令。
///
/// 业务意图：
/// - 复制依赖当前正文选区；另存为依赖当前 tab 的原始日志来源，集中枚举可以保持渲染文案和执行逻辑一致。
#[derive(Clone, Copy)]
pub(in crate::app) enum LogViewerContextMenuAction {
    /// 复制当前正文选区到剪贴板。
    Copy,
    /// 将当前 tab 的日志来源保存到用户选择的目录。
    SaveAs,
}

/// 左侧目录树单行渲染所需的输入数据。
///
/// 业务意图：
/// - 目录树行需要同时表达层级、图标、元信息和点击行为，字段较多；集中成结构体可以保持渲染函数签名稳定。
/// - 该结构只服务 UI 渲染，不回写加载层，也不作为文件来源持久化模型。
pub(in crate::app) struct LogTreeRowRenderData {
    /// 当前加载结果内部的节点 ID，用于展开状态和元素稳定标识。
    pub(in crate::app) node_id: usize,
    /// 当前节点在树中的深度，用于计算左侧缩进。
    pub(in crate::app) depth: usize,
    /// 展开箭头图标；没有子节点时为 `None`，仍会保留占位宽度。
    pub(in crate::app) expand_icon: Option<Icon>,
    /// 节点类型图标。
    pub(in crate::app) item_icon: Icon,
    /// 节点类型图标颜色。
    pub(in crate::app) icon_color: u32,
    /// 当前行是否可以展开或收起。
    pub(in crate::app) can_toggle: bool,
    /// 当前行是否绑定日志正文来源；存在时点击打开右侧 tab。
    pub(in crate::app) source: Option<LogFileSource>,
    /// 当前行在可见目录树中的下标。
    ///
    /// 业务意图：
    /// - Shift 多选需要按当前可见顺序选择连续行，折叠隐藏的节点不应被纳入本次范围。
    pub(in crate::app) visible_index: usize,
    /// 当前行是否处于左侧树选择集合中。
    ///
    /// 业务意图：
    /// - 单击、多选和右键菜单都依赖可见选中态，渲染层需要用该字段决定背景和文字强调。
    pub(in crate::app) selected: bool,
    /// 当前行展示文本。
    pub(in crate::app) label: String,
    /// 当前行右侧短元信息。
    pub(in crate::app) meta: Option<String>,
}

/// 左侧目录树普通点击触发的主动作。
///
/// 业务意图：
/// - 目录树需要同时支持“单击打开/展开”和 Shift、Ctrl/Command 多选；集中描述主动作可以避免点击处理里散落判断。
/// - 该动作不携带真实文件来源，文件来源仍由调用点从加载层传入，避免测试辅助类型复制路径模型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum LogTreePrimaryClickAction {
    /// 不执行打开或展开，只保留选择变化。
    None,
    /// 打开当前日志文件到右侧 tab。
    OpenSource,
    /// 展开或收起当前目录/压缩包节点。
    ToggleNode,
}

/// 左侧目录树右键菜单状态。
///
/// 业务意图：
/// - 右键菜单作用于左侧当前选择文件集合；如果用户右键未选中文件，先把右键行作为唯一选择。
/// - 菜单位置保存为左侧面板局部坐标，避免主窗口顶部工具栏或右侧区域偏移影响定位。
pub(in crate::app) struct LogTreeContextMenu {
    /// 右键落点对应的节点 ID。
    pub(in crate::app) node_id: usize,
    /// 右键落点对应的可读取文件来源。
    ///
    /// 业务意图：
    /// - 如果当前多选集合里没有可读取文件，右键菜单命令仍应能作用于右键点中的文件。
    /// - 单文件压缩包会在渲染阶段被映射成内部唯一文件来源，因此这里保存的是最终可打开/可分析来源。
    pub(in crate::app) source: Option<LogFileSource>,
    /// 菜单左上角相对左侧目录树面板的横坐标。
    pub(in crate::app) x: f32,
    /// 菜单左上角相对左侧目录树面板的纵坐标。
    pub(in crate::app) y: f32,
}

/// 打开左侧目录树右键菜单所需的鼠标和节点快照。
///
/// 业务意图：
/// - 右键事件发生后，行节点、可见下标、来源和窗口坐标必须作为同一个快照处理，避免函数签名持续膨胀。
pub(in crate::app) struct LogTreeContextMenuRequest {
    /// 右键落点对应的节点 ID。
    pub(in crate::app) node_id: usize,
    /// 右键落点在当前可见目录树中的下标。
    pub(in crate::app) visible_index: usize,
    /// 右键落点对应的可读取来源。
    pub(in crate::app) source: Option<LogFileSource>,
    /// 鼠标窗口横坐标。
    pub(in crate::app) window_x: f32,
    /// 鼠标窗口纵坐标。
    pub(in crate::app) window_y: f32,
}

/// 左侧目录树右键菜单命令。
///
/// 业务意图：
/// - 文件另存为和线程日志分析都基于当前多选文件集合，集中枚举可以让渲染和执行逻辑保持一致。
#[derive(Clone, Copy)]
pub(in crate::app) enum LogTreeContextMenuAction {
    /// 把当前多选日志文件保存到用户指定目录。
    SaveAs,
    /// 对当前多选日志文件执行 Java thread dump 时间线分析。
    AnalyzeThreads,
}

/// 渲染左侧目录树右键菜单项的参数对象。
///
/// 业务意图：
/// - 菜单项需要携带节点快照、动作、展示文案和主题；集中为参数对象后，新增菜单项时不会继续拉长渲染 helper。
pub(in crate::app) struct LogTreeContextMenuItemRequest {
    /// 菜单归属节点 ID。
    pub(in crate::app) node_id: usize,
    /// 当多选集合没有可读取文件时的右键来源兜底。
    pub(in crate::app) fallback_source: Option<LogFileSource>,
    /// 点击菜单项时执行的命令。
    pub(in crate::app) action: LogTreeContextMenuAction,
    /// 菜单展示文案。
    pub(in crate::app) label: &'static str,
    /// 菜单图标。
    pub(in crate::app) icon: Icon,
    /// 当前主题色板。
    pub(in crate::app) palette: AppThemePalette,
}

/// 批量另存为的后台结果。
///
/// 业务意图：
/// - 文件复制发生在后台线程，结果回到 UI 后只需要知道成功和失败数量，用于开发期诊断或后续状态栏展示。
pub(in crate::app) struct SaveSelectedLogsResult {
    /// 成功写入的文件数量。
    pub(in crate::app) saved_count: usize,
    /// 因用户选择“跳过”而未覆盖的同名文件数量。
    pub(in crate::app) skipped_count: usize,
    /// 失败的文件数量。
    pub(in crate::app) failed_count: usize,
}

/// 另存为遇到同名目标文件时的处理策略。
///
/// 业务意图：
/// - 用户要求目标目录已有同名文件时必须弹窗确认，并提供“跳过”和“覆盖”两种明确选择。
/// - 策略作为纯数据传入后台保存函数，避免 UI 弹窗逻辑和文件复制逻辑耦合。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum SaveConflictPolicy {
    /// 跳过已存在的目标文件。
    SkipExisting,
    /// 覆盖已存在的目标文件。
    OverwriteExisting,
}

/// 另存为同名文件确认弹窗状态。
///
/// 业务意图：
/// - 批量另存为可能一次命中多个同名文件；主窗口只弹一次确认，用户选择后对本次保存任务统一应用。
///
/// 边界条件：
/// - 这里只保存必要展示信息和待保存来源，不保存文件句柄；确认后仍按原管线重新物化和复制来源。
pub(in crate::app) struct SaveOverwriteConfirmDialog {
    /// 待保存的日志来源。
    pub(in crate::app) sources: Vec<LogFileSource>,
    /// 用户选择的目标目录。
    pub(in crate::app) target_directory: PathBuf,
    /// 已存在目标路径数量。
    pub(in crate::app) conflict_count: usize,
    /// 首个冲突目标路径，用于弹窗展示具体示例，避免用户无法判断风险。
    pub(in crate::app) first_conflict_path: PathBuf,
}

/// 用户通过“加载日志”按钮选择的路径来源类型。
///
/// 业务意图：
/// - “加载日志”按钮现在直接打开系统选择器，减少点击后无反馈的歧义。
/// - 枚举保留路径选择配置入口，后续如需区分平台或补充 Windows 专用目录选择策略时不影响调用方。
#[derive(Clone, Copy)]
pub(in crate::app) enum LoadPromptKind {
    /// 选择普通文件、目录或压缩包来源。
    Sources,
    /// 选择普通文件或压缩包来源。
    FilesOrArchives,
    /// 选择目录来源。
    Directories,
}

impl LoadPromptKind {
    /// 为 GPUI 系统路径选择器生成配置。
    ///
    /// 业务意图：
    /// - `files` 和 `directories` 同时开启，让 macOS 能像常见桌面工具一样在一次对话框里选择文件或目录。
    /// - Windows 和部分 Linux 后端不支持文件、目录混选，此时优先显示文件，保证 ZIP/RAR/7Z/TAR.GZ/GZ 等压缩包可直接选择。
    /// - `multiple` 保持为 `true`，允许用户一次加载多个文件、目录或压缩包来源。
    ///
    /// 边界条件：
    /// - GPUI 0.2.2 的 `PathPromptOptions` 不提供扩展名过滤字段，因此压缩包类型由加载层识别。
    /// - 不支持混选的平台如果仍传 `directories=true`，Windows 会进入只选目录模式，导致用户看不到压缩包文件。
    pub(in crate::app) fn to_prompt_options(
        self,
        can_select_mixed_files_and_dirs: bool,
    ) -> PathPromptOptions {
        match self {
            Self::Sources => PathPromptOptions {
                files: true,
                directories: can_select_mixed_files_and_dirs,
                multiple: true,
                prompt: Some("选择日志来源".into()),
            },
            Self::FilesOrArchives => PathPromptOptions {
                files: true,
                directories: false,
                multiple: true,
                prompt: Some("选择日志文件或压缩包".into()),
            },
            Self::Directories => PathPromptOptions {
                files: false,
                directories: true,
                multiple: true,
                prompt: Some("选择日志目录".into()),
            },
        }
    }

    /// 返回进入加载状态时展示的中文说明。
    ///
    /// 边界条件：
    /// - 当前文案只用于左侧目录树区域，不作为错误判断依据。
    pub(in crate::app) fn loading_message(self) -> &'static str {
        match self {
            Self::Sources => "正在扫描已选择的日志来源...",
            Self::FilesOrArchives => "正在扫描已选择的日志文件或压缩包...",
            Self::Directories => "正在扫描已选择的日志目录...",
        }
    }
}
