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
    pub(in crate::app) fn new() -> Self {
        Self {
            load_state: LogTreeLoadState::Empty,
            log_tree_scroll_handle: UniformListScrollHandle::new(),
            log_tree_selected_node_ids: HashSet::new(),
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
            search_results_panel: None,
            search_results_resize_drag: None,
            search_results_scrollbar_drag: None,
            search_results_context_menu: None,
            next_search_job_id: 1,
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
