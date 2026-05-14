// 主窗口导航、工具栏和加载入口协调。
//
// 业务意图：
// - 承载主导航、主题色板、工具栏、加载来源菜单和路径选择器入口，保持根模块只负责装配。
// - 所有行为保持迁移前一致，不改变窗口生命周期、路径选择策略或加载状态文案。

use super::*;

impl MainView {
    /// 返回当前主视图实际生效的主题。
    pub(in crate::app) fn effective_theme(&self) -> EffectiveTheme {
        EffectiveTheme::resolve(
            self.settings.theme_preference,
            self.system_window_appearance,
        )
    }

    /// 返回当前主视图调色板。
    pub(in crate::app) fn palette(&self) -> AppThemePalette {
        AppThemePalette::for_theme(self.effective_theme())
    }

    /// 更新系统窗口外观。
    ///
    /// 业务意图：
    /// - 当用户选择“跟随系统”时，系统外观变化应立即驱动当前窗口重绘。
    /// - 即使用户强制选择明亮或暗色，也保存最新系统外观，方便之后切回“跟随系统”时立即正确。
    pub(in crate::app) fn set_system_window_appearance(
        &mut self,
        appearance: WindowAppearance,
        context: &mut Context<Self>,
    ) {
        self.system_window_appearance = appearance;
        context.notify();
    }

    /// 渲染主窗口左侧大导航竖条。
    ///
    /// 业务意图：
    /// - 左侧导航是主窗口唯一的全局功能入口，顶部三个图标切换大功能，底部设置图标打开设置窗口。
    /// - 关于内容已经收入口设置窗口，不再在大导航底部占用独立入口。
    /// - 按用户要求导航本体不显示文字；功能名称只在 hover 气泡中显示。
    ///
    /// 边界条件：
    /// - 导航宽度固定，不随窗口缩放变化；窄屏时优先保留入口可点击性，再由右侧内容区自行处理可用宽度。
    pub(in crate::app) fn render_main_navigation(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = self.palette();
        let main_items = MainFeature::all()
            .iter()
            .copied()
            .map(|feature| {
                self.render_main_navigation_button(
                    MainNavigationItem::Feature(feature),
                    self.navigation.active_main_feature == feature,
                    palette,
                    context,
                )
            })
            .collect::<Vec<_>>();

        div()
            .id("main-navigation")
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .justify_between()
            .w(px(MAIN_NAV_WIDTH))
            .h_full()
            .flex_none()
            .py(px(MAIN_NAV_PADDING))
            .bg(rgb(palette.panel))
            .border_r_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(MAIN_NAV_BUTTON_GAP))
                    .children(main_items),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(MAIN_NAV_BUTTON_GAP))
                    .child(self.render_main_navigation_button(
                        MainNavigationItem::Settings,
                        false,
                        palette,
                        context,
                    )),
            )
    }

    /// 渲染左侧大导航中的单个图标按钮。
    ///
    /// 业务意图：
    /// - 按钮本体只渲染图标，hover 状态下把中文名称作为气泡渲染到按钮右侧。
    /// - 设置不是主功能页，不参与选中态；点击后打开设置窗口，关于内容从设置窗口页签访问。
    pub(in crate::app) fn render_main_navigation_button(
        &self,
        item: MainNavigationItem,
        selected: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let hovered = self.navigation.hovered_navigation_item == Some(item);
        let text_color = if selected || hovered {
            palette.accent
        } else {
            palette.muted_text
        };

        div()
            .id(SharedString::from(format!("main-nav-{}", item.label())))
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .w(px(MAIN_NAV_BUTTON_SIZE))
            .h(px(MAIN_NAV_BUTTON_SIZE))
            .rounded(px(8.0))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.panel
            }))
            .text_color(rgb(text_color))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .active(|button| button.opacity(0.82))
            .child(Self::render_lucide_icon(
                Some(item.icon()),
                MAIN_NAV_ICON_WIDTH,
                MAIN_NAV_ICON_SIZE,
                text_color,
            ))
            .on_hover(
                context.listener(move |view, is_hovered: &bool, _window, context| {
                    let next_item = (*is_hovered).then_some(item);
                    if view.navigation.hovered_navigation_item != next_item {
                        view.navigation.hovered_navigation_item = next_item;
                        context.notify();
                    }
                }),
            )
            .on_click(
                context.listener(move |view, _event: &ClickEvent, window, context| {
                    match item {
                        MainNavigationItem::Feature(feature) => {
                            view.select_main_feature(feature, context);
                        }
                        MainNavigationItem::Settings => {
                            view.schedule_open_settings_window(window, context);
                        }
                    }
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染左侧导航 hover 名称气泡覆盖层。
    ///
    /// 业务意图：
    /// - 主导航只显示图标，为了避免用户猜测图标语义，鼠标悬浮时在右侧显示中文功能名称。
    /// - 气泡必须作为主窗口根节点的后置覆盖层绘制；如果作为导航按钮子元素，右侧功能页会在后续绘制中覆盖它。
    pub(in crate::app) fn render_main_navigation_tooltip_overlay(
        &self,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(item) = self.navigation.hovered_navigation_item else {
            return div().id("main-nav-tooltip-empty").hidden();
        };
        let palette = self.palette();
        let tooltip = Self::render_main_navigation_tooltip(item.label(), palette);

        match item.tooltip_anchor() {
            MainNavigationTooltipAnchor::Top(top) => tooltip.top(px(top)),
            MainNavigationTooltipAnchor::Bottom(bottom) => tooltip.bottom(px(bottom)),
        }
    }

    /// 渲染左侧导航 hover 名称气泡本体。
    ///
    /// 业务意图：
    /// - 气泡样式集中在这里，垂直位置由 `MainNavigationItem::tooltip_anchor` 决定，避免按钮布局和根层覆盖层重复写样式。
    pub(in crate::app) fn render_main_navigation_tooltip(
        label: &'static str,
        palette: AppThemePalette,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("main-nav-tooltip-{label}")))
            .absolute()
            .left(px(MAIN_NAV_WIDTH + MAIN_NAV_TOOLTIP_GAP))
            .w(px(MAIN_NAV_TOOLTIP_WIDTH))
            .h(px(MAIN_NAV_TOOLTIP_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(palette.text))
            .child(label)
    }

    /// 切换主窗口大功能页。
    ///
    /// 业务意图：
    /// - 大导航切换时应收起日志页中的临时弹层，避免用户进入 HPROF 或 AI 页后仍看到旧 tab 菜单、编码下拉或搜索结果菜单。
    pub(in crate::app) fn select_main_feature(
        &mut self,
        feature: MainFeature,
        context: &mut Context<Self>,
    ) {
        if self.navigation.active_main_feature == MainFeature::Notes
            && feature != MainFeature::Notes
            && self.notes.has_unsaved_changes()
        {
            self.notes.unsaved_dialog = Some(NotesUnsavedDialog {
                action: NotesPendingAction::SwitchFeature(feature),
            });
            context.notify();
            return;
        }
        self.navigation.active_main_feature = feature;
        self.log.load_source_menu = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        self.log.log_tree_context_menu = None;
        self.search.search_results_context_menu = None;
        if feature == MainFeature::AiChat {
            self.ensure_ai_chat_initial_data_loaded(context);
        }
        context.notify();
    }

    /// 构建日志分析页顶部操作栏。
    ///
    /// 业务意图：
    /// - 根级文字工具栏已迁移到左侧大导航；日志页仍需要保留“加载日志”和“搜索”两个高频入口。
    pub(in crate::app) fn render_log_action_bar(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = self.palette();

        div()
            .id("log-action-bar")
            .flex()
            .items_center()
            .gap_2()
            .h(px(TOOLBAR_HEIGHT))
            .pl(px(LOG_TREE_ROW_HORIZONTAL_PADDING))
            .pr_4()
            .bg(rgb(palette.panel))
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(self.render_load_toolbar_button(context))
            .child(self.render_search_toolbar_button(context))
    }

    /// 构建“加载日志”工具栏按钮。
    ///
    /// 业务意图：
    /// - 该按钮是当前首个真实业务入口，点击后直接打开系统选择器，避免用户误判为没有响应。
    /// - 与其它工具栏按钮保持相同文字按钮样式，避免功能入口因为有状态而产生视觉突兀。
    ///
    /// 边界条件：
    /// - 按钮左内边距为 0，使图标左缘和加载后目录树标题左缘使用同一条基准线。
    /// - Windows 原生选择器不能在同一个对话框里同时选择文件和目录，因此加载入口会先弹出来源类型菜单。
    pub(in crate::app) fn render_load_toolbar_button(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let action = &TOOLBAR_ACTIONS[0];
        let palette = self.palette();

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
            .text_color(rgb(palette.text))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(move |button| button.text_color(rgb(palette.accent)))
            .active(|button| button.opacity(0.82))
            .child(Self::render_lucide_icon(
                Some(action.icon),
                TOOLBAR_BUTTON_ICON_WIDTH,
                TOOLBAR_BUTTON_ICON_SIZE,
                palette.muted_text,
            ))
            .child(action.label)
            .on_click(context.listener(Self::open_log_sources_prompt))
    }

    /// 构建“搜索”工具栏按钮。
    ///
    /// 业务意图：
    /// - 搜索除了快捷键外必须有可见入口，用户在 macOS/Windows 快捷键被系统或输入法拦截时仍能打开搜索窗口。
    /// - 搜索按钮放在日志分析页操作栏内，只作用于当前日志工作区，不污染 HPROF 和 AI 页的占位状态。
    ///
    /// 边界条件：
    /// - 点击按钮来自鼠标事件，不处于 macOS key equivalent 回调栈中，因此可以直接打开或激活独立搜索窗口。
    /// - 如果日志正文已有选区，沿用快捷键入口的预填逻辑，把选中文本写入搜索关键字。
    pub(in crate::app) fn render_search_toolbar_button(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let action = &TOOLBAR_ACTIONS[1];
        let palette = self.palette();

        div()
            .id(SharedString::from(action.label))
            .flex()
            .items_center()
            .gap_1()
            .flex_none()
            .px(px(TOOLBAR_BUTTON_HORIZONTAL_PADDING))
            .py(px(TOOLBAR_BUTTON_VERTICAL_PADDING))
            .text_sm()
            .text_color(rgb(palette.text))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(move |button| button.text_color(rgb(palette.accent)))
            .active(|button| button.opacity(0.82))
            .child(Self::render_lucide_icon(
                Some(action.icon),
                TOOLBAR_BUTTON_ICON_WIDTH,
                TOOLBAR_BUTTON_ICON_SIZE,
                palette.muted_text,
            ))
            .child(action.label)
            .on_click(context.listener(Self::open_search_from_toolbar))
    }

    /// 打开日志来源选择器。
    ///
    /// 业务意图：
    /// - 用户点击“加载日志”后必须立即看到系统选择器反馈，避免工具栏按钮看起来没有响应。
    /// - 支持混选的平台直接打开文件/目录混选选择器；Windows 等不支持混选的平台先展示来源类型菜单。
    pub(in crate::app) fn open_log_sources_prompt(
        &mut self,
        event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if Self::should_show_load_source_menu() {
            let position = event.position();
            self.toggle_load_source_menu(f32::from(position.x), context);
        } else {
            self.begin_path_prompt(LoadPromptKind::Sources, context);
        }
    }

    /// 从 HPROF 页打开 HPROF 文件选择器。
    ///
    /// 业务意图：
    /// - HPROF 分析只接受单个 dump 文件，不依赖当前日志树状态，也不会清空已经加载的日志工作区。
    /// - 选择器本身无法过滤 `.hprof/.bin`，因此这里只负责拿到路径，真正校验由 HPROF 页后台任务执行并展示错误。
    pub(in crate::app) fn open_hprof_from_toolbar(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.begin_hprof_file_prompt(context);
    }

    /// 从工具栏按钮打开搜索窗口。
    ///
    /// 业务意图：
    /// - 该入口和 `Ctrl+F` / `Cmd+F` 使用同一套 `open_search_dialog` 状态初始化逻辑，保证查询词预填、目录目标和已有窗口激活行为一致。
    /// - GPUI 的点击监听执行时 `MainView` 仍处于更新租借中；搜索窗口创建会观察并读取 `MainView`，
    ///   因此这里也必须排到下一帧执行，避免“cannot read MainView while it is already being updated”。
    pub(in crate::app) fn open_search_from_toolbar(
        &mut self,
        _event: &ClickEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.schedule_open_search_dialog(window, context);
    }

    /// 打开 HPROF 文件选择器，并在确认后打开分析窗口。
    ///
    /// 边界条件：
    /// - 用户取消选择时不改变现有 UI。
    /// - 当前只取第一个路径；`multiple=false` 已要求系统选择器只返回单个文件，但这里仍防御性处理平台差异。
    pub(in crate::app) fn begin_hprof_file_prompt(&mut self, context: &mut Context<Self>) {
        let main_view = context.entity();
        context
            .spawn(async move |_view, app| {
                let options = PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: false,
                    prompt: Some("选择 HPROF dump 文件".into()),
                };
                let receiver = match app.update(|app| app.prompt_for_paths(options)) {
                    Ok(receiver) => receiver,
                    Err(_error) => return,
                };
                let selected_path = match receiver.await {
                    Ok(Ok(Some(paths))) => paths.into_iter().next(),
                    Ok(Ok(None)) | Ok(Err(_)) | Err(_) => None,
                };
                let Some(selected_path) = selected_path else {
                    return;
                };

                app.update(move |app| {
                    Self::open_hprof_analysis_page_after_main_update(main_view, selected_path, app);
                })
                .ok();
            })
            .detach();
    }

    /// 判断当前平台是否需要先展示“加载日志”来源类型菜单。
    ///
    /// 业务意图：
    /// - GPUI 0.2.2 的 Windows 和 Linux 后端目前不能在同一个系统对话框里混选文件与目录。
    /// - Windows 用户已经反馈只看到目录、看不到压缩包，因此这些平台先让用户选择“文件/压缩包”或“目录”。
    ///
    /// 边界条件：
    /// - macOS 后端支持混选，继续保持一次打开系统选择器的原有高效路径。
    pub(in crate::app) fn should_show_load_source_menu() -> bool {
        cfg!(any(target_os = "windows", target_os = "linux"))
    }

    /// 切换加载日志来源类型菜单。
    ///
    /// 业务意图：
    /// - 用户在 Windows 上点击“加载日志”时先看到两个明确入口，避免系统选择器隐藏压缩包文件。
    /// - 再次点击工具栏按钮会收起菜单，符合下拉按钮的常见交互预期。
    pub(in crate::app) fn toggle_load_source_menu(
        &mut self,
        window_x: f32,
        context: &mut Context<Self>,
    ) {
        self.log.load_source_menu = if self.log.load_source_menu.is_some() {
            None
        } else {
            Some(LoadSourceMenu {
                x: Self::load_source_menu_x(window_x),
                y: TOOLBAR_HEIGHT + LOAD_SOURCE_MENU_TOP_GAP,
            })
        };
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        self.log.log_tree_context_menu = None;
        self.search.search_results_context_menu = None;
        context.notify();
    }

    /// 计算加载日志来源菜单的横坐标。
    ///
    /// 边界条件：
    /// - 点击可能落在按钮图标或文字上，横向回退后再限制到窗口左边界，避免菜单超出可见区域。
    pub(in crate::app) fn load_source_menu_x(window_x: f32) -> f32 {
        (window_x - LOAD_SOURCE_MENU_POINTER_BACKTRACK).max(0.0)
    }

    /// 执行加载日志来源菜单命令。
    ///
    /// 业务意图：
    /// - 菜单项只负责选择系统对话框类型；最终路径扫描仍复用 `begin_path_prompt`，保证文件、目录、拖拽入口共用加载流程。
    pub(in crate::app) fn select_load_source_kind(
        &mut self,
        prompt_kind: LoadPromptKind,
        context: &mut Context<Self>,
    ) {
        self.log.load_source_menu = None;
        self.begin_path_prompt(prompt_kind, context);
        context.notify();
    }

    /// 渲染加载日志来源菜单的关闭遮罩。
    ///
    /// 业务意图：
    /// - 菜单打开后用户点击菜单外任意位置都应收起，避免悬浮菜单长期遮挡工具栏或内容区。
    pub(in crate::app) fn render_load_source_menu_dismiss_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if self.log.load_source_menu.is_none() {
            return div().id("load-source-menu-dismiss-overlay-empty").hidden();
        }

        div()
            .id("load-source-menu-dismiss-overlay")
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.log.load_source_menu = None;
                    context.notify();
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染加载日志来源类型菜单。
    ///
    /// 业务意图：
    /// - Windows 不能混选文件和目录时，通过两个自绘菜单项明确区分“文件/压缩包”和“目录”入口。
    /// - 菜单作为根节点弹层渲染，不受日志目录树是否已经加载、是否隐藏左侧树影响。
    pub(in crate::app) fn render_load_source_menu(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = &self.log.load_source_menu else {
            return div().id("load-source-menu-empty").hidden();
        };
        let palette = self.palette();

        div()
            .id("load-source-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(LOAD_SOURCE_MENU_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 菜单自身覆盖在全屏关闭遮罩之上，空白边距点击不能继续落到遮罩或底层工具栏。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 右键点在菜单上只应停留在当前菜单层，避免误触发底层区域的右键处理。
                    context.stop_propagation();
                }),
            )
            .child(self.render_load_source_menu_item(
                LoadPromptKind::FilesOrArchives,
                Icon::FileArchive,
                "文件/压缩包",
                palette,
                context,
            ))
            .child(self.render_load_source_menu_item(
                LoadPromptKind::Directories,
                Icon::FolderOpen,
                "目录",
                palette,
                context,
            ))
    }

    /// 渲染加载日志来源菜单单项。
    ///
    /// 业务意图：
    /// - 菜单项用图标区分文件/压缩包和目录，减少 Windows 下需要理解系统选择器模式的负担。
    pub(in crate::app) fn render_load_source_menu_item(
        &self,
        prompt_kind: LoadPromptKind,
        icon: Icon,
        label: &'static str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("load-source-menu-{label}")))
            .flex()
            .items_center()
            .gap_2()
            .h(px(LOAD_SOURCE_MENU_ITEM_HEIGHT))
            .px_3()
            .text_sm()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |item| item.bg(rgb(palette.hover)).text_color(rgb(palette.accent)))
            .child(Self::render_lucide_icon(
                Some(icon),
                16.0,
                15.0,
                palette.muted_text,
            ))
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                    view.select_load_source_kind(prompt_kind, context);
                    context.stop_propagation();
                }),
            )
    }

    /// 在当前主视图更新结束后打开设置窗口。
    ///
    /// 业务意图：
    /// - 设置窗口会读取并观察 `MainView`，直接在按钮监听中创建会和当前更新租借冲突。
    /// - 重复点击设置按钮时只排队一次，避免同一帧创建多个设置窗口。
    pub(in crate::app) fn schedule_open_settings_window(
        &mut self,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.settings.settings_window_open_pending {
            return;
        }

        self.settings.settings_window_open_pending = true;
        let main_view = context.entity();
        window.defer(context, move |_window, app| {
            Self::open_settings_window_after_main_update(main_view, app);
        });
        context.notify();
    }

    /// 启动系统路径选择器，并在用户确认后异步扫描路径。
    ///
    /// 业务意图：
    /// - 系统选择器必须在前台应用上下文中打开；文件系统扫描可能较慢，因此放到后台执行器。
    /// - 新一轮扫描完成前会清空旧 tab，避免右侧继续展示不属于当前来源树的日志内容。
    /// - Windows 后端不能混选文件和目录，打开选择器前必须根据平台能力生成参数，避免只显示目录而隐藏压缩包文件。
    ///
    /// 边界条件：
    /// - 用户取消选择时保持现有目录树不变。
    /// - 当前不支持取消后台扫描；如果用户连续触发多次加载，后完成的任务会覆盖先完成的任务。
    pub(in crate::app) fn begin_path_prompt(
        &mut self,
        prompt_kind: LoadPromptKind,
        context: &mut Context<Self>,
    ) {
        self.log.load_source_menu = None;
        let loading_message = prompt_kind.loading_message().to_string();

        context
            .spawn(async move |view, app| {
                let receiver = match app.update(|app| {
                    let options =
                        prompt_kind.to_prompt_options(app.can_select_mixed_files_and_dirs());
                    app.prompt_for_paths(options)
                }) {
                    Ok(receiver) => receiver,
                    Err(error) => {
                        view.update(app, |view, context| {
                            view.log.load_state = LogTreeLoadState::Failed {
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
                            view.log.load_state = LogTreeLoadState::Failed {
                                message: format!("路径选择器返回错误：{}", error),
                            };
                            context.notify();
                        })
                        .ok();
                        return;
                    }
                    Err(error) => {
                        view.update(app, |view, context| {
                            view.log.load_state = LogTreeLoadState::Failed {
                                message: format!("路径选择器被中断：{}", error),
                            };
                            context.notify();
                        })
                        .ok();
                        return;
                    }
                };

                view.update(app, |view, context| {
                    view.start_log_source_load(selected_paths, loading_message, context);
                })
                .ok();
            })
            .detach();
    }

    /// 启动一轮日志来源加载。
    ///
    /// 业务意图：
    /// - 系统路径选择器、窗口拖放、程序图标拖放和命令行启动都应该复用同一套加载流程。
    /// - 入口统一后，清理旧 tab、重置搜索结果、单日志自动打开和错误展示不会在不同入口之间出现行为差异。
    ///
    /// 边界条件：
    /// - 空路径通常代表用户取消或平台传入了非文件 URL，直接忽略，避免清空当前工作区。
    /// - 后台扫描不能阻塞 GPUI 主线程；完成后再回到主视图更新 UI。
    pub(in crate::app) fn start_log_source_load(
        &mut self,
        selected_paths: Vec<PathBuf>,
        loading_message: String,
        context: &mut Context<Self>,
    ) {
        if selected_paths.is_empty() {
            return;
        }

        self.clear_workspace_for_new_log_load();
        self.log.load_state = LogTreeLoadState::Loading {
            message: loading_message,
        };
        context.notify();

        context
            .spawn(async move |view, app| {
                let load_result = app
                    .background_executor()
                    .spawn(async move { load_log_sources(selected_paths) })
                    .await;

                view.update(app, |view, context| {
                    view.log.load_state = match load_result {
                        Ok(tree) => {
                            view.log.log_tree_scroll_handle = UniformListScrollHandle::new();
                            view.log.log_tree_scrollbar_drag = None;
                            view.log.log_tree_context_menu = None;
                            view.log.log_tree_selected_node_ids.clear();
                            view.log.log_tree_selection_anchor = None;
                            let tree_state = LoadedLogTreeState::new(tree);
                            let single_log_source = tree_state.single_log_source();
                            let load_state = LogTreeLoadState::Loaded(Box::new(tree_state));

                            // 加载结果只有一个日志时直接打开正文，左侧树由渲染层隐藏。
                            // 这里仍然保留 `Loaded` 状态，搜索当前目录等功能可以继续复用完整来源树。
                            if let Some(source) = single_log_source {
                                view.log.load_state = load_state;
                                view.open_log_file(source, context);
                                return;
                            }

                            load_state
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

    /// 清理重新加载日志前依赖旧来源树的工作区状态。
    ///
    /// 业务意图：
    /// - 用户重新加载日志后，右侧 tab 和搜索结果都不应继续展示旧目录树中的文件，否则会误以为这些结果来自新加载内容。
    /// - 清理集中在一个函数里，避免后续新增右侧工作区状态时只清 tab、漏掉搜索结果或弹层。
    ///
    /// 边界条件：
    /// - 搜索窗口本身不强制关闭，保留用户输入的关键字；但正在运行的搜索会被置为无效，旧后台回调无法继续写回结果面板。
    /// - 只清理会引用旧日志来源的数据，不重置主题、窗口、左侧宽度等会话级偏好。
    pub(in crate::app) fn clear_workspace_for_new_log_load(&mut self) {
        for tab in &self.log.open_tabs {
            Self::cleanup_tab_paged_resources(tab);
        }
        if let LogTreeLoadState::Loaded(tree_state) = &self.log.load_state {
            tree_state.cleanup_temporary_paths();
        }
        self.log.open_tabs.clear();
        self.log.active_tab_id = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        self.log.log_tree_context_menu = None;
        self.log.save_overwrite_confirm_dialog = None;
        self.log.log_tree_selected_node_ids.clear();
        self.log.log_tree_search.reset_for_new_tree();
        self.log.log_tree_selection_anchor = None;
        self.log.log_scrollbar_drag = None;
        self.log.log_tree_scrollbar_drag = None;
        self.log.tab_bar_scroll_handle = ScrollHandle::new();

        self.search.search_results_panel = None;
        self.search.search_results_resize_drag = None;
        self.search.search_results_scrollbar_drag = None;
        self.search.search_results_context_menu = None;
        self.search.next_search_job_id += 1;
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            Self::reset_search_dialog_for_log_reload(dialog);
        }
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
    pub(in crate::app) fn render_lucide_icon(
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
}
