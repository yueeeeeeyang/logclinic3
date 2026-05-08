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
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        match active_tab {
            SettingsTab::General => {
                self.render_general_tab(theme, log_viewer_font_size, palette, context)
            }
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
            .gap_3()
            .size_full()
            .p_4()
            .bg(rgb(palette.background))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(MainView::render_lucide_icon(
                        Some(Icon::Palette),
                        16.0,
                        16.0,
                        palette.muted_text,
                    ))
                    .child("主题设置"),
            )
            .child(
                div().flex().flex_col().gap_2().children(
                    ThemePreference::all()
                        .iter()
                        .copied()
                        .map(|option| self.render_theme_option(option, theme, palette, context)),
                ),
            )
            .child(self.render_log_font_size_setting(log_viewer_font_size, palette, context))
    }

    /// 渲染单个主题选项。
    fn render_theme_option(
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
            .justify_between()
            .h(px(40.0))
            .px_3()
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
                palette.surface
            }))
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .text_color(rgb(palette.text))
                    .child(MainView::render_lucide_icon(
                        Some(option.icon()),
                        16.0,
                        15.0,
                        palette.muted_text,
                    ))
                    .child(option.label()),
            )
            .child(MainView::render_lucide_icon(
                Some(if selected {
                    Icon::CheckCircle2
                } else {
                    Icon::Circle
                }),
                16.0,
                15.0,
                if selected {
                    palette.accent
                } else {
                    palette.muted_text
                },
            ))
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
            .h(px(44.0))
            .px_3()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
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
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child(format!(
                                "范围 {}px - {}px，默认 {}px",
                                LOG_VIEWER_MIN_FONT_SIZE.round(),
                                LOG_VIEWER_MAX_FONT_SIZE.round(),
                                LOG_VIEWER_DEFAULT_FONT_SIZE.round()
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
                            .bg(rgb(palette.input))
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
        let (active_tab, theme, log_viewer_font_size, palette) = {
            let main_view = self.main_view.read(context);
            (
                main_view.settings_active_tab,
                main_view.theme_preference,
                main_view.log_viewer_font_size,
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
                        palette,
                        context,
                    )),
            )
    }
}
