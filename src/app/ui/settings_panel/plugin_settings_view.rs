// 插件声明式设置页签渲染。
//
// 业务意图：
// - 外部插件通过 manifest 声明设置项，宿主在设置窗口中渲染可编辑规则，不在设置页启动插件进程。
// - 当前主要服务 `weaver-logext` 的 14 类日志匹配规则配置，保存后写入插件专属 JSON。
//
// 边界条件：
// - 输入内容只作为字符串保存，宿主不解析 glob 语义，避免不同插件业务规则混入宿主。
// - 插件未启用、加载失败或没有声明设置页签时展示空状态，不影响插件管理页。

use super::*;

impl SettingsWindowView {
    /// 渲染插件声明式设置页签。
    pub(in crate::app) fn render_plugin_settings_tab(
        &self,
        active_plugin_tab: Option<PluginSettingsTabSelection>,
        plugin_settings_tabs: Vec<PluginSettingsTabRenderItem>,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        self.main_view.update(context, |view, context| {
            view.sync_plugin_pattern_setting_inputs(context);
        });

        let selected_item = active_plugin_tab.as_ref().and_then(|selection| {
            plugin_settings_tabs
                .iter()
                .find(|item| item.matches_selection(selection))
                .cloned()
        });
        let (inputs, status_message) = {
            let main_view = self.main_view.read(context);
            (
                main_view
                    .settings
                    .plugin_pattern_inputs
                    .iter()
                    .cloned()
                    .enumerate()
                    .filter(|(_, input)| {
                        active_plugin_tab
                            .as_ref()
                            .map(|selection| {
                                input.plugin_id == selection.plugin_id
                                    && input.tab_id == selection.tab_id
                            })
                            .unwrap_or(false)
                    })
                    .collect::<Vec<_>>(),
                main_view.settings.plugin_settings_status_message.clone(),
            )
        };
        let title = selected_item
            .as_ref()
            .map(|item| item.title.clone())
            .unwrap_or_else(|| "插件设置".to_string());
        let icon = selected_item
            .as_ref()
            .map(|item| item.icon.clone())
            .unwrap_or(Icon::Settings);

        div()
            .id("settings-plugin-pattern-tab")
            .flex()
            .flex_col()
            .size_full()
            .p_4()
            .gap_3()
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .bg(rgb(palette.background))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(MainView::render_lucide_icon(
                                Some(icon),
                                16.0,
                                16.0,
                                palette.muted_text,
                            ))
                            .child(title),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(self.render_plugin_settings_button(
                                "plugin-settings-restore",
                                "恢复默认",
                                Icon::RefreshCw,
                                palette,
                                context,
                                |view, context| {
                                    view.main_view.update(context, |main_view, context| {
                                        main_view.restore_plugin_pattern_settings_defaults();
                                        context.notify();
                                    });
                                },
                            ))
                            .child(self.render_plugin_settings_button(
                                "plugin-settings-save",
                                "保存",
                                Icon::Check,
                                palette,
                                context,
                                |view, context| {
                                    view.main_view.update(context, |main_view, context| {
                                        main_view.save_plugin_pattern_settings_from_inputs();
                                        context.notify();
                                    });
                                },
                            )),
                    ),
            )
            .when_some(status_message, |body, message| {
                body.child(
                    div()
                        .rounded(px(6.0))
                        .border_1()
                        .border_color(rgb(palette.border))
                        .bg(rgb(palette.surface))
                        .p_3()
                        .text_xs()
                        .text_color(rgb(palette.muted_text))
                        .child(message),
                )
            })
            .when(inputs.is_empty(), |body| {
                body.child(
                    div()
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(rgb(palette.border))
                        .bg(rgb(palette.surface))
                        .p_4()
                        .text_sm()
                        .text_color(rgb(palette.muted_text))
                        .child("当前没有插件声明设置项"),
                )
            })
            .children(inputs.into_iter().map(|(index, input)| {
                self.render_plugin_pattern_input_row(index, input, palette, context)
            }))
    }

    /// 渲染单条插件匹配规则输入行。
    fn render_plugin_pattern_input_row(
        &self,
        index: usize,
        input: PluginPatternSettingInputState,
        palette: AppThemePalette,
        _context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let focus_handle = input.focus.clone();
        div()
            .id(SharedString::from(format!(
                "plugin-pattern-setting-{}-{}",
                input.plugin_id, input.key
            )))
            .flex()
            .flex_col()
            .gap_2()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .p_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
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
                                    .child(input.label),
                            )
                            .when_some(input.description, |body, description| {
                                body.child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(palette.muted_text))
                                        .child(description),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child(format!("默认：{}", input.default_value)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .min_w_0()
                    .h(px(32.0))
                    .px_2()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.input))
                    .track_focus(&focus_handle)
                    .key_context("plugin-pattern-setting-input")
                    .child(TextInputElement {
                        view: self.main_view.clone(),
                        binding: TextInputBinding::PluginPatternSetting(index),
                        focus_handle,
                        placeholder: "匹配规则，多个用分号分隔",
                        palette,
                    }),
            )
    }

    /// 渲染插件设置页按钮。
    fn render_plugin_settings_button<F>(
        &self,
        id: &'static str,
        label: &'static str,
        icon: Icon,
        palette: AppThemePalette,
        context: &mut Context<Self>,
        handler: F,
    ) -> gpui::Stateful<gpui::Div>
    where
        F: Fn(&mut SettingsWindowView, &mut Context<SettingsWindowView>) + 'static,
    {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .h(px(28.0))
            .px_2()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.input))
            .text_xs()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(MainView::render_lucide_icon(
                Some(icon),
                14.0,
                14.0,
                palette.muted_text,
            ))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    handler(view, context);
                    context.stop_propagation();
                    context.notify();
                }),
            )
    }
}
