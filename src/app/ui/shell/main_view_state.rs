// 主窗口按 UI 工作区拆分后的纯状态容器。
//
// 业务意图：
// - 该文件只保存 `MainView` 内部状态分组，不改变 GPUI 根实体、渲染方法或事件处理入口。
// - 第一阶段先把导航、日志工作区、搜索工作区、设置和模型配置从巨型 `MainView` 字段列表中分离，
//   后续再逐步把对应方法下沉到各自视图或动作适配模块。
//
// 边界条件：
// - 所有字段仍限制在 `app` 模块内部访问，避免把当前 UI 状态固化成公开 API。
// - 构造函数只做原有默认值搬迁，不读取新配置、不改变任务 ID 初始值、不改变滚动句柄生命周期。

use super::*;

/// 主窗口左侧导航和全局滚动目标状态。
///
/// 业务意图：
/// - 导航状态独立于日志、搜索和设置，后续切换大功能页时只需要修改这个小状态对象。
/// - 键盘滚动目标记录用户最近操作区域，不能混入具体列表滚动句柄，否则不同工作区之间会产生隐式依赖。
pub(in crate::app) struct NavigationState {
    /// 当前主窗口选中的大功能页。
    pub(in crate::app) active_main_feature: MainFeature,
    /// 当前鼠标悬浮的左侧导航入口。
    pub(in crate::app) hovered_navigation_item: Option<MainNavigationItem>,
    /// 最近一次鼠标所在或点击的键盘滚动区域。
    pub(in crate::app) keyboard_scroll_region: Option<KeyboardScrollRegion>,
}

impl NavigationState {
    /// 创建主导航初始状态。
    ///
    /// 边界条件：
    /// - 默认页必须继续是日志分析页，保证启动首屏行为不变。
    pub(in crate::app) fn new() -> Self {
        Self {
            active_main_feature: MainFeature::default(),
            hovered_navigation_item: None,
            keyboard_scroll_region: None,
        }
    }
}

/// 日志加载、目录树、tab、编码菜单和日志正文相关状态。
///
/// 业务意图：
/// - 该状态覆盖日志查看主流程中依赖当前加载来源树的数据，重新加载日志时可以集中清理。
/// - 窗口尺寸、主题、设置窗口等会话级状态不放入这里，避免“加载日志”误重置用户偏好。
pub(in crate::app) struct LogWorkspaceState {
    /// 左侧日志目录树当前的数据状态。
    pub(in crate::app) load_state: LogTreeLoadState,
    /// 左侧目录树虚拟列表的滚动句柄。
    pub(in crate::app) log_tree_scroll_handle: UniformListScrollHandle,
    /// 左侧目录树当前选中的节点 ID 集合。
    pub(in crate::app) log_tree_selected_node_ids: HashSet<usize>,
    /// 左侧目录树文件名搜索状态。
    ///
    /// 业务意图：
    /// - 搜索只服务当前已加载目录树，用于按文件名快速过滤并定位可打开日志文件。
    /// - 状态放在日志工作区内，重新加载日志时可以和选择、右键菜单、滚动句柄一起重置，避免旧关键字作用到新树。
    pub(in crate::app) log_tree_search: LogTreeSearchState,
    /// Shift 多选的锚点节点 ID。
    pub(in crate::app) log_tree_selection_anchor: Option<usize>,
    /// 当前打开的左侧目录树右键菜单。
    pub(in crate::app) log_tree_context_menu: Option<LogTreeContextMenu>,
    /// 右侧 tab 栏横向滚动句柄。
    pub(in crate::app) tab_bar_scroll_handle: ScrollHandle,
    /// 右侧已经打开的日志 tab 列表。
    pub(in crate::app) open_tabs: Vec<OpenLogTab>,
    /// 当前激活的日志 tab ID。
    pub(in crate::app) active_tab_id: Option<usize>,
    /// 下一个待分配的 tab ID。
    pub(in crate::app) next_tab_id: usize,
    /// 当前打开的 tab 右键菜单。
    pub(in crate::app) tab_context_menu: Option<TabContextMenu>,
    /// 当前打开的加载日志来源菜单。
    pub(in crate::app) load_source_menu: Option<LoadSourceMenu>,
    /// 当前打开的编码下拉框。
    pub(in crate::app) encoding_dropdown_menu: Option<EncodingDropdownMenu>,
    /// 当前打开的日志正文右键菜单。
    pub(in crate::app) log_viewer_context_menu: Option<LogViewerContextMenu>,
    /// 另存为同名文件覆盖确认弹窗。
    pub(in crate::app) save_overwrite_confirm_dialog: Option<SaveOverwriteConfirmDialog>,
    /// 日志正文自绘滚动条的拖动状态。
    pub(in crate::app) log_scrollbar_drag: Option<LogScrollbarDrag>,
    /// 左侧目录树自绘滚动条的拖动状态。
    pub(in crate::app) log_tree_scrollbar_drag: Option<LogTreeScrollbarDrag>,
}

impl LogWorkspaceState {
    /// 创建日志工作区初始状态。
    ///
    /// 边界条件：
    /// - `next_tab_id` 从 1 开始，继续保持现有后台任务定位和测试语义。
    /// - 各滚动句柄必须在构造时创建，不能在渲染路径临时创建，否则滚动位置会丢失。
    pub(in crate::app) fn new(context: &mut Context<MainView>) -> Self {
        Self {
            load_state: LogTreeLoadState::Empty,
            log_tree_scroll_handle: UniformListScrollHandle::new(),
            log_tree_selected_node_ids: HashSet::new(),
            log_tree_search: LogTreeSearchState::new(context),
            log_tree_selection_anchor: None,
            log_tree_context_menu: None,
            tab_bar_scroll_handle: ScrollHandle::new(),
            open_tabs: Vec::new(),
            active_tab_id: None,
            next_tab_id: 1,
            tab_context_menu: None,
            load_source_menu: None,
            encoding_dropdown_menu: None,
            log_viewer_context_menu: None,
            save_overwrite_confirm_dialog: None,
            log_scrollbar_drag: None,
            log_tree_scrollbar_drag: None,
        }
    }
}

/// 搜索窗口、搜索历史和底部结果面板状态。
///
/// 业务意图：
/// - 搜索输入窗口已经是独立窗口，但搜索任务和结果面板仍由主窗口协调，因此先集中到一个工作区状态。
/// - 后台任务 ID、窗口句柄和滚动句柄放在同一对象内，便于后续搜索功能继续下沉而不穿透整个 `MainView`。
pub(in crate::app) struct SearchWorkspaceState {
    /// 当前搜索对话框状态。
    pub(in crate::app) search_dialog: Option<SearchDialogState>,
    /// 当前会话内的搜索关键字历史。
    pub(in crate::app) search_query_history: Vec<SearchQueryHistoryItem>,
    /// 搜索对话框独立窗口句柄。
    pub(in crate::app) search_dialog_window: Option<WindowHandle<SearchDialogWindowView>>,
    /// 搜索对话框打开请求是否已经排队到下一帧。
    pub(in crate::app) search_dialog_open_pending: bool,
    /// 搜索对话框打开时需要应用的范围预设。
    ///
    /// 业务意图：
    /// - 搜索窗口创建被延迟到下一帧；左侧树“选中搜索”必须把当时的文件快照暂存在这里，避免延迟期间用户改变选择后影响搜索范围。
    pub(in crate::app) search_dialog_open_preset: SearchDialogOpenPreset,
    /// 搜索结果底部面板状态。
    pub(in crate::app) search_results_panel: Option<SearchResultsPanelState>,
    /// 搜索结果面板高度拖动状态。
    pub(in crate::app) search_results_resize_drag: Option<SearchResultsResizeDrag>,
    /// 搜索结果面板滚动条拖动状态。
    pub(in crate::app) search_results_scrollbar_drag: Option<SearchResultsScrollbarDrag>,
    /// 当前打开的搜索结果右键菜单。
    pub(in crate::app) search_results_context_menu: Option<SearchResultsContextMenu>,
    /// 下一个搜索任务 ID。
    pub(in crate::app) next_search_job_id: usize,
    /// 当前文件轻量定位请求 ID。
    ///
    /// 业务意图：
    /// - 搜索输入框会在用户输入后自动定位当前文件第一处命中；分页大日志定位在后台线程执行，
    ///   需要用请求 ID 丢弃旧输入产生的过期回调，避免把界面滚回旧关键字。
    pub(in crate::app) current_file_navigation_request_id: usize,
    /// 搜索输入框焦点句柄。
    pub(in crate::app) search_input_focus: gpui::FocusHandle,
    /// 当前目录搜索目标输入框焦点句柄。
    pub(in crate::app) search_directory_focus: gpui::FocusHandle,
    /// 搜索关键字输入框最近一次排版结果。
    pub(in crate::app) search_query_last_layout: Option<ShapedLine>,
    /// 搜索关键字输入框最近一次绘制边界。
    pub(in crate::app) search_query_last_bounds: Option<Bounds<Pixels>>,
    /// 目录目标输入框最近一次排版结果。
    pub(in crate::app) search_directory_last_layout: Option<ShapedLine>,
    /// 目录目标输入框最近一次绘制边界。
    pub(in crate::app) search_directory_last_bounds: Option<Bounds<Pixels>>,
    /// 当前正在拖拽选择的搜索文本输入槽位。
    pub(in crate::app) search_text_selection_drag: Option<(SearchTextInputKind, usize)>,
    /// 搜索输入框最近一次光标活动时间。
    pub(in crate::app) search_text_cursor_last_activity: Instant,
}

impl SearchWorkspaceState {
    /// 创建搜索工作区初始状态。
    ///
    /// 边界条件：
    /// - 焦点句柄必须从当前 `Context<MainView>` 创建，保证独立搜索窗口仍能把 IME 输入转发回主视图。
    pub(in crate::app) fn new(context: &mut Context<MainView>) -> Self {
        Self {
            search_dialog: None,
            search_query_history: Vec::new(),
            search_dialog_window: None,
            search_dialog_open_pending: false,
            search_dialog_open_preset: SearchDialogOpenPreset::Default,
            search_results_panel: None,
            search_results_resize_drag: None,
            search_results_scrollbar_drag: None,
            search_results_context_menu: None,
            next_search_job_id: 1,
            current_file_navigation_request_id: 0,
            search_input_focus: context.focus_handle(),
            search_directory_focus: context.focus_handle(),
            search_query_last_layout: None,
            search_query_last_bounds: None,
            search_directory_last_layout: None,
            search_directory_last_bounds: None,
            search_text_selection_drag: None,
            search_text_cursor_last_activity: Instant::now(),
        }
    }
}

/// 设置窗口、主题、字号、快搜和线程过滤配置状态。
///
/// 业务意图：
/// - 设置页中的可编辑文本和显示偏好需要跨多个设置窗口渲染函数共享，但不应散落在主视图顶层。
/// - 模型配置字段较多且有独立测试请求状态，因此继续放入 `ModelConfigState`。
pub(in crate::app) struct SettingsState {
    /// 设置窗口独立窗口句柄。
    pub(in crate::app) settings_window: Option<WindowHandle<SettingsWindowView>>,
    /// 设置窗口打开请求是否已经排队到下一帧。
    pub(in crate::app) settings_window_open_pending: bool,
    /// 设置窗口当前激活页签。
    pub(in crate::app) settings_active_tab: SettingsTab,
    /// 当前主题偏好。
    pub(in crate::app) theme_preference: ThemePreference,
    /// 日志正文显示字号。
    pub(in crate::app) log_viewer_font_size: f32,
    /// 系统右键菜单集成状态。
    ///
    /// 业务意图：
    /// - 注册和卸载右键菜单是跨会话平台副作用，设置页需要明确展示当前状态和执行中反馈。
    /// - 状态只存在于当前 UI 会话，不写入配置文件；真实来源始终是平台注册表或 LaunchServices 查询结果。
    pub(in crate::app) shell_integration_state: ShellIntegrationUiState,
    /// 线程日志分析过滤配置原文。
    pub(in crate::app) thread_analysis_filter_text: String,
    /// 线程日志分析过滤输入区是否处于编辑状态。
    pub(in crate::app) thread_analysis_filter_is_editing: bool,
    /// 进入编辑前的线程日志分析过滤配置快照。
    pub(in crate::app) thread_analysis_filter_saved_text_before_edit: Option<String>,
    /// 快搜关键字单行输入框状态。
    pub(in crate::app) quick_search_keywords_input: SingleLineTextInputState,
    /// 快搜关键字输入区是否处于编辑状态。
    pub(in crate::app) quick_search_keywords_is_editing: bool,
    /// 进入编辑前的快搜关键字配置快照。
    pub(in crate::app) quick_search_keywords_saved_text_before_edit: Option<String>,
    /// 快搜关键字输入区焦点句柄。
    pub(in crate::app) quick_search_keywords_focus: gpui::FocusHandle,
    /// 快搜关键字输入区最近一次单行排版结果。
    pub(in crate::app) quick_search_keywords_last_layout: Option<ShapedLine>,
    /// 快搜关键字输入区最近一次绘制边界。
    pub(in crate::app) quick_search_keywords_last_bounds: Option<Bounds<Pixels>>,
    /// 快搜关键字输入区拖拽选择锚点。
    pub(in crate::app) quick_search_keywords_selection_drag: Option<usize>,
    /// 线程日志分析过滤输入区的选择范围。
    pub(in crate::app) thread_analysis_filter_selection_range: Range<usize>,
    /// 线程日志分析过滤输入区的输入法组合文本范围。
    pub(in crate::app) thread_analysis_filter_marked_range: Option<Range<usize>>,
    /// 线程日志分析过滤输入区焦点句柄。
    pub(in crate::app) thread_analysis_filter_focus: gpui::FocusHandle,
    /// 线程日志分析过滤输入区最近一次绘制的逐行布局。
    pub(in crate::app) thread_analysis_filter_last_layouts: Vec<ThreadAnalysisFilterLineLayout>,
    /// 线程日志分析过滤输入区最近一次整体绘制边界。
    pub(in crate::app) thread_analysis_filter_last_bounds: Option<Bounds<Pixels>>,
    /// 线程日志分析过滤输入区拖拽选择锚点。
    pub(in crate::app) thread_analysis_filter_selection_drag: Option<usize>,
    /// 存储页扫描和操作状态。
    ///
    /// 业务意图：
    /// - 存储页需要后台统计应用配置目录、数据库、插件目录和临时缓存大小；扫描结果保存在设置状态中，避免渲染阶段同步访问磁盘。
    /// - 该状态只存在于当前设置会话，不写入配置；真实来源始终是文件系统当前状态。
    pub(in crate::app) storage: StorageSettingsState,
}

/// 存储位置类型。
///
/// 业务意图：
/// - 存储页需要用稳定分类区分 SQLite 数据库、普通目录和临时缓存，便于渲染不同说明和安全操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::app) enum StorageLocationKind {
    /// SQLite 数据库文件。
    Database,
    /// 应用配置目录或插件安装目录。
    Directory,
    /// 系统临时目录中的大日志物化缓存。
    Cache,
}

impl StorageLocationKind {
    /// 返回存储类型中文标签。
    pub(in crate::app) fn label(self) -> &'static str {
        match self {
            Self::Database => "数据库",
            Self::Directory => "目录",
            Self::Cache => "临时缓存",
        }
    }
}

/// 单个存储位置的扫描状态。
///
/// 业务意图：
/// - UI 渲染只消费该快照，不在渲染阶段读取文件系统，避免目录过大或权限异常时卡住设置窗口。
/// - 路径不可用、未创建和读取失败都需要区分展示，方便用户判断是正常尚未使用还是存在权限问题。
#[derive(Clone, Debug)]
pub(in crate::app) struct StorageLocationStatus {
    /// 稳定条目 ID，用于按钮事件回查当前扫描结果。
    pub(in crate::app) id: &'static str,
    /// 展示名称。
    pub(in crate::app) name: &'static str,
    /// 所属分组。
    pub(in crate::app) group: &'static str,
    /// 存储用途说明。
    pub(in crate::app) purpose: &'static str,
    /// 存储类型。
    pub(in crate::app) kind: StorageLocationKind,
    /// 当前平台解析出的路径；为空表示该平台没有对应存储位置。
    pub(in crate::app) path: Option<PathBuf>,
    /// 路径是否已经存在。
    pub(in crate::app) exists: bool,
    /// 文件大小或目录递归统计大小；为空表示未创建或无法统计。
    pub(in crate::app) size_bytes: Option<u64>,
    /// 最近修改时间；为空表示未创建或系统无法提供。
    pub(in crate::app) modified: Option<SystemTime>,
    /// 当前条目的读取错误；不会影响其它条目展示。
    pub(in crate::app) error: Option<String>,
}

impl StorageLocationStatus {
    /// 返回当前条目的状态标签。
    pub(in crate::app) fn state_label(&self) -> &'static str {
        if self.path.is_none() {
            "不可用"
        } else if self.error.is_some() {
            "读取失败"
        } else if self.exists {
            "已创建"
        } else {
            "未创建"
        }
    }
}

/// 设置页存储管理状态。
///
/// 业务意图：
/// - 存储扫描涉及文件系统递归统计，必须由后台任务刷新；UI 用这里的快照展示最近一次结果。
/// - 安全管理操作只更新状态提示和重新扫描结果，不直接删除配置、数据库或插件目录。
#[derive(Clone, Debug)]
pub(in crate::app) struct StorageSettingsState {
    /// 是否正在后台扫描。
    pub(in crate::app) loading: bool,
    /// 最近一次刷新完成时间。
    pub(in crate::app) last_refreshed_at: Option<SystemTime>,
    /// 最近一次扫描得到的存储条目。
    pub(in crate::app) locations: Vec<StorageLocationStatus>,
    /// 存储页操作提示。
    pub(in crate::app) status_message: Option<String>,
}

impl StorageSettingsState {
    /// 创建空的存储页状态。
    pub(in crate::app) fn new() -> Self {
        Self {
            loading: false,
            last_refreshed_at: None,
            locations: Vec::new(),
            status_message: None,
        }
    }
}

/// 插件管理和声明式插件窗口状态。
///
/// 业务意图：
/// - 插件框架允许第三方通过 JSON manifest 贡献导航页和右键菜单；主窗口需要持有当前会话内的加载快照，
///   让设置页、日志树和笔记树在同一份数据上渲染。
/// - 注册表仍以 JSON 写入应用配置目录；该状态只缓存读取结果和最近一次操作提示，不把插件运行结果写回配置。
///
/// 边界条件：
/// - 插件启用状态由注册表持久化，加载错误只在当前启动后重新计算，避免旧错误在修复插件目录后继续误导用户。
/// - 插件进程由命令触发时短暂启动，当前第一版不保持常驻进程，因此这里不保存子进程句柄。
pub(in crate::app) struct PluginWorkspaceState {
    /// 插件注册表 JSON 的内存副本。
    ///
    /// 业务意图：
    /// - 设置页启用、禁用、加载和卸载操作先更新该副本，再写回磁盘，最后重建 `definitions`。
    /// - 如果应用配置目录不可用，写入函数会返回中文错误并展示在设置页。
    pub(in crate::app) registry: PluginRegistry,
    /// 当前可渲染插件定义列表，包含开发目录插件、ZIP 安装插件和加载失败占位项。
    pub(in crate::app) definitions: Vec<PluginDefinition>,
    /// 最近一次插件管理或运行操作的用户可见提示。
    ///
    /// 边界条件：
    /// - `None` 表示当前没有需要展示的提示；错误和成功提示都用短中文文本保存，避免设置页继续理解底层错误类型。
    pub(in crate::app) status_message: Option<String>,
    /// 插件声明式页面独立窗口句柄。
    ///
    /// 业务意图：
    /// - 插件 v1 只允许返回宿主可控的声明式页面；窗口句柄由主视图统一持有，重复打开时复用并替换页面内容。
    pub(in crate::app) page_window: Option<WindowHandle<PluginPageWindowView>>,
    /// 当前正在执行的插件命令代次。
    ///
    /// 业务意图：
    /// - 插件命令在后台进程中运行，用户可能连续触发不同插件；代次用于丢弃旧进程迟到的进度和结果。
    /// - `None` 表示当前没有受宿主追踪的运行中插件命令，窗口可以展示最终结果或空状态。
    pub(in crate::app) active_command_generation: Option<usize>,
    /// 下一个插件命令代次。
    ///
    /// 边界条件：
    /// - 代次只在当前会话内用于 UI 竞争消解，不写入配置；溢出时使用饱和加一即可，实际会话不会达到上限。
    pub(in crate::app) next_command_generation: usize,
}

impl PluginWorkspaceState {
    /// 从 JSON 注册表加载插件工作区状态。
    ///
    /// 边界条件：
    /// - 注册表读取失败会回退为空注册表；具体磁盘错误不阻断主窗口启动，用户可以在设置页重新加载插件。
    pub(in crate::app) fn load() -> Self {
        let registry = load_plugin_registry();
        let definitions = load_plugin_definitions(&registry);
        Self {
            registry,
            definitions,
            status_message: None,
            page_window: None,
            active_command_generation: None,
            next_command_generation: 1,
        }
    }

    /// 按当前注册表重新读取插件 manifest。
    ///
    /// 业务意图：
    /// - 加载目录插件、安装 ZIP、启用禁用和卸载后都需要立刻刷新贡献点列表，让右键菜单和设置页保持一致。
    pub(in crate::app) fn reload_definitions(&mut self) {
        self.definitions = load_plugin_definitions(&self.registry);
    }

    /// 开始追踪一个新的插件命令并返回代次。
    pub(in crate::app) fn begin_command_generation(&mut self) -> usize {
        let generation = self.next_command_generation;
        self.next_command_generation = self.next_command_generation.saturating_add(1);
        self.active_command_generation = Some(generation);
        generation
    }
}

impl SettingsState {
    /// 创建设置状态并读取已有持久化偏好。
    ///
    /// 边界条件：
    /// - 读取失败仍沿用原有默认值规则；该构造函数不把失败暴露为 UI 错误，避免影响日志查看主流程。
    pub(in crate::app) fn new(context: &mut Context<MainView>) -> Self {
        Self {
            settings_window: None,
            settings_window_open_pending: false,
            settings_active_tab: SettingsTab::General,
            theme_preference: load_theme_preference(),
            log_viewer_font_size: load_log_viewer_font_size_preference(),
            shell_integration_state: ShellIntegrationUiState::Unknown,
            thread_analysis_filter_text: load_thread_analysis_filter_preference(),
            thread_analysis_filter_is_editing: false,
            thread_analysis_filter_saved_text_before_edit: None,
            quick_search_keywords_input: SingleLineTextInputState::from_text(
                load_quick_search_keywords_preference(),
            ),
            quick_search_keywords_is_editing: false,
            quick_search_keywords_saved_text_before_edit: None,
            quick_search_keywords_focus: context.focus_handle(),
            quick_search_keywords_last_layout: None,
            quick_search_keywords_last_bounds: None,
            quick_search_keywords_selection_drag: None,
            thread_analysis_filter_selection_range: 0..0,
            thread_analysis_filter_marked_range: None,
            thread_analysis_filter_focus: context.focus_handle(),
            thread_analysis_filter_last_layouts: Vec::new(),
            thread_analysis_filter_last_bounds: None,
            thread_analysis_filter_selection_drag: None,
            storage: StorageSettingsState::new(),
        }
    }
}

/// 模型配置列表、表单和接口测试状态。
///
/// 业务意图：
/// - 模型配置包含可持久化列表和当前未保存表单草稿，单独成组后设置窗口可以围绕该状态继续演进。
/// - API Key 明文仍只存在于原有内存字段和配置文件中，本次重构不改变安全边界。
pub(in crate::app) struct ModelConfigState {
    /// 已保存的模型配置列表。
    pub(in crate::app) model_config_profiles: Vec<ModelProfile>,
    /// 当前默认模型配置 ID。
    pub(in crate::app) model_config_default_profile_id: Option<String>,
    /// 当前在模型配置列表中选中的已保存配置 ID。
    pub(in crate::app) model_config_selected_profile_id: Option<String>,
    /// 当前表单对应的已保存配置 ID。
    pub(in crate::app) model_config_form_profile_id: Option<String>,
    /// 模型配置名称输入框状态。
    pub(in crate::app) model_config_name_input: ModelConfigTextFieldState,
    /// 模型 Base URL 输入框状态。
    pub(in crate::app) model_config_base_url_input: ModelConfigTextFieldState,
    /// 模型 API Key 输入框状态。
    pub(in crate::app) model_config_api_key_input: ModelConfigTextFieldState,
    /// 模型 ID 输入框状态。
    pub(in crate::app) model_config_model_input: ModelConfigTextFieldState,
    /// API Key 是否在 UI 中明文显示。
    pub(in crate::app) model_config_api_key_visible: bool,
    /// 模型测试请求状态。
    pub(in crate::app) model_test_status: ModelTestStatus,
    /// 下一个模型测试任务 ID。
    pub(in crate::app) next_model_test_job_id: usize,
}

impl ModelConfigState {
    /// 从持久化模型配置创建设置页表单状态。
    ///
    /// 业务意图：
    /// - 启动时继续选中第一条配置并把它填入表单，保证设置窗口首次打开行为不变。
    /// - 默认模型 ID 只保存到状态中，不在这里重新校验；读取配置阶段已经清理悬空 ID。
    pub(in crate::app) fn new(
        context: &mut Context<MainView>,
        model_configs: ModelConfigs,
    ) -> Self {
        let selected_model_profile_id = model_configs
            .profiles
            .first()
            .map(|profile| profile.id.clone());
        let selected_model_profile = selected_model_profile_id.as_ref().and_then(|profile_id| {
            model_configs
                .profiles
                .iter()
                .find(|profile| &profile.id == profile_id)
        });
        let mut model_config_name_input = ModelConfigTextFieldState::new(context);
        let mut model_config_base_url_input = ModelConfigTextFieldState::new(context);
        let mut model_config_api_key_input = ModelConfigTextFieldState::new(context);
        let mut model_config_model_input = ModelConfigTextFieldState::new(context);
        let model_config_form_profile_id = selected_model_profile.map(|profile| {
            model_config_name_input.set_text(profile.name.clone());
            model_config_base_url_input.set_text(profile.base_url.clone());
            model_config_api_key_input.set_text(profile.api_key.clone());
            model_config_model_input.set_text(profile.model.clone());
            profile.id.clone()
        });

        Self {
            model_config_profiles: model_configs.profiles,
            model_config_default_profile_id: model_configs.default_profile_id,
            model_config_selected_profile_id: selected_model_profile_id,
            model_config_form_profile_id,
            model_config_name_input,
            model_config_base_url_input,
            model_config_api_key_input,
            model_config_model_input,
            model_config_api_key_visible: false,
            model_test_status: ModelTestStatus::Idle,
            next_model_test_job_id: 1,
        }
    }
}
