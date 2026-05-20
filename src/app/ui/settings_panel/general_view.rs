// 设置窗口通用页签渲染。
//
// 业务意图：
// - 从设置窗口框架中拆出主题、日志字号、minimap 和右键菜单设置渲染，避免页签内容继续堆在独立窗口入口文件中。
// - 通用页统一采用“左侧说明、右侧轻量控件”的结构，保持设置项密度和跨平台布局稳定。

use super::*;

impl SettingsWindowView {
    /// 渲染通用页签。
    ///
    /// 业务意图：
    /// - 通用页签承载主题、日志阅读体验和系统集成入口，未来可继续加入语言等全局体验类配置。
    pub(in crate::app) fn render_general_tab(
        &self,
        theme: ThemePreference,
        log_viewer_font_size: f32,
        log_minimap_enabled: bool,
        shell_integration_state: &ShellIntegrationUiState,
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
                    ))
                    .child(div().h(px(1.0)).mx_3().bg(rgb(palette.border)))
                    .child(self.render_log_minimap_setting(log_minimap_enabled, palette, context))
                    .child(div().h(px(1.0)).mx_3().bg(rgb(palette.border)))
                    .child(self.render_shell_integration_setting(
                        shell_integration_state,
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

    /// 渲染日志 minimap 显示设置。
    ///
    /// 业务意图：
    /// - minimap 是日志正文右侧的 VS Code 风格预览栏，能辅助定位长日志结构，但会占用宽度并增加绘制缓存任务。
    /// - 用户要求默认关闭，因此通用设置提供显式开关；开启后才允许日志视图进入 minimap 渲染路径。
    ///
    /// 边界条件：
    /// - 开关只控制普通内存日志是否允许显示 minimap；分页大日志和一屏可完整显示的短日志仍由日志视图按性能规则隐藏。
    pub(in crate::app) fn render_log_minimap_setting(
        &self,
        enabled: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("settings-log-minimap")
            .flex()
            .items_center()
            .justify_between()
            .min_h(px(74.0))
            .px_4()
            .py_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .min_w_0()
                    .child(MainView::render_lucide_icon(
                        Some(Icon::Map),
                        18.0,
                        18.0,
                        palette.muted_text,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.text))
                                    .child("日志 Minimap"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette.muted_text))
                                    .child("在日志正文右侧显示预览栏，默认关闭以保持滚动性能"),
                            ),
                    ),
            )
            .child(self.render_log_minimap_toggle(enabled, palette, context))
    }

    /// 渲染日志 minimap 开关控件。
    ///
    /// 业务意图：
    /// - 使用紧凑的开关表达二元状态，避免把性能相关设置做成大按钮导致通用页信息密度失衡。
    /// - 文字状态固定宽度，开启/关闭切换时不会挤压左侧说明或造成布局跳动。
    pub(in crate::app) fn render_log_minimap_toggle(
        &self,
        enabled: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("settings-log-minimap-toggle-wrapper")
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .child(
                div()
                    .id("settings-log-minimap-toggle")
                    .flex()
                    .items_center()
                    .w(px(44.0))
                    .h(px(24.0))
                    .p(px(3.0))
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(rgb(if enabled {
                        palette.accent
                    } else {
                        palette.border
                    }))
                    .bg(rgb(if enabled {
                        palette.accent
                    } else {
                        palette.input
                    }))
                    .cursor_pointer()
                    .hover(move |toggle| {
                        toggle.bg(rgb(if enabled {
                            palette.accent_hover
                        } else {
                            palette.hover
                        }))
                    })
                    .when(enabled, |toggle| toggle.justify_end())
                    .when(!enabled, |toggle| toggle.justify_start())
                    .child(
                        div()
                            .w(px(16.0))
                            .h(px(16.0))
                            .rounded(px(8.0))
                            .border_1()
                            .border_color(rgb(if enabled {
                                palette.on_accent
                            } else {
                                palette.border
                            }))
                            .bg(rgb(if enabled {
                                palette.on_accent
                            } else {
                                palette.surface
                            })),
                    )
                    .on_click(
                        context.listener(|view, _event: &ClickEvent, _window, context| {
                            view.toggle_log_minimap_enabled(context);
                        }),
                    ),
            )
            .child(
                div()
                    .w(px(42.0))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(if enabled {
                        palette.accent
                    } else {
                        palette.muted_text
                    }))
                    .child(if enabled { "已开启" } else { "已关闭" }),
            )
    }

    /// 渲染系统右键菜单集成设置。
    ///
    /// 业务意图：
    /// - 用户需要在应用内主动注册或卸载系统右键入口，不依赖额外安装器或手工脚本。
    /// - 该设置只触发平台集成模块，不改变日志加载、HPROF 解析或文件关联业务规则。
    ///
    /// 边界条件：
    /// - 平台状态查询和注册动作在后台执行，按钮根据 `ShellIntegrationUiState` 禁用，避免重复点击产生并发注册。
    pub(in crate::app) fn render_shell_integration_setting(
        &self,
        state: &ShellIntegrationUiState,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let status_icon = match state {
            ShellIntegrationUiState::Ready(status) if status.is_registered() => Icon::Check,
            ShellIntegrationUiState::Failed(_) => Icon::AlertCircle,
            ShellIntegrationUiState::Checking
            | ShellIntegrationUiState::Registering
            | ShellIntegrationUiState::Unregistering => Icon::Loader,
            _ => Icon::MousePointerClick,
        };
        let status_color = match state {
            ShellIntegrationUiState::Ready(status) if status.is_registered() => palette.accent,
            ShellIntegrationUiState::Failed(_) => 0xcf222e,
            _ => palette.muted_text,
        };

        div()
            .id("settings-shell-integration")
            .flex()
            .items_center()
            .justify_between()
            .min_h(px(82.0))
            .px_4()
            .py_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .min_w_0()
                    .child(MainView::render_lucide_icon(
                        Some(status_icon),
                        18.0,
                        18.0,
                        status_color,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.text))
                                    .child("右键菜单集成"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(palette.muted_text))
                                    .child(Self::shell_integration_description()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(status_color))
                                    .child(state.message().to_string()),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.render_shell_integration_button(
                        "settings-shell-integration-refresh",
                        "刷新",
                        Icon::RefreshCw,
                        !state.is_busy(),
                        false,
                        ShellIntegrationAction::Refresh,
                        palette,
                        context,
                    ))
                    .child(self.render_shell_integration_button(
                        "settings-shell-integration-register",
                        "注册",
                        Icon::Plus,
                        state.can_register(),
                        true,
                        ShellIntegrationAction::Register,
                        palette,
                        context,
                    ))
                    .child(self.render_shell_integration_button(
                        "settings-shell-integration-unregister",
                        "卸载",
                        Icon::Trash2,
                        state.can_unregister(),
                        false,
                        ShellIntegrationAction::Unregister,
                        palette,
                        context,
                    )),
            )
    }

    /// 返回当前平台右键菜单入口的说明文案。
    fn shell_integration_description() -> &'static str {
        if cfg!(target_os = "macos") {
            "注册后会出现在 Finder 右键“打开方式”菜单中"
        } else if cfg!(target_os = "windows") {
            "注册后会出现在资源管理器所有文件的右键菜单中"
        } else {
            "当前平台暂不支持系统右键菜单注册"
        }
    }

    /// 渲染右键菜单集成操作按钮。
    ///
    /// 边界条件：
    /// - 禁用按钮仍保留占位，防止后台状态变化时右侧控件宽度跳动。
    #[allow(clippy::too_many_arguments)]
    fn render_shell_integration_button(
        &self,
        id: &'static str,
        label: &'static str,
        icon: Icon,
        enabled: bool,
        primary: bool,
        action: ShellIntegrationAction,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (text_color, background) = if primary && enabled {
            (palette.on_accent, palette.accent)
        } else {
            (
                if enabled {
                    palette.text
                } else {
                    palette.muted_text
                },
                palette.panel,
            )
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
            .bg(rgb(background))
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
                    if !enabled {
                        return;
                    }
                    match action {
                        ShellIntegrationAction::Refresh => {
                            view.refresh_shell_integration_status(context)
                        }
                        ShellIntegrationAction::Register => {
                            view.register_shell_integration_from_settings(context)
                        }
                        ShellIntegrationAction::Unregister => {
                            view.unregister_shell_integration_from_settings(context)
                        }
                    }
                }),
            )
    }
}

/// 右键菜单集成按钮动作。
///
/// 业务意图：
/// - 三个按钮共用渲染函数，用枚举比传闭包更容易保持 GPUI listener 的生命周期简单。
#[derive(Clone, Copy)]
enum ShellIntegrationAction {
    /// 重新查询平台注册状态。
    Refresh,
    /// 注册右键菜单入口。
    Register,
    /// 卸载右键菜单入口。
    Unregister,
}
