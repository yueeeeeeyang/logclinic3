// 设置窗口通用页签渲染。
//
// 业务意图：
// - 从设置窗口框架中拆出 主题和日志字号设置渲染，避免页签内容继续堆在独立窗口入口文件中。
// - 本轮只移动渲染方法，保持按钮、输入元素、配置读写和焦点流转行为不变。

use super::*;

impl SettingsWindowView {
    /// 渲染通用页签。
    ///
    /// 业务意图：
    /// - 通用页签当前承载主题和日志显示字号，未来可继续加入语言等全局体验类配置。
    pub(in crate::app) fn render_general_tab(
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
    pub(in crate::app) fn render_theme_setting_row(
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
    pub(in crate::app) fn render_theme_segment_option(
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
    pub(in crate::app) fn render_log_font_size_setting(
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
    pub(in crate::app) fn render_log_font_size_button(
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
}
