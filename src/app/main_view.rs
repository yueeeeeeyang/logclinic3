//! 主窗口根实体和根状态构造模块。
//!
//! 业务意图：
//! - 该模块承载 `MainView` 字段组成和根构造函数，让应用装配入口不再直接混合所有工作区状态初始化细节。
//! - `MainView` 仍然是唯一 GPUI 根实体；日志、搜索、设置、AI 和 HPROF 功能域继续通过 `app` 内部模块协作。
//!
//! 边界条件：
//! - 构造函数只组合既有状态容器，不改变窗口生命周期、配置格式、AI 数据库路径或日志加载行为。
//! - 所有字段保持 `app` 内部可见，不新增公开 API。

use super::*;

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
pub(in crate::app) struct MainView {
    /// 主窗口句柄。
    ///
    /// 业务意图：
    /// - 线程分析、搜索等独立窗口可能触发主窗口定位行为；保存主窗口句柄后，这些辅助窗口可以在操作完成后
    ///   把主窗口重新激活到前台，让用户立刻看到跳转结果。
    ///
    /// 边界条件：
    /// - `MainView::new` 执行时主窗口句柄尚未由 GPUI 返回，因此初始化为 `None`，窗口创建完成后立即回填。
    pub(in crate::app) main_window: Option<WindowHandle<MainView>>,

    /// 左侧内容区域当前宽度。
    ///
    /// 业务意图：
    /// - 保存用户在当前进程内拖动分割线后的结果，让左右栏布局能即时响应。
    /// - 初始值来自 `LEFT_PANEL_DEFAULT_WIDTH`，严格满足默认左侧 300px 的需求。
    ///
    /// 边界条件：
    /// - 当前宽度不写入磁盘；关闭应用后会恢复默认值。
    /// - 更新宽度时必须经过最小宽度约束，避免左右任一区域被拖到不可用。
    pub(in crate::app) left_panel_width: f32,

    /// 当前是否处于拖动分割线状态。
    ///
    /// 业务意图：
    /// - 区分普通鼠标移动和调整分栏宽度，避免鼠标经过内容区域时误改布局。
    ///
    /// 边界条件：
    /// - 鼠标左键在分割线按下时变为 `true`，在内容区域收到左键释放时恢复为 `false`。
    /// - 当前不处理窗口失焦或鼠标释放发生在窗口外的情况，后续如需更强交互再补充捕获策略。
    pub(in crate::app) is_resizing_splitter: bool,

    /// 主窗口左侧导航和全局滚动目标状态。
    ///
    /// 业务意图：
    /// - 导航状态已经从根视图拆入独立容器，避免日志、搜索和设置功能继续直接扩张 `MainView` 字段列表。
    /// - 该状态只保存当前会话内的临时交互信息，不写入配置文件。
    pub(in crate::app) navigation: NavigationState,

    /// 日志加载、目录树、tab、编码菜单和日志正文相关状态。
    ///
    /// 业务意图：
    /// - 日志查看主流程的状态统一收口到该字段，后续重新加载日志或关闭 tab 时可以更清晰地区分工作区状态和全局偏好。
    /// - 本次迁移不改变任何日志来源、编码、搜索跳转或另存为行为。
    pub(in crate::app) log: LogWorkspaceState,

    /// 搜索窗口、搜索历史和底部结果面板状态。
    ///
    /// 业务意图：
    /// - 搜索状态与日志 tab 状态仍需协作，但先拆成独立容器可以减少 `MainView` 的字段耦合。
    /// - 后台搜索任务 ID、独立窗口句柄和结果面板滚动状态继续保持当前会话内有效。
    pub(in crate::app) search: SearchWorkspaceState,

    /// 设置窗口、主题、字号、快搜和线程过滤配置状态。
    ///
    /// 业务意图：
    /// - 设置相关字段集中后，配置读写迁移和后续设置窗口维护都不再需要穿过完整 `MainView` 字段列表。
    /// - 当前只迁移状态所有权，不改变配置文件路径、默认值或保存时机。
    pub(in crate::app) settings: SettingsState,

    /// 模型配置列表、表单和接口测试状态。
    ///
    /// 业务意图：
    /// - 模型配置既服务设置页，也服务 AI 对话默认模型选择；单独成组可以让这两个功能共享同一份状态快照。
    /// - API Key 的明文存储策略保持原状，本次重构不扩大可见范围。
    pub(in crate::app) model_config: ModelConfigState,

    /// 插件注册、贡献点和插件声明式结果窗口状态。
    ///
    /// 业务意图：
    /// - 设置页、日志树右键菜单和笔记树右键菜单都需要读取同一份插件定义快照。
    /// - 插件第一版只通过外部进程协议返回声明式页面或消息，不允许直接持有或修改日志正文、笔记正文等内部状态。
    ///
    /// 边界条件：
    /// - 插件注册表读取失败不阻断主窗口启动；用户安装插件的加载错误会在设置页中展示。
    pub(in crate::app) plugins: PluginWorkspaceState,

    /// 线程日志分析独立窗口句柄。
    ///
    /// 业务意图：
    /// - 线程分析窗口生命周期仍由主视图协调，避免本次状态拆分改变独立窗口复用和激活规则。
    /// - 句柄只服务当前会话；关闭窗口后由回调清空。
    pub(in crate::app) thread_analysis_window: Option<WindowHandle<ThreadAnalysisWindowView>>,

    /// 当前线程日志分析后台任务代次。
    ///
    /// 业务意图：
    /// - 用户可能在旧解析尚未完成时再次启动线程日志分析；代次用于丢弃旧任务迟到的进度和结果，避免旧窗口覆盖新分析。
    /// - 该值只在当前 UI 会话内使用，不写入配置；溢出时饱和加一即可。
    pub(in crate::app) thread_analysis_generation: usize,

    /// 日志智能分析独立窗口句柄。
    ///
    /// 业务意图：
    /// - 智能分析窗口需要持续展示后台流式结果和用户停止/继续操作，生命周期由主视图协调。
    /// - 句柄只服务当前会话；关闭窗口后由关闭回调清空，后台任务会通过窗口视图 drop 置位取消标记。
    pub(in crate::app) log_ai_analysis_window: Option<WindowHandle<LogAiAnalysisWindowView>>,

    /// HPROF dump 分析内嵌视图实体。
    ///
    /// 业务意图：
    /// - HPROF 分析视图仍保持原有实体生命周期，后续单独拆分 HPROF 模块时再调整内部结构。
    /// - 实体只服务当前会话；应用关闭或实体释放时由视图自身取消后台解析任务。
    pub(in crate::app) hprof_analysis_view: Option<Entity<HprofAnalysisView>>,

    /// AI 对话页面完整工作区状态。
    ///
    /// 业务意图：
    /// - AI 会话、消息、输入、列表滚动、数据库错误和流式任务由 AI 功能域统一维护。
    /// - 主视图只负责在大功能页之间协调，避免根实体继续直接理解 AI 数据库初始化细节。
    pub(in crate::app) ai_chat: AiChatWorkspaceState,

    /// 笔记页面完整工作区状态。
    ///
    /// 业务意图：
    /// - 笔记目录树、当前笔记、阅读器选区、编辑草稿和确认弹窗统一收口到该字段。
    /// - 主视图只负责在大功能页之间协调，避免根实体直接理解笔记文件扫描和迁移细节。
    pub(in crate::app) notes: NotesWorkspaceState,

    /// 当前窗口系统外观。
    ///
    /// 业务意图：
    /// - 当主题偏好为“跟随系统”时，需要用 GPUI 提供的窗口外观计算实际调色板。
    /// - 该字段会随系统外观变化更新，驱动主窗口和独立窗口重绘。
    pub(in crate::app) system_window_appearance: WindowAppearance,

    /// 主窗口外观变化订阅。
    ///
    /// 业务意图：
    /// - GPUI 的窗口外观监听返回订阅句柄，必须保存在主视图中，否则监听会立即失效。
    pub(in crate::app) window_appearance_subscription: Option<gpui::Subscription>,

    /// 全局键盘监听订阅。
    ///
    /// 业务意图：
    /// - GPUI 的全局快捷键拦截器返回订阅句柄，必须跟随主视图保存，否则订阅被释放后 `Ctrl+F` 将不再生效。
    /// - 使用拦截器而不是事后观察器，是为了在 macOS key equivalent 和输入控件提前消费事件前捕获搜索快捷键。
    pub(in crate::app) global_keystroke_subscription: Option<gpui::Subscription>,

    /// 主界面根节点焦点句柄。
    ///
    /// 业务意图：
    /// - 主窗口需要存在稳定焦点路径，搜索对话框关闭后可以把焦点恢复到根节点，后续全局快捷键和普通鼠标操作才能继续落到主界面。
    /// - 搜索对话框关闭后可把焦点还给根节点，避免焦点停在已经关闭的搜索窗口上导致后续快捷键无响应。
    pub(in crate::app) root_focus_handle: gpui::FocusHandle,
}

impl MainView {
    /// 创建主窗口根视图。
    ///
    /// 业务意图：
    /// - 集中初始化所有首屏 UI 状态，避免在 `main` 的窗口创建回调中散落默认值。
    /// - 左侧栏默认 300px 是用户明确要求，必须从这里作为唯一入口初始化。
    pub(in crate::app) fn new(context: &mut Context<Self>) -> Self {
        let model_configs = load_model_configs_preference();
        let model_config = ModelConfigState::new(context, model_configs);
        let ai_chat = AiChatWorkspaceState::new_unloaded(context);
        let notes = NotesWorkspaceState::load_or_initialize(context);

        // AI 对话页第一次渲染代码块时需要初始化 syntect 语法和主题集合；该初始化与 UI 状态无关，
        // 提前放到 GPUI 后台执行器中完成，避免用户首次点击 AI 导航时把这部分成本压到主线程。
        context
            .background_spawn(async {
                prewarm_app_markdown_code_highlighting();
            })
            .detach();

        Self {
            left_panel_width: LEFT_PANEL_DEFAULT_WIDTH,
            main_window: None,
            is_resizing_splitter: false,
            navigation: NavigationState::new(),
            log: LogWorkspaceState::new(context),
            search: SearchWorkspaceState::new(context),
            settings: SettingsState::new(context),
            model_config,
            plugins: PluginWorkspaceState::load(),
            thread_analysis_window: None,
            thread_analysis_generation: 0,
            log_ai_analysis_window: None,
            hprof_analysis_view: None,
            ai_chat,
            notes,
            system_window_appearance: WindowAppearance::Light,
            window_appearance_subscription: None,
            global_keystroke_subscription: None,
            root_focus_handle: context.focus_handle(),
        }
    }
}
