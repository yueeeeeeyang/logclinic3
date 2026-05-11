// 设置窗口模型页签渲染。
//
// 业务意图：
// - 从设置窗口框架中拆出 模型配置列表、表单和测试状态渲染，避免页签内容继续堆在独立窗口入口文件中。
// - 本轮只移动渲染方法，保持按钮、输入元素、配置读写和焦点流转行为不变。

use super::settings_model_input::ModelConfigInputElement;
use super::settings_window_view::ModelIconButtonRequest;
use super::*;

impl SettingsWindowView {
    /// 渲染模型页签。
    ///
    /// 业务意图：
    /// - 模型页提供 OpenAI 兼容模型配置管理，用户可以维护多条配置、测试当前表单并设置默认模型。
    /// - 列表和表单并排展示，避免多条配置时频繁滚动；窗口较小时外层允许纵向滚动，保证按钮不被裁剪。
    pub(in crate::app) fn render_model_tab(
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
    pub(in crate::app) fn render_model_profile_list(
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
    pub(in crate::app) fn render_model_profile_list_item(
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
    pub(in crate::app) fn render_model_text_field(
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
    pub(in crate::app) fn render_model_api_key_visibility_button(
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
    pub(in crate::app) fn render_model_icon_button<F>(
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
    pub(in crate::app) fn render_model_test_status(
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
