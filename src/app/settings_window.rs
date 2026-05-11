// 设置独立窗口实现。
//
// 业务意图：
// - 该文件集中维护“通用/日志/模型”页签、主题选择、日志显示字号、日志分析过滤、快搜关键字和模型配置设置，避免设置 UI 继续堆在应用根文件里。
// - 当前作为 `app` 的子模块，通过显式 `pub(super)` 接口更新 `MainView` 的会话状态和持久化配置。
//
// 边界条件：
// - 设置窗口承载会写入应用配置目录的偏好，输入类设置必须显式处理只读、编辑和保存状态，避免误触改变跨会话配置。

use super::*;

pub(super) struct SettingsWindowView {
    /// 主窗口视图实体。
    ///
    /// 业务意图：
    /// - 设置窗口需要读写主窗口中的设置状态；状态放在主视图中可以在关闭再打开设置窗口后保留当前会话选择。
    main_view: Entity<MainView>,
    /// 主窗口状态变更订阅。
    ///
    /// 业务意图：
    /// - 如果设置状态被其它入口更新，独立设置窗口需要跟随重绘；订阅句柄必须保存在视图中防止释放。
    _main_view_subscription: gpui::Subscription,
}

/// 设置内容区渲染所需的主视图快照。
///
/// 业务意图：
/// - 设置窗口渲染需要从主视图读取多个偏好和焦点句柄；集中成快照后，内容渲染函数只接收一个稳定输入。
/// - 快照只存在于单次渲染，不写入配置，也不改变设置保存时机。
struct SettingsContentSnapshot {
    /// 当前设置页签。
    active_tab: SettingsTab,
    /// 主题偏好。
    theme: ThemePreference,
    /// 日志正文字号。
    log_viewer_font_size: f32,
    /// 快搜关键字是否处于编辑态。
    quick_search_keywords_is_editing: bool,
    /// 快搜关键字输入焦点。
    quick_search_keywords_focus: gpui::FocusHandle,
    /// 线程过滤规则是否处于编辑态。
    thread_analysis_filter_is_editing: bool,
    /// 线程过滤规则输入焦点。
    thread_analysis_filter_focus: gpui::FocusHandle,
    /// 当前主题色板。
    palette: AppThemePalette,
}

/// 模型配置工具按钮的渲染参数。
///
/// 业务意图：
/// - 模型页多个按钮共享同一视觉样式，参数对象避免布尔和图标位置参数误传。
struct ModelIconButtonRequest {
    /// 元素稳定 ID。
    id: &'static str,
    /// 按钮文字。
    label: &'static str,
    /// 按钮图标。
    icon: Icon,
    /// 是否可点击。
    enabled: bool,
    /// 是否使用主按钮样式。
    primary: bool,
    /// 当前主题色板。
    palette: AppThemePalette,
}

impl SettingsWindowView {
    /// 创建设置窗口根视图。
    pub(super) fn new(main_view: Entity<MainView>, context: &mut Context<Self>) -> Self {
        let observed_main_view = main_view.clone();
        let main_view_subscription = context.observe(&observed_main_view, |_, _, context| {
            context.notify();
        });

        Self {
            main_view,
            _main_view_subscription: main_view_subscription,
        }
    }

    /// 切换设置窗口页签。
    ///
    /// 业务意图：
    /// - 页签状态保存在 `MainView`，让窗口关闭后再次打开仍停留在当前会话最后访问的页签。
    pub(super) fn select_tab(&mut self, tab: SettingsTab, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            view.settings.settings_active_tab = tab;
            context.notify();
        });
        context.notify();
    }

    /// 切换主题选项。
    ///
    /// 业务意图：
    /// - 用户在设置窗口中选择主题后应立即影响所有已打开窗口，并写入配置供下次启动恢复。
    pub(super) fn select_theme(&mut self, theme: ThemePreference, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            view.settings.theme_preference = theme;
            save_theme_preference(theme);
            context.notify();
        });
        context.notify();
    }

    /// 调整日志显示字号。
    ///
    /// 业务意图：
    /// - 用户在通用设置中点击加减按钮后，应立即刷新日志正文和搜索结果预览，并写入配置供下次启动恢复。
    ///
    /// 边界条件：
    /// - 调整结果超出允许范围时不写入，避免设置按钮或损坏状态把日志字号推到不可读或挤破行高的值。
    pub(super) fn adjust_log_viewer_font_size(&mut self, delta: f32, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            let target = view.settings.log_viewer_font_size + delta;
            if let Some(font_size) = normalize_log_viewer_font_size(target) {
                view.settings.log_viewer_font_size = font_size;
                save_log_viewer_font_size_preference(font_size);
                context.notify();
            }
        });
        context.notify();
    }

    /// 处理线程日志分析过滤输入区键盘编辑。
    pub(super) fn handle_thread_analysis_filter_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.handle_thread_analysis_filter_key_down(event, context);
        });
        context.notify();
    }

    /// 处理线程日志分析过滤输入区鼠标按下。
    pub(super) fn handle_thread_analysis_filter_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let focus_handle = self.main_view.update(context, |view, context| {
            view.start_thread_analysis_filter_mouse_selection(event, context);
            view.settings.thread_analysis_filter_focus.clone()
        });
        window.focus(&focus_handle);
        context.notify();
    }

    /// 拖动扩展线程日志分析过滤输入区的选择范围。
    pub(super) fn handle_thread_analysis_filter_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.update_thread_analysis_filter_mouse_selection(event.position, context);
        });
        context.notify();
    }

    /// 结束线程日志分析过滤输入区鼠标选择。
    pub(super) fn handle_thread_analysis_filter_mouse_up(&mut self, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            view.finish_thread_analysis_filter_mouse_selection(context);
        });
        context.notify();
    }

    /// 处理线程日志分析过滤的编辑/保存按钮。
    ///
    /// 返回值：
    /// - 进入编辑态时返回输入区焦点句柄，调用方负责把焦点交给多行输入区。
    /// - 保存时返回 `None`，避免保存按钮点击后再次抢回只读输入区焦点。
    pub(super) fn toggle_thread_analysis_filter_editing(
        &mut self,
        context: &mut Context<Self>,
    ) -> Option<gpui::FocusHandle> {
        let focus_handle = self.main_view.update(context, |view, context| {
            if view.settings.thread_analysis_filter_is_editing {
                view.save_thread_analysis_filter_edit(context);
                None
            } else {
                view.begin_thread_analysis_filter_edit(context);
                Some(view.settings.thread_analysis_filter_focus.clone())
            }
        });
        context.notify();
        focus_handle
    }

    /// 处理快搜关键字输入区键盘编辑。
    pub(super) fn handle_quick_search_keywords_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.handle_quick_search_keywords_key_down(event, context);
        });
        context.notify();
    }

    /// 处理快搜关键字输入区鼠标按下。
    pub(super) fn handle_quick_search_keywords_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let focus_handle = self.main_view.update(context, |view, context| {
            view.start_quick_search_keywords_mouse_selection(event, context);
            view.settings.quick_search_keywords_focus.clone()
        });
        window.focus(&focus_handle);
        context.notify();
    }

    /// 拖动扩展快搜关键字输入区选择范围。
    pub(super) fn handle_quick_search_keywords_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.update_quick_search_keywords_mouse_selection(event.position, context);
        });
        context.notify();
    }

    /// 结束快搜关键字输入区鼠标选择。
    pub(super) fn handle_quick_search_keywords_mouse_up(&mut self, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            view.finish_quick_search_keywords_mouse_selection(context);
        });
        context.notify();
    }

    /// 处理快搜关键字配置的编辑/保存按钮。
    pub(super) fn toggle_quick_search_keywords_editing(
        &mut self,
        context: &mut Context<Self>,
    ) -> Option<gpui::FocusHandle> {
        let focus_handle = self.main_view.update(context, |view, context| {
            if view.settings.quick_search_keywords_is_editing {
                view.save_quick_search_keywords_edit(context);
                None
            } else {
                view.begin_quick_search_keywords_edit(context);
                Some(view.settings.quick_search_keywords_focus.clone())
            }
        });
        context.notify();
        focus_handle
    }

    /// 处理模型配置输入框键盘编辑。
    pub(super) fn handle_model_config_input_key_down(
        &mut self,
        kind: ModelConfigInputKind,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.handle_model_config_input_key_down(kind, event, context);
        });
        context.notify();
    }

    /// 处理模型配置输入框鼠标按下并聚焦对应字段。
    pub(super) fn handle_model_config_input_mouse_down(
        &mut self,
        kind: ModelConfigInputKind,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let focus_handle = self.main_view.update(context, |view, context| {
            view.start_model_config_input_mouse_selection(kind, event, context);
            view.model_config_input_focus(kind)
        });
        window.focus(&focus_handle);
        context.notify();
    }

    /// 拖动扩展模型配置输入框选择范围。
    pub(super) fn handle_model_config_input_mouse_move(
        &mut self,
        kind: ModelConfigInputKind,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.update_model_config_input_mouse_selection(kind, event.position, context);
        });
        context.notify();
    }

    /// 结束模型配置输入框鼠标选择。
    pub(super) fn handle_model_config_input_mouse_up(
        &mut self,
        kind: ModelConfigInputKind,
        context: &mut Context<Self>,
    ) {
        self.main_view.update(context, |view, context| {
            view.finish_model_config_input_mouse_selection(kind, context);
        });
        context.notify();
    }

    /// 渲染左侧页签栏。
    fn render_tab_sidebar(
        &self,
        active_tab: SettingsTab,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .w(px(SETTINGS_TAB_SIDEBAR_WIDTH))
            .h_full()
            .p_2()
            .border_r_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .children(
                SettingsTab::all()
                    .iter()
                    .copied()
                    .map(|tab| self.render_tab_button(tab, active_tab, palette, context)),
            )
    }

    /// 渲染单个设置页签按钮。
    fn render_tab_button(
        &self,
        tab: SettingsTab,
        active_tab: SettingsTab,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = tab == active_tab;
        div()
            .id(SharedString::from(format!("settings-tab-{}", tab.label())))
            .flex()
            .items_center()
            .gap_2()
            .h(px(34.0))
            .px_2()
            .mb_1()
            .rounded(px(6.0))
            .text_sm()
            .font_weight(if selected {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            })
            .text_color(rgb(if selected {
                palette.accent
            } else {
                palette.muted_text
            }))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.panel
            }))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(MainView::render_lucide_icon(
                Some(tab.icon()),
                16.0,
                15.0,
                if selected {
                    palette.accent
                } else {
                    palette.muted_text
                },
            ))
            .child(tab.label())
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.select_tab(tab, context);
                }),
            )
    }

    /// 渲染设置内容区域。
    ///
    /// 业务意图：
    /// - 设置页内容来自主视图快照和当前窗口上下文，保持显式参数能避免渲染阶段重新借用主视图状态。
    fn render_content(
        &self,
        snapshot: SettingsContentSnapshot,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let SettingsContentSnapshot {
            active_tab,
            theme,
            log_viewer_font_size,
            quick_search_keywords_is_editing,
            quick_search_keywords_focus,
            thread_analysis_filter_is_editing,
            thread_analysis_filter_focus,
            palette,
        } = snapshot;
        match active_tab {
            SettingsTab::General => {
                self.render_general_tab(theme, log_viewer_font_size, palette, context)
            }
            SettingsTab::Log => self.render_log_tab(
                quick_search_keywords_is_editing,
                quick_search_keywords_focus,
                thread_analysis_filter_is_editing,
                thread_analysis_filter_focus,
                palette,
                context,
            ),
            SettingsTab::Model => self.render_model_tab(palette, context),
            SettingsTab::About => super::about_window::render_about_settings_tab(palette),
        }
    }

    /// 渲染通用页签。
    ///
    /// 业务意图：
    /// - 通用页签当前承载主题和日志显示字号，未来可继续加入语言等全局体验类配置。
    fn render_general_tab(
        &self,
        theme: ThemePreference,
        log_viewer_font_size: f32,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("settings-general-tab")
            .flex()
            .flex_col()
            .size_full()
            .p_4()
            .bg(rgb(palette.background))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .mb_3()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(MainView::render_lucide_icon(
                        Some(Icon::Palette),
                        16.0,
                        16.0,
                        palette.muted_text,
                    ))
                    .child("通用设置"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .child(self.render_theme_setting_row(theme, palette, context))
                    .child(div().h(px(1.0)).mx_3().bg(rgb(palette.border)))
                    .child(self.render_log_font_size_setting(
                        log_viewer_font_size,
                        palette,
                        context,
                    )),
            )
    }

    /// 渲染主题设置行。
    ///
    /// 业务意图：
    /// - 通用设置中的每个设置项都采用“左侧说明、右侧控件”的结构，避免主题选择占用三张大卡片而日志字号只有一行造成视觉不协调。
    /// - 主题选项使用分段按钮，可以保持信息密度，同时与工具栏、页签等现有轻量控件风格一致。
    fn render_theme_setting_row(
        &self,
        selected_theme: ThemePreference,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .min_h(px(70.0))
            .px_4()
            .py_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(MainView::render_lucide_icon(
                        Some(Icon::Palette),
                        18.0,
                        18.0,
                        palette.muted_text,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.text))
                                    .child("主题设置"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette.muted_text))
                                    .child("选择界面明暗外观"),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .p_1()
                    .rounded(px(7.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.input))
                    .children(ThemePreference::all().iter().copied().map(|option| {
                        self.render_theme_segment_option(option, selected_theme, palette, context)
                    })),
            )
    }

    /// 渲染主题分段选项按钮。
    ///
    /// 业务意图：
    /// - 分段按钮把三个互斥主题选项放在同一控件内，视觉上与字号步进器同属“右侧控件”。
    /// - 选中态使用应用强调色和选中背景，明暗主题下都沿用主程序的基础调色板。
    fn render_theme_segment_option(
        &self,
        option: ThemePreference,
        selected_theme: ThemePreference,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let selected = option == selected_theme;
        div()
            .id(SharedString::from(format!(
                "settings-theme-{}",
                option.label()
            )))
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .h(px(30.0))
            .px_2()
            .rounded(px(6.0))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.input
            }))
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .child(MainView::render_lucide_icon(
                Some(if selected { Icon::Check } else { option.icon() }),
                14.0,
                14.0,
                if selected {
                    palette.accent
                } else {
                    palette.muted_text
                },
            ))
            .child(
                div()
                    .text_xs()
                    .font_weight(if selected {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(rgb(if selected {
                        palette.accent
                    } else {
                        palette.text
                    }))
                    .child(option.label()),
            )
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.select_theme(option, context);
                }),
            )
    }

    /// 渲染日志显示字号设置。
    ///
    /// 业务意图：
    /// - 日志正文是长时间阅读区域，通用设置中提供字号微调，让用户在不改变布局密度的前提下改善可读性。
    /// - 当前只提供加减步进，不提供任意输入框，避免非法文本、过大字号和固定行高冲突。
    fn render_log_font_size_setting(
        &self,
        log_viewer_font_size: f32,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("settings-log-font-size")
            .flex()
            .items_center()
            .justify_between()
            .min_h(px(70.0))
            .px_4()
            .py_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(MainView::render_lucide_icon(
                        Some(Icon::Type),
                        18.0,
                        18.0,
                        palette.muted_text,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.text))
                                    .child("日志显示字号"),
                            )
                            .child(div().text_xs().text_color(rgb(palette.muted_text)).child(
                                format!(
                                    "范围 {}px - {}px，默认 {}px",
                                    LOG_VIEWER_MIN_FONT_SIZE.round(),
                                    LOG_VIEWER_MAX_FONT_SIZE.round(),
                                    LOG_VIEWER_DEFAULT_FONT_SIZE.round()
                                ),
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.render_log_font_size_button(
                        "-",
                        log_viewer_font_size > LOG_VIEWER_MIN_FONT_SIZE,
                        -LOG_VIEWER_FONT_SIZE_STEP,
                        palette,
                        context,
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(54.0))
                            .h(px(28.0))
                            .rounded(px(5.0))
                            .border_1()
                            .border_color(rgb(palette.border))
                            .bg(rgb(palette.surface))
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(format!("{}px", log_viewer_font_size.round())),
                    )
                    .child(self.render_log_font_size_button(
                        "+",
                        log_viewer_font_size < LOG_VIEWER_MAX_FONT_SIZE,
                        LOG_VIEWER_FONT_SIZE_STEP,
                        palette,
                        context,
                    )),
            )
    }

    /// 渲染日志字号调整按钮。
    ///
    /// 边界条件：
    /// - 到达最小或最大字号时按钮仍保留占位但降低透明度，避免右侧控件宽度跳动。
    fn render_log_font_size_button(
        &self,
        label: &'static str,
        enabled: bool,
        delta: f32,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "settings-log-font-size-{label}"
            )))
            .flex()
            .items_center()
            .justify_center()
            .w(px(28.0))
            .h(px(28.0))
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(if enabled {
                palette.text
            } else {
                palette.muted_text
            }))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
            })
            .when(!enabled, |button| button.opacity(0.55))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    if enabled {
                        view.adjust_log_viewer_font_size(delta, context);
                    }
                }),
            )
    }

    /// 渲染日志页签。
    ///
    /// 业务意图：
    /// - 日志页集中放置影响日志解析、分析和展示的偏好；当前先承载线程日志分析过滤配置。
    fn render_log_tab(
        &self,
        quick_search_keywords_is_editing: bool,
        quick_search_keywords_focus: gpui::FocusHandle,
        thread_analysis_filter_is_editing: bool,
        focus_handle: gpui::FocusHandle,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("settings-log-tab")
            .flex()
            .flex_col()
            .size_full()
            .p_4()
            // 日志页同时承载快搜配置和 300px 的线程过滤输入区，固定窗口高度下必须允许纵向滚动，
            // 否则底部输入框和保存按钮在 macOS/Windows 的标题栏高度差异下都可能被裁剪。
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .bg(rgb(palette.background))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .mb_3()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(MainView::render_lucide_icon(
                        Some(Icon::FileText),
                        16.0,
                        16.0,
                        palette.muted_text,
                    ))
                    .child("日志设置"),
            )
            .child(self.render_quick_search_keywords_setting(
                quick_search_keywords_is_editing,
                quick_search_keywords_focus,
                palette,
                context,
            ))
            .child(self.render_thread_analysis_filter_setting(
                thread_analysis_filter_is_editing,
                focus_handle,
                palette,
                context,
            ))
    }

    /// 渲染快搜关键字配置项。
    ///
    /// 业务意图：
    /// - 用户维护一组英文逗号分隔的排障关键字，搜索窗口“快搜”按钮会按任一关键字命中生成结果。
    /// - 配置项默认只读，和线程过滤配置保持一致，降低误修改跨会话搜索偏好的风险。
    fn render_quick_search_keywords_setting(
        &self,
        quick_search_keywords_is_editing: bool,
        focus_handle: gpui::FocusHandle,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let input_background = if quick_search_keywords_is_editing {
            palette.input
        } else {
            palette.panel
        };
        div()
            .id("settings-quick-search-keywords")
            .flex()
            .flex_col()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .p_4()
            .mb_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mb_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(MainView::render_lucide_icon(
                                Some(Icon::Zap),
                                18.0,
                                18.0,
                                palette.muted_text,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(palette.text))
                                            .child("快搜关键字配置"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(palette.muted_text))
                                            .child("英文逗号分隔多个关键字，快搜命中任意关键字"),
                                    ),
                            ),
                    )
                    .child(self.render_quick_search_keywords_edit_button(
                        quick_search_keywords_is_editing,
                        palette,
                        context,
                    )),
            )
            .child(
                div()
                    .id("settings-quick-search-keywords-input")
                    .relative()
                    .h(px(SEARCH_INPUT_HEIGHT))
                    .w_full()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(if quick_search_keywords_is_editing {
                        palette.accent
                    } else {
                        palette.border
                    }))
                    .bg(rgb(input_background))
                    .track_focus(&focus_handle)
                    .key_context("quick-search-keywords-input")
                    .on_key_down(context.listener(Self::handle_quick_search_keywords_key_down))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, event: &MouseDownEvent, window, context| {
                            view.handle_quick_search_keywords_mouse_down(event, window, context);
                        }),
                    )
                    .on_mouse_move(context.listener(
                        |view, event: &MouseMoveEvent, _window, context| {
                            view.handle_quick_search_keywords_mouse_move(event, context);
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.handle_quick_search_keywords_mouse_up(context);
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.handle_quick_search_keywords_mouse_up(context);
                        }),
                    )
                    .child(
                        div()
                            .id("settings-quick-search-keywords-text")
                            .absolute()
                            .left(px(10.0))
                            .right(px(10.0))
                            .top(px(5.0))
                            .bottom(px(5.0))
                            .overflow_hidden()
                            .text_sm()
                            .text_color(rgb(palette.text))
                            .child(QuickSearchKeywordsInputElement {
                                view: self.main_view.clone(),
                                focus_handle,
                                editable: quick_search_keywords_is_editing,
                                placeholder: "ERROR,Exception,Timeout",
                                palette,
                            }),
                    ),
            )
    }

    /// 渲染快搜关键字编辑/保存按钮。
    fn render_quick_search_keywords_edit_button(
        &self,
        is_editing: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (label, icon, text_color, background) = if is_editing {
            ("保存", Icon::Save, palette.on_accent, palette.accent)
        } else {
            ("编辑", Icon::Pencil, palette.text, palette.panel)
        };
        div()
            .id("settings-quick-search-keywords-edit")
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .h(px(28.0))
            .px_3()
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(background))
            .text_xs()
            .text_color(rgb(text_color))
            .cursor_pointer()
            .hover(move |button| {
                button.bg(rgb(if is_editing {
                    palette.accent_hover
                } else {
                    palette.hover
                }))
            })
            .child(MainView::render_lucide_icon(
                Some(icon),
                13.0,
                13.0,
                text_color,
            ))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, window, context| {
                    if let Some(focus_handle) = view.toggle_quick_search_keywords_editing(context) {
                        window.focus(&focus_handle);
                    }
                }),
            )
    }

    /// 渲染线程日志分析过滤设置项。
    ///
    /// 业务意图：
    /// - 用户点击编辑后可以粘贴一个或多个完整线程堆栈，保存后的下一次线程日志分析会按这些片段过滤无效线程。
    /// - 输入区使用等宽字体和滚动容器，便于核对 Java 堆栈中的类名、方法名和锁信息。
    fn render_thread_analysis_filter_setting(
        &self,
        thread_analysis_filter_is_editing: bool,
        focus_handle: gpui::FocusHandle,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let input_background = if thread_analysis_filter_is_editing {
            palette.input
        } else {
            palette.panel
        };
        div()
            .id("settings-thread-analysis-filter")
            .flex()
            .flex_col()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .p_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mb_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(MainView::render_lucide_icon(
                                Some(Icon::ListFilter),
                                18.0,
                                18.0,
                                palette.muted_text,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(palette.text))
                                            .child("线程日志分析过滤"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(palette.muted_text))
                                            .child("空行分隔多段堆栈，命中连续片段的线程不会显示"),
                                    ),
                            ),
                    )
                    .child(self.render_thread_analysis_filter_edit_button(
                        thread_analysis_filter_is_editing,
                        palette,
                        context,
                    )),
            )
            .child(
                div()
                    .id("settings-thread-analysis-filter-input")
                    .relative()
                    .h(px(THREAD_ANALYSIS_FILTER_TEXTAREA_HEIGHT))
                    .w_full()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(if thread_analysis_filter_is_editing {
                        palette.accent
                    } else {
                        palette.border
                    }))
                    .bg(rgb(input_background))
                    .track_focus(&focus_handle)
                    .key_context("thread-analysis-filter-input")
                    .on_key_down(context.listener(Self::handle_thread_analysis_filter_key_down))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, event: &MouseDownEvent, window, context| {
                            view.handle_thread_analysis_filter_mouse_down(event, window, context);
                        }),
                    )
                    .on_mouse_move(context.listener(
                        |view, event: &MouseMoveEvent, _window, context| {
                            view.handle_thread_analysis_filter_mouse_move(event, context);
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.handle_thread_analysis_filter_mouse_up(context);
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.handle_thread_analysis_filter_mouse_up(context);
                        }),
                    )
                    .child(
                        div()
                            .id("settings-thread-analysis-filter-scroll")
                            .size_full()
                            .px_2()
                            .py_2()
                            .overflow_y_scroll()
                            .scrollbar_width(px(6.0))
                            .text_size(px(12.0))
                            .line_height(px(THREAD_ANALYSIS_FILTER_TEXT_LINE_HEIGHT))
                            .text_color(rgb(palette.text))
                            .font_family(LOG_VIEWER_FONT_FAMILY)
                            .child(ThreadAnalysisFilterTextAreaElement {
                                view: self.main_view.clone(),
                                focus_handle,
                                editable: thread_analysis_filter_is_editing,
                                placeholder: "粘贴需要过滤的线程堆栈；多段堆栈之间用空行分隔",
                                palette,
                            }),
                    ),
            )
    }

    /// 渲染线程分析过滤编辑/保存按钮。
    fn render_thread_analysis_filter_edit_button(
        &self,
        is_editing: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (label, icon, text_color, background) = if is_editing {
            ("保存", Icon::Save, palette.on_accent, palette.accent)
        } else {
            ("编辑", Icon::Pencil, palette.text, palette.panel)
        };
        div()
            .id("settings-thread-analysis-filter-edit")
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .h(px(28.0))
            .px_3()
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(background))
            .text_xs()
            .text_color(rgb(text_color))
            .cursor_pointer()
            .hover(move |button| {
                button.bg(rgb(if is_editing {
                    palette.accent_hover
                } else {
                    palette.hover
                }))
            })
            .child(MainView::render_lucide_icon(
                Some(icon),
                13.0,
                13.0,
                text_color,
            ))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, window, context| {
                    if let Some(focus_handle) = view.toggle_thread_analysis_filter_editing(context)
                    {
                        window.focus(&focus_handle);
                    }
                }),
            )
    }

    /// 渲染模型页签。
    ///
    /// 业务意图：
    /// - 模型页提供 OpenAI 兼容模型配置管理，用户可以维护多条配置、测试当前表单并设置默认模型。
    /// - 列表和表单并排展示，避免多条配置时频繁滚动；窗口较小时外层允许纵向滚动，保证按钮不被裁剪。
    fn render_model_tab(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (
            profiles,
            default_profile_id,
            selected_profile_id,
            form_profile_id,
            name,
            base_url,
            model,
            api_key_visible,
            test_status,
        ) = {
            let main_view = self.main_view.read(context);
            (
                main_view.model_config.model_config_profiles.clone(),
                main_view
                    .model_config
                    .model_config_default_profile_id
                    .clone(),
                main_view
                    .model_config
                    .model_config_selected_profile_id
                    .clone(),
                main_view.model_config.model_config_form_profile_id.clone(),
                main_view.model_config.model_config_name_input.text.clone(),
                main_view
                    .model_config
                    .model_config_base_url_input
                    .text
                    .clone(),
                main_view.model_config.model_config_model_input.text.clone(),
                main_view.model_config.model_config_api_key_visible,
                main_view.model_config.model_test_status.clone(),
            )
        };
        let can_save_or_test = validate_model_profile_fields(&name, &base_url, &model).is_ok();
        let can_test = can_save_or_test && !test_status.is_testing();
        let can_delete = form_profile_id.is_some();
        let can_set_default = form_profile_id
            .as_ref()
            .is_some_and(|profile_id| default_profile_id.as_ref() != Some(profile_id));

        div()
            .id("settings-model-tab")
            .flex()
            .flex_col()
            .size_full()
            .p_4()
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .bg(rgb(palette.background))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mb_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(MainView::render_lucide_icon(
                                Some(Icon::MonitorCog),
                                16.0,
                                16.0,
                                palette.muted_text,
                            ))
                            .child("模型设置"),
                    )
                    .child(self.render_model_icon_button(
                        ModelIconButtonRequest {
                            id: "settings-model-new",
                            label: "新增",
                            icon: Icon::Plus,
                            enabled: true,
                            primary: false,
                            palette,
                        },
                        context,
                        |view, context| view.begin_new_model_profile(context),
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap_4()
                    .min_h(px(390.0))
                    .child(self.render_model_profile_list(
                        profiles,
                        default_profile_id.clone(),
                        selected_profile_id,
                        palette,
                        context,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(rgb(palette.border))
                            .bg(rgb(palette.surface))
                            .p_4()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .mb_4()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(palette.text))
                                            .child(MainView::render_lucide_icon(
                                                Some(Icon::Pencil),
                                                15.0,
                                                15.0,
                                                palette.muted_text,
                                            ))
                                            .child("配置详情"),
                                    )
                                    .when(
                                        form_profile_id.as_ref().is_some_and(|profile_id| {
                                            default_profile_id.as_ref() == Some(profile_id)
                                        }),
                                        |row| {
                                            row.child(
                                                div()
                                                    .px_2()
                                                    .py_1()
                                                    .rounded(px(999.0))
                                                    .bg(rgb(palette.selected))
                                                    .text_xs()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(palette.accent))
                                                    .child("默认"),
                                            )
                                        },
                                    ),
                            )
                            .child(self.render_model_text_field(
                                ModelConfigInputKind::Name,
                                "配置名称",
                                "OpenAI / 本地 vLLM",
                                false,
                                palette,
                                context,
                            ))
                            .child(self.render_model_text_field(
                                ModelConfigInputKind::BaseUrl,
                                "Base URL",
                                "https://api.openai.com/v1",
                                false,
                                palette,
                                context,
                            ))
                            .child(self.render_model_text_field(
                                ModelConfigInputKind::ApiKey,
                                "API Key",
                                "可为空，兼容本地模型服务",
                                true,
                                palette,
                                context,
                            ))
                            .child(self.render_model_text_field(
                                ModelConfigInputKind::Model,
                                "模型 ID",
                                "gpt-4.1-mini / qwen2.5:7b",
                                false,
                                palette,
                                context,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .mt_2()
                                    .gap_2()
                                    .child(self.render_model_icon_button(
                                        ModelIconButtonRequest {
                                            id: "settings-model-save",
                                            label: "保存",
                                            icon: Icon::Save,
                                            enabled: can_save_or_test,
                                            primary: true,
                                            palette,
                                        },
                                        context,
                                        |view, context| view.save_current_model_profile(context),
                                    ))
                                    .child(self.render_model_icon_button(
                                        ModelIconButtonRequest {
                                            id: "settings-model-delete",
                                            label: "删除",
                                            icon: Icon::Trash2,
                                            enabled: can_delete,
                                            primary: false,
                                            palette,
                                        },
                                        context,
                                        |view, context| view.delete_current_model_profile(context),
                                    ))
                                    .child(self.render_model_icon_button(
                                        ModelIconButtonRequest {
                                            id: "settings-model-default",
                                            label: "设为默认",
                                            icon: Icon::Check,
                                            enabled: can_set_default,
                                            primary: false,
                                            palette,
                                        },
                                        context,
                                        |view, context| {
                                            view.set_current_model_profile_default(context)
                                        },
                                    ))
                                    .child(self.render_model_icon_button(
                                        ModelIconButtonRequest {
                                            id: "settings-model-test",
                                            label: if test_status.is_testing() {
                                                "测试中"
                                            } else {
                                                "测试模型"
                                            },
                                            icon: Icon::TestTube,
                                            enabled: can_test,
                                            primary: false,
                                            palette,
                                        },
                                        context,
                                        |view, context| view.start_model_profile_test(context),
                                    )),
                            )
                            .child(self.render_model_test_status(test_status, palette))
                            .child(
                                div()
                                    .mt_2()
                                    .text_xs()
                                    .text_color(rgb(palette.muted_text))
                                    .child(if api_key_visible {
                                        "API Key 当前明文显示"
                                    } else {
                                        "API Key 默认掩码显示，配置保存到应用配置目录"
                                    }),
                            ),
                    ),
            )
    }

    /// 渲染模型配置列表。
    fn render_model_profile_list(
        &self,
        profiles: Vec<ModelProfile>,
        default_profile_id: Option<String>,
        selected_profile_id: Option<String>,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("settings-model-profile-list")
            .flex()
            .flex_col()
            .w(px(200.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .p_3()
            .child(
                div()
                    .mb_2()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted_text))
                    .child("已保存配置"),
            )
            .when(profiles.is_empty(), |list| {
                list.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .flex_1()
                        .min_h(px(240.0))
                        .text_center()
                        .text_xs()
                        .text_color(rgb(palette.muted_text))
                        .child(MainView::render_lucide_icon(
                            Some(Icon::MonitorCog),
                            24.0,
                            22.0,
                            palette.muted_text,
                        ))
                        .child("暂无模型配置"),
                )
            })
            .children(profiles.into_iter().map(|profile| {
                self.render_model_profile_list_item(
                    profile,
                    default_profile_id.clone(),
                    selected_profile_id.clone(),
                    palette,
                    context,
                )
            }))
    }

    /// 渲染单个模型配置列表项。
    fn render_model_profile_list_item(
        &self,
        profile: ModelProfile,
        default_profile_id: Option<String>,
        selected_profile_id: Option<String>,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let profile_id = profile.id.clone();
        let selected = selected_profile_id.as_ref() == Some(&profile.id);
        let is_default = default_profile_id.as_ref() == Some(&profile.id);
        div()
            .id(SharedString::from(format!(
                "settings-model-profile-{}",
                profile.id
            )))
            .flex()
            .flex_col()
            .gap_1()
            .w_full()
            .mb_2()
            .p_2()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(if selected {
                palette.accent
            } else {
                palette.border
            }))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.panel
            }))
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(if selected {
                                palette.accent
                            } else {
                                palette.text
                            }))
                            .overflow_hidden()
                            .child(profile.name),
                    )
                    .when(is_default, |row| {
                        row.child(
                            div()
                                .px_1()
                                .rounded(px(999.0))
                                .bg(rgb(palette.selected))
                                .text_xs()
                                .text_color(rgb(palette.accent))
                                .child("默认"),
                        )
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted_text))
                    .overflow_hidden()
                    .child(profile.model),
            )
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.main_view.update(context, |main_view, context| {
                        main_view.select_model_profile(&profile_id, context);
                    });
                    context.notify();
                }),
            )
    }

    /// 渲染模型配置单行字段。
    fn render_model_text_field(
        &self,
        kind: ModelConfigInputKind,
        label: &'static str,
        placeholder: &'static str,
        secret: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (focus_handle, api_key_visible) = {
            let main_view = self.main_view.read(context);
            (
                main_view.model_config_input_focus(kind),
                main_view.model_config.model_config_api_key_visible,
            )
        };
        div()
            .id(SharedString::from(format!("settings-model-field-{label}")))
            .flex()
            .flex_col()
            .gap_1()
            .mb_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.muted_text))
                            .child(label),
                    )
                    .when(secret, |row| {
                        row.child(self.render_model_api_key_visibility_button(
                            api_key_visible,
                            palette,
                            context,
                        ))
                    }),
            )
            .child(
                div()
                    .relative()
                    .h(px(SEARCH_INPUT_HEIGHT))
                    .w_full()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.input))
                    .track_focus(&focus_handle)
                    .key_context("model-config-input")
                    .on_key_down(context.listener(
                        move |view, event: &KeyDownEvent, window, context| {
                            view.handle_model_config_input_key_down(kind, event, window, context);
                        },
                    ))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(move |view, event: &MouseDownEvent, window, context| {
                            view.handle_model_config_input_mouse_down(kind, event, window, context);
                        }),
                    )
                    .on_mouse_move(context.listener(
                        move |view, event: &MouseMoveEvent, _window, context| {
                            view.handle_model_config_input_mouse_move(kind, event, context);
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        context.listener(move |view, _event: &MouseUpEvent, _window, context| {
                            view.handle_model_config_input_mouse_up(kind, context);
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        context.listener(move |view, _event: &MouseUpEvent, _window, context| {
                            view.handle_model_config_input_mouse_up(kind, context);
                        }),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(10.0))
                            .right(px(10.0))
                            .top(px(5.0))
                            .bottom(px(5.0))
                            .overflow_hidden()
                            .text_sm()
                            .text_color(rgb(palette.text))
                            .child(ModelConfigInputElement {
                                view: self.main_view.clone(),
                                kind,
                                focus_handle,
                                placeholder,
                                palette,
                            }),
                    ),
            )
    }

    /// 渲染 API Key 显示/隐藏按钮。
    fn render_model_api_key_visibility_button(
        &self,
        api_key_visible: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("settings-model-api-key-visibility")
            .flex()
            .items_center()
            .gap_1()
            .h(px(24.0))
            .px_2()
            .rounded(px(5.0))
            .text_xs()
            .text_color(rgb(palette.muted_text))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(MainView::render_lucide_icon(
                Some(if api_key_visible {
                    Icon::EyeOff
                } else {
                    Icon::Eye
                }),
                13.0,
                13.0,
                palette.muted_text,
            ))
            .child(if api_key_visible { "隐藏" } else { "显示" })
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.main_view.update(context, |main_view, context| {
                        main_view.toggle_model_api_key_visibility(context);
                    });
                    context.notify();
                }),
            )
    }

    /// 渲染模型页通用按钮。
    ///
    /// 业务意图：
    /// - 模型配置按钮需要同时携带展示样式和回写主视图的动作，保持一个小型渲染 helper 可以减少四个按钮重复。
    fn render_model_icon_button<F>(
        &self,
        request: ModelIconButtonRequest,
        context: &mut Context<Self>,
        action: F,
    ) -> gpui::Stateful<gpui::Div>
    where
        F: Fn(&mut MainView, &mut Context<MainView>) + 'static,
    {
        let ModelIconButtonRequest {
            id,
            label,
            icon,
            enabled,
            primary,
            palette,
        } = request;
        let text_color = if primary && enabled {
            palette.on_accent
        } else if enabled {
            palette.text
        } else {
            palette.muted_text
        };
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .h(px(28.0))
            .px_3()
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(if primary && enabled {
                palette.accent
            } else {
                palette.panel
            }))
            .text_xs()
            .text_color(rgb(text_color))
            .when(enabled, |button| {
                button.cursor_pointer().hover(move |button| {
                    button.bg(rgb(if primary {
                        palette.accent_hover
                    } else {
                        palette.hover
                    }))
                })
            })
            .when(!enabled, |button| button.opacity(0.55))
            .child(MainView::render_lucide_icon(
                Some(icon),
                13.0,
                13.0,
                text_color,
            ))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    if enabled {
                        view.main_view.update(context, |main_view, context| {
                            action(main_view, context);
                        });
                        context.notify();
                    }
                }),
            )
    }

    /// 渲染模型测试状态。
    fn render_model_test_status(
        &self,
        test_status: ModelTestStatus,
        palette: AppThemePalette,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(message) = test_status.message() else {
            return div().id("settings-model-test-status-empty").h(px(24.0));
        };
        let color = match test_status {
            ModelTestStatus::Success(_) => palette.accent,
            ModelTestStatus::Failed(_) => palette.error,
            ModelTestStatus::Testing { .. } | ModelTestStatus::Idle => palette.muted_text,
        };
        div()
            .id("settings-model-test-status")
            .mt_3()
            .min_h(px(24.0))
            .text_xs()
            .text_color(rgb(color))
            .child(message.to_string())
    }
}

impl Render for SettingsWindowView {
    /// 渲染独立设置窗口。
    ///
    /// 业务意图：
    /// - 设置窗口内容由左侧页签和右侧内容组成，根节点填满独立窗口，避免系统标题栏下方出现未绘制区域。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let (
            active_tab,
            theme,
            log_viewer_font_size,
            quick_search_keywords_is_editing,
            quick_search_keywords_focus,
            thread_analysis_filter_is_editing,
            thread_analysis_filter_focus,
            palette,
        ) = {
            let main_view = self.main_view.read(context);
            (
                main_view.settings.settings_active_tab,
                main_view.settings.theme_preference,
                main_view.settings.log_viewer_font_size,
                main_view.settings.quick_search_keywords_is_editing,
                main_view.settings.quick_search_keywords_focus.clone(),
                main_view.settings.thread_analysis_filter_is_editing,
                main_view.settings.thread_analysis_filter_focus.clone(),
                main_view.palette(),
            )
        };

        div()
            .id("settings-window")
            .flex()
            .size_full()
            .bg(rgb(palette.background))
            .child(self.render_tab_sidebar(active_tab, palette, context))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(self.render_content(
                        SettingsContentSnapshot {
                            active_tab,
                            theme,
                            log_viewer_font_size,
                            quick_search_keywords_is_editing,
                            quick_search_keywords_focus,
                            thread_analysis_filter_is_editing,
                            thread_analysis_filter_focus,
                            palette,
                        },
                        context,
                    )),
            )
    }
}

/// 快搜关键字输入框的预绘制结果。
struct QuickSearchKeywordsInputPrepaint {
    /// 当前帧单行字形布局。
    line: ShapedLine,
    /// 当前选择范围对应的高亮矩形。
    selection: Option<PaintQuad>,
    /// 当前插入光标矩形。
    cursor: Option<PaintQuad>,
}

/// 快搜关键字单行输入元素。
///
/// 业务意图：
/// - 设置页需要一个支持中文 IME、复制粘贴和精确鼠标选区的单行输入框，GPUI 当前版本没有可直接复用的文本输入控件。
/// - 元素只负责绘制和注册平台输入协议，真实状态仍保存在 `MainView` 中，便于快搜按钮读取同一份配置。
struct QuickSearchKeywordsInputElement {
    /// 主视图实体，用于读取和写回快搜关键字输入状态。
    view: Entity<MainView>,
    /// 输入区焦点句柄。
    focus_handle: gpui::FocusHandle,
    /// 当前是否允许平台输入法和键盘写入文本。
    editable: bool,
    /// 输入为空时显示的占位文案。
    placeholder: &'static str,
    /// 当前主题调色板。
    palette: AppThemePalette,
}

impl IntoElement for QuickSearchKeywordsInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for QuickSearchKeywordsInputElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<QuickSearchKeywordsInputPrepaint>;

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
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], context), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        context: &mut App,
    ) -> Self::PrepaintState {
        let (text, selection_range, marked_range, cursor_visible_by_activity) = {
            let view = self.view.read(context);
            let (text, selection_range, marked_range) = view.quick_search_keywords_text_snapshot();
            (
                text,
                selection_range,
                marked_range,
                view.search_text_cursor_visible(),
            )
        };
        let style = window.text_style();
        let display_text = if text.is_empty() {
            SharedString::from(self.placeholder)
        } else {
            SharedString::from(text.clone())
        };
        let text_color = if text.is_empty() {
            rgb(self.palette.muted_text).into()
        } else {
            style.color
        };
        let base_run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if !text.is_empty() {
            if let Some(marked_range) = marked_range {
                vec![
                    TextRun {
                        len: marked_range.start,
                        ..base_run.clone()
                    },
                    TextRun {
                        len: marked_range.end.saturating_sub(marked_range.start),
                        underline: Some(UnderlineStyle {
                            color: Some(base_run.color),
                            thickness: px(1.0),
                            wavy: false,
                        }),
                        ..base_run.clone()
                    },
                    TextRun {
                        len: display_text.len().saturating_sub(marked_range.end),
                        ..base_run
                    },
                ]
                .into_iter()
                .filter(|run| run.len > 0)
                .collect()
            } else {
                vec![base_run]
            }
        } else {
            vec![base_run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);
        let focused = self.focus_handle.is_focused(window);
        let selection_range = MainView::clamp_search_text_range(&text, selection_range);
        let has_selection =
            focused && !text.is_empty() && selection_range.start < selection_range.end;
        let cursor_index = selection_range.end;
        let selection = has_selection.then(|| {
            let mut selection_color = rgb(self.palette.accent);
            selection_color.a = 0.32;
            fill(
                Bounds::from_corners(
                    point(
                        bounds.left() + line.x_for_index(selection_range.start),
                        bounds.top(),
                    ),
                    point(
                        bounds.left() + line.x_for_index(selection_range.end),
                        bounds.bottom(),
                    ),
                ),
                selection_color,
            )
        });
        let cursor_visible =
            focused && self.editable && !has_selection && cursor_visible_by_activity;
        let cursor = cursor_visible.then(|| {
            fill(
                Bounds::new(
                    point(bounds.left() + line.x_for_index(cursor_index), bounds.top()),
                    size(px(1.5), bounds.bottom() - bounds.top()),
                ),
                rgb(self.palette.accent),
            )
        });

        Some(QuickSearchKeywordsInputPrepaint {
            line,
            selection,
            cursor,
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        context: &mut App,
    ) {
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            context,
        );
        let Some(prepaint) = prepaint.take() else {
            return;
        };
        if let Some(selection) = prepaint.selection {
            window.paint_quad(selection);
        }
        prepaint
            .line
            .paint(bounds.origin, window.line_height(), window, context)
            .ok();
        if let Some(cursor) = prepaint.cursor {
            window.paint_quad(cursor);
        }
        if self.focus_handle.is_focused(window) && self.editable {
            window.request_animation_frame();
        }
        self.view.update(context, |view, _context| {
            view.store_quick_search_keywords_layout(prepaint.line, bounds);
        });
    }
}

/// 模型配置输入框的预绘制结果。
struct ModelConfigInputPrepaint {
    /// 当前帧单行字形布局。
    line: ShapedLine,
    /// 当前选择范围对应的高亮矩形。
    selection: Option<PaintQuad>,
    /// 当前插入光标矩形。
    cursor: Option<PaintQuad>,
}

/// 模型配置单行输入元素。
///
/// 业务意图：
/// - 设置-模型页需要四个支持 IME 的自绘输入框，复用快搜单行输入的绘制和平台输入协议方案。
/// - 元素只负责绘制可见文本、选区和光标；真实字段值、掩码状态和保存逻辑仍由 `MainView` 统一管理。
struct ModelConfigInputElement {
    /// 主视图实体，用于读取和写回模型配置输入状态。
    view: Entity<MainView>,
    /// 当前字段类型。
    kind: ModelConfigInputKind,
    /// 输入区焦点句柄。
    focus_handle: gpui::FocusHandle,
    /// 输入为空时显示的占位文案。
    placeholder: &'static str,
    /// 当前主题调色板。
    palette: AppThemePalette,
}

impl IntoElement for ModelConfigInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ModelConfigInputElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<ModelConfigInputPrepaint>;

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
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], context), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        context: &mut App,
    ) -> Self::PrepaintState {
        let (text, display_text, selection_range, marked_range, cursor_visible_by_activity) = {
            let view = self.view.read(context);
            let (text, display_text, selection_range, marked_range) =
                view.model_config_input_text_snapshot(self.kind);
            (
                text,
                display_text,
                selection_range,
                marked_range,
                view.search_text_cursor_visible(),
            )
        };
        let style = window.text_style();
        let rendered_text = if text.is_empty() {
            SharedString::from(self.placeholder)
        } else {
            SharedString::from(display_text)
        };
        let text_color = if text.is_empty() {
            rgb(self.palette.muted_text).into()
        } else {
            style.color
        };
        let base_run = TextRun {
            len: rendered_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if !text.is_empty() {
            if let Some(marked_range) = marked_range {
                vec![
                    TextRun {
                        len: marked_range.start,
                        ..base_run.clone()
                    },
                    TextRun {
                        len: marked_range.end.saturating_sub(marked_range.start),
                        underline: Some(UnderlineStyle {
                            color: Some(base_run.color),
                            thickness: px(1.0),
                            wavy: false,
                        }),
                        ..base_run.clone()
                    },
                    TextRun {
                        len: rendered_text.len().saturating_sub(marked_range.end),
                        ..base_run
                    },
                ]
                .into_iter()
                .filter(|run| run.len > 0)
                .collect()
            } else {
                vec![base_run]
            }
        } else {
            vec![base_run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(rendered_text, font_size, &runs, None);
        let focused = self.focus_handle.is_focused(window);
        let selection_range = MainView::clamp_search_text_range(&text, selection_range);
        let has_selection =
            focused && !text.is_empty() && selection_range.start < selection_range.end;
        let cursor_index = selection_range.end;
        let selection = has_selection.then(|| {
            let mut selection_color = rgb(self.palette.accent);
            selection_color.a = 0.32;
            fill(
                Bounds::from_corners(
                    point(
                        bounds.left() + line.x_for_index(selection_range.start),
                        bounds.top(),
                    ),
                    point(
                        bounds.left() + line.x_for_index(selection_range.end),
                        bounds.bottom(),
                    ),
                ),
                selection_color,
            )
        });
        let cursor = (focused && !has_selection && cursor_visible_by_activity).then(|| {
            fill(
                Bounds::new(
                    point(bounds.left() + line.x_for_index(cursor_index), bounds.top()),
                    size(px(1.5), bounds.bottom() - bounds.top()),
                ),
                rgb(self.palette.accent),
            )
        });

        Some(ModelConfigInputPrepaint {
            line,
            selection,
            cursor,
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        context: &mut App,
    ) {
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            context,
        );
        let Some(prepaint) = prepaint.take() else {
            return;
        };
        if let Some(selection) = prepaint.selection {
            window.paint_quad(selection);
        }
        prepaint
            .line
            .paint(bounds.origin, window.line_height(), window, context)
            .ok();
        if let Some(cursor) = prepaint.cursor {
            window.paint_quad(cursor);
        }
        if self.focus_handle.is_focused(window) {
            window.request_animation_frame();
        }
        self.view.update(context, |view, _context| {
            view.store_model_config_input_layout(self.kind, prepaint.line, bounds);
        });
    }
}

/// 线程日志分析过滤输入区单行绘制状态。
struct ThreadAnalysisFilterTextPaintLine {
    /// 当前行对应的原始文本 UTF-8 字节范围。
    byte_range: Range<usize>,
    /// 当前行绘制边界。
    bounds: Bounds<Pixels>,
    /// 当前行字形布局。
    line: ShapedLine,
}

/// 线程日志分析过滤输入区绘制状态。
struct ThreadAnalysisFilterTextAreaPrepaint {
    /// 当前帧需要绘制的所有文本行。
    lines: Vec<ThreadAnalysisFilterTextPaintLine>,
    /// 当前选择范围对应的高亮矩形。
    selections: Vec<PaintQuad>,
    /// 当前光标矩形。
    cursor: Option<PaintQuad>,
}

/// 线程日志分析过滤多行输入元素。
///
/// 业务意图：
/// - GPUI 0.2.2 没有现成多行文本框；该元素复用搜索输入框的自定义元素方案，注册平台输入协议并手动绘制文本、选区和光标。
/// - 输入内容可能是完整 Java 堆栈，必须保留换行并使用等宽字体，方便用户核对过滤片段。
///
/// 边界条件：
/// - 当前不做自动换行，长堆栈行横向超出时由输入区裁切；过滤匹配仍使用完整原文，不受显示裁切影响。
struct ThreadAnalysisFilterTextAreaElement {
    /// 主视图实体，用于读取和写回过滤输入状态。
    view: Entity<MainView>,
    /// 过滤输入区焦点句柄。
    focus_handle: gpui::FocusHandle,
    /// 当前是否允许平台输入法和键盘写入文本。
    ///
    /// 业务意图：
    /// - 只读态仍要绘制文本和选区，但不显示插入光标，避免用户误以为内容已经可编辑。
    editable: bool,
    /// 输入为空时显示的占位文案。
    placeholder: &'static str,
    /// 当前主题调色板。
    palette: AppThemePalette,
}

impl IntoElement for ThreadAnalysisFilterTextAreaElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ThreadAnalysisFilterTextAreaElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<ThreadAnalysisFilterTextAreaPrepaint>;

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
        let line_count = self
            .view
            .read(context)
            .thread_analysis_filter_visual_line_count();
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = px(
            (line_count as f32 * THREAD_ANALYSIS_FILTER_TEXT_LINE_HEIGHT)
                .max(THREAD_ANALYSIS_FILTER_TEXT_LINE_HEIGHT),
        )
        .into();
        (window.request_layout(style, [], context), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        context: &mut App,
    ) -> Self::PrepaintState {
        let (text, selection_range, marked_range, cursor_visible_by_activity) = {
            let view = self.view.read(context);
            let (text, selection_range, marked_range) = view.thread_analysis_filter_text_snapshot();
            (
                text,
                selection_range,
                marked_range,
                view.search_text_cursor_visible(),
            )
        };
        let focused = self.focus_handle.is_focused(window);
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = px(THREAD_ANALYSIS_FILTER_TEXT_LINE_HEIGHT);
        let line_ranges = MainView::thread_analysis_filter_line_ranges(&text);
        let display_ranges = if text.is_empty() {
            std::iter::once(0..0).collect::<Vec<_>>()
        } else {
            line_ranges
        };

        let mut lines = Vec::new();
        let mut selections = Vec::new();
        let mut cursor = None;
        let selection_range = MainView::clamp_search_text_range(&text, selection_range);
        let has_selection = focused && selection_range.start < selection_range.end;
        let cursor_index = selection_range.end;

        for (line_index, byte_range) in display_ranges.into_iter().enumerate() {
            let is_placeholder = text.is_empty();
            let display_text = if is_placeholder {
                SharedString::from(self.placeholder)
            } else {
                SharedString::from(text[byte_range.clone()].to_string())
            };
            let text_color = if is_placeholder {
                rgb(self.palette.muted_text).into()
            } else {
                style.color
            };
            let base_run = TextRun {
                len: display_text.len(),
                font: style.font(),
                color: text_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let runs = if !is_placeholder {
                if let Some(marked_range) = marked_range.clone() {
                    let local_marked_start = marked_range
                        .start
                        .saturating_sub(byte_range.start)
                        .min(byte_range.len());
                    let local_marked_end = marked_range
                        .end
                        .saturating_sub(byte_range.start)
                        .min(byte_range.len());
                    vec![
                        TextRun {
                            len: local_marked_start,
                            ..base_run.clone()
                        },
                        TextRun {
                            len: local_marked_end.saturating_sub(local_marked_start),
                            underline: Some(UnderlineStyle {
                                color: Some(base_run.color),
                                thickness: px(1.0),
                                wavy: false,
                            }),
                            ..base_run.clone()
                        },
                        TextRun {
                            len: display_text.len().saturating_sub(local_marked_end),
                            ..base_run
                        },
                    ]
                    .into_iter()
                    .filter(|run| run.len > 0)
                    .collect()
                } else {
                    vec![base_run]
                }
            } else {
                vec![base_run]
            };
            let line = window
                .text_system()
                .shape_line(display_text, font_size, &runs, None);
            let line_top =
                bounds.top() + px(line_index as f32 * THREAD_ANALYSIS_FILTER_TEXT_LINE_HEIGHT);
            let line_bounds = Bounds::new(
                point(bounds.left(), line_top),
                size(bounds.right() - bounds.left(), line_height),
            );

            if has_selection && !is_placeholder {
                let start = selection_range
                    .start
                    .max(byte_range.start)
                    .min(byte_range.end);
                let end = selection_range
                    .end
                    .max(byte_range.start)
                    .min(byte_range.end);
                if start < end {
                    let mut selection_color = rgb(self.palette.accent);
                    selection_color.a = 0.32;
                    selections.push(fill(
                        Bounds::from_corners(
                            point(
                                line_bounds.left() + line.x_for_index(start - byte_range.start),
                                line_bounds.top(),
                            ),
                            point(
                                line_bounds.left() + line.x_for_index(end - byte_range.start),
                                line_bounds.bottom(),
                            ),
                        ),
                        selection_color,
                    ));
                }
            }

            if focused
                && self.editable
                && !has_selection
                && cursor.is_none()
                && cursor_visible_by_activity
                && cursor_index >= byte_range.start
                && cursor_index <= byte_range.end
            {
                cursor = Some(fill(
                    Bounds::new(
                        point(
                            line_bounds.left() + line.x_for_index(cursor_index - byte_range.start),
                            line_bounds.top(),
                        ),
                        size(px(1.5), line_bounds.bottom() - line_bounds.top()),
                    ),
                    rgb(self.palette.accent),
                ));
            }

            lines.push(ThreadAnalysisFilterTextPaintLine {
                byte_range,
                bounds: line_bounds,
                line,
            });
        }

        Some(ThreadAnalysisFilterTextAreaPrepaint {
            lines,
            selections,
            cursor,
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        context: &mut App,
    ) {
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            context,
        );
        let Some(prepaint) = prepaint.take() else {
            return;
        };
        for selection in prepaint.selections {
            window.paint_quad(selection);
        }

        let mut layouts = Vec::new();
        for paint_line in prepaint.lines {
            paint_line
                .line
                .paint(
                    paint_line.bounds.origin,
                    paint_line.bounds.bottom() - paint_line.bounds.top(),
                    window,
                    context,
                )
                .ok();
            layouts.push(ThreadAnalysisFilterLineLayout {
                byte_range: paint_line.byte_range,
                line: paint_line.line,
                bounds: paint_line.bounds,
            });
        }
        if let Some(cursor) = prepaint.cursor {
            window.paint_quad(cursor);
        }
        if self.focus_handle.is_focused(window) && self.editable {
            window.request_animation_frame();
        }
        self.view.update(context, |view, _context| {
            view.store_thread_analysis_filter_text_layouts(layouts, bounds);
        });
    }
}
