// 设置独立窗口实现。
//
// 业务意图：
// - 该文件集中维护“通用/模型”页签、主题选择和日志显示字号设置，避免设置 UI 继续堆在应用根文件里。
// - 当前作为 `app` 的子模块，通过显式 `pub(super)` 接口更新 `MainView` 的会话状态和持久化配置。
//
// 边界条件：
// - 本阶段只做物理拆分，不改变设置项、配置格式、按钮行为或窗口尺寸。

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
            view.settings_active_tab = tab;
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
            view.theme_preference = theme;
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
            let target = view.log_viewer_font_size + delta;
            if let Some(font_size) = normalize_log_viewer_font_size(target) {
                view.log_viewer_font_size = font_size;
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
            view.thread_analysis_filter_focus.clone()
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

    /// 清空线程日志分析过滤配置。
    pub(super) fn clear_thread_analysis_filter(&mut self, context: &mut Context<Self>) {
        self.main_view.update(context, |view, context| {
            view.clear_thread_analysis_filter_text(context);
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
    fn render_content(
        &self,
        active_tab: SettingsTab,
        theme: ThemePreference,
        log_viewer_font_size: f32,
        thread_analysis_filter_text: String,
        thread_analysis_filter_focus: gpui::FocusHandle,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        match active_tab {
            SettingsTab::General => {
                self.render_general_tab(theme, log_viewer_font_size, palette, context)
            }
            SettingsTab::Log => self.render_log_tab(
                thread_analysis_filter_text,
                thread_analysis_filter_focus,
                palette,
                context,
            ),
            SettingsTab::Model => self.render_model_tab(palette),
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
        thread_analysis_filter_text: String,
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
            .child(self.render_thread_analysis_filter_setting(
                thread_analysis_filter_text,
                focus_handle,
                palette,
                context,
            ))
    }

    /// 渲染线程日志分析过滤设置项。
    ///
    /// 业务意图：
    /// - 用户可以直接粘贴一个或多个完整线程堆栈，后续线程日志分析会按这些片段过滤无效线程。
    /// - 输入区使用等宽字体和滚动容器，便于核对 Java 堆栈中的类名、方法名和锁信息。
    fn render_thread_analysis_filter_setting(
        &self,
        thread_analysis_filter_text: String,
        focus_handle: gpui::FocusHandle,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let has_filter = !thread_analysis_filter_text.trim().is_empty();
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
                    .child(
                        self.render_clear_thread_analysis_filter_button(
                            has_filter, palette, context,
                        ),
                    ),
            )
            .child(
                div()
                    .id("settings-thread-analysis-filter-input")
                    .relative()
                    .h(px(THREAD_ANALYSIS_FILTER_TEXTAREA_HEIGHT))
                    .w_full()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.input))
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
                                placeholder: "粘贴需要过滤的线程堆栈；多段堆栈之间用空行分隔",
                                palette,
                            }),
                    ),
            )
    }

    /// 渲染线程分析过滤清空按钮。
    fn render_clear_thread_analysis_filter_button(
        &self,
        enabled: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("settings-thread-analysis-filter-clear")
            .flex()
            .items_center()
            .justify_center()
            .h(px(28.0))
            .px_3()
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .text_xs()
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
            .child("清空")
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    if enabled {
                        view.clear_thread_analysis_filter(context);
                    }
                }),
            )
    }

    /// 渲染模型页签。
    ///
    /// 业务意图：
    /// - 用户要求模型页签先留白，因此这里仅保留空白内容区，不展示占位说明或未完成提示。
    fn render_model_tab(&self, palette: AppThemePalette) -> gpui::Stateful<gpui::Div> {
        div()
            .id("settings-model-tab")
            .size_full()
            .bg(rgb(palette.background))
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
            thread_analysis_filter_text,
            thread_analysis_filter_focus,
            palette,
        ) = {
            let main_view = self.main_view.read(context);
            (
                main_view.settings_active_tab,
                main_view.theme_preference,
                main_view.log_viewer_font_size,
                main_view.thread_analysis_filter_text.clone(),
                main_view.thread_analysis_filter_focus.clone(),
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
                        active_tab,
                        theme,
                        log_viewer_font_size,
                        thread_analysis_filter_text,
                        thread_analysis_filter_focus,
                        palette,
                        context,
                    )),
            )
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
            vec![0..0]
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
        if self.focus_handle.is_focused(window) {
            window.request_animation_frame();
        }
        self.view.update(context, |view, _context| {
            view.store_thread_analysis_filter_text_layouts(layouts, bounds);
        });
    }
}
