// AI 对话页面渲染方法。
//
// 业务意图：
// - 该文件只描述 AI 对话左侧历史栏、右侧消息流、模型菜单和输入栏的 GPUI 结构。
// - 元素 ID、尺寸常量、边距和交互回调保持迁移前一致，确保重构不改变 UI 表现。

use super::*;

/// 返回 AI 对话页占位提示。
///
/// 业务意图：
/// - AI 对话入口尚未接入真实请求时仍要把当前阻塞原因说明清楚；没有任何模型配置时，用户应优先去设置页新增模型，而不是看到“请求未接入”的泛化提示。
///
/// 边界条件：
/// - 这里仅判断是否存在已保存配置，不在渲染层重新校验 URL、API Key 或模型 ID；保存入口已经负责字段校验，避免占位页引入额外业务分支。
pub(in crate::app) fn ai_chat_placeholder_description(
    model_profiles: &[ModelProfile],
) -> &'static str {
    if model_profiles.is_empty() {
        "需要至少配置一个模型"
    } else {
        "当前版本尚未接入对话请求。"
    }
}

impl MainView {
    /// 渲染 AI 对话页。
    ///
    /// 业务意图：
    /// - AI 对话页提供多会话管理、模型选择、消息显示和流式输入，是日志查看器内的智能排障入口。
    pub(in crate::app) fn render_ai_chat_page(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let palette = self.palette();
        div()
            .id("ai-chat-page")
            .relative()
            .flex()
            .size_full()
            .bg(rgb(palette.background))
            .on_mouse_move(context.listener(Self::handle_ai_chat_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(Self::handle_ai_chat_mouse_up),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(Self::handle_ai_chat_mouse_up),
            )
            .child(self.render_ai_chat_sidebar(palette, context))
            .child(self.render_ai_chat_workspace(palette, context))
            .when(self.ai_chat.input_resize_drag.is_some(), |page| {
                page.child(
                    div()
                        .id("ai-chat-input-resize-cursor-overlay")
                        .absolute()
                        .left(px(0.0))
                        .right(px(0.0))
                        .top(px(0.0))
                        .bottom(px(0.0))
                        .cursor_row_resize()
                        .on_mouse_move(context.listener(Self::handle_ai_chat_mouse_move))
                        .on_mouse_up(
                            MouseButton::Left,
                            context.listener(Self::handle_ai_chat_mouse_up),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            context.listener(Self::handle_ai_chat_mouse_up),
                        ),
                )
            })
    }

    /// 渲染 AI 对话左侧会话栏。
    pub(in crate::app) fn render_ai_chat_sidebar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("ai-chat-sidebar")
            .flex()
            .flex_col()
            .w(px(AI_CHAT_CONVERSATION_LIST_WIDTH))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(AI_CHAT_TOP_BAR_HEIGHT))
                    .px_3()
                    .border_b_1()
                    .border_color(rgb(palette.border))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(Self::render_lucide_icon(
                                Some(Icon::BotMessageSquare),
                                15.0,
                                15.0,
                                palette.muted_text,
                            ))
                            .child("AI对话"),
                    )
                    .child(
                        div()
                            .id("ai-chat-new-conversation")
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(28.0))
                            .h(px(28.0))
                            .rounded(px(5.0))
                            .cursor_pointer()
                            .hover(move |button| button.bg(rgb(palette.hover)))
                            .child(Self::render_lucide_icon(
                                Some(Icon::Plus),
                                15.0,
                                15.0,
                                palette.text,
                            ))
                            .on_click(context.listener(
                                |view, _event: &ClickEvent, _window, context| {
                                    view.create_ai_chat_conversation(context);
                                },
                            )),
                    ),
            )
            .child(
                div()
                    .id("ai-chat-conversation-list")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .id("ai-chat-conversation-list-viewport")
                            .relative()
                            .size_full()
                            .p_2()
                            .overflow_hidden()
                            .child(
                                list(
                                    self.ai_chat.conversation_list_state.clone(),
                                    context.processor(
                                        move |view, index: usize, _window, context| {
                                            view.render_ai_chat_conversation_item_at_index(
                                                index, palette, context,
                                            )
                                        },
                                    ),
                                )
                                .size_full(),
                            )
                            .child(self.render_ai_chat_list_scrollbar(
                                AiChatScrollArea::Conversations,
                                &self.ai_chat.conversation_list_state,
                                palette,
                                context,
                            )),
                    ),
            )
    }

    /// 按索引渲染 AI 会话列表项。
    ///
    /// 性能约束：
    /// - 该方法由虚拟列表按可视区间调用，只读取当前索引对应的会话，避免历史很多时一次性创建所有行元素。
    pub(in crate::app) fn render_ai_chat_conversation_item_at_index(
        &self,
        index: usize,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        self.ai_chat
            .conversations
            .get(index)
            .cloned()
            .map(|conversation| {
                self.render_ai_chat_conversation_item(&conversation, palette, context)
                    .into_any_element()
            })
            .unwrap_or_else(|| {
                div()
                    .id("ai-chat-conversation-missing")
                    .hidden()
                    .into_any_element()
            })
    }

    /// 渲染单个 AI 会话列表项。
    pub(in crate::app) fn render_ai_chat_conversation_item(
        &self,
        conversation: &AiChatConversation,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let conversation_id = conversation.id.clone();
        let selected = self.ai_chat.active_conversation_id.as_deref() == Some(&conversation.id);
        div()
            .id(SharedString::from(format!(
                "ai-chat-conversation-{}",
                conversation.id
            )))
            .flex()
            .items_center()
            .gap_2()
            .w_full()
            .min_w_0()
            .h(px(36.0))
            .mb_1()
            .px_2()
            .rounded(px(6.0))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.panel
            }))
            .text_sm()
            .text_color(rgb(if selected {
                palette.accent
            } else {
                palette.text
            }))
            .cursor_pointer()
            .hover(move |row| {
                row.bg(rgb(if selected {
                    palette.selected
                } else {
                    palette.hover
                }))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(conversation.title.clone()),
            )
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.select_ai_chat_conversation(&conversation_id, context);
                }),
            )
    }

    /// 渲染 AI 对话右侧工作区。
    pub(in crate::app) fn render_ai_chat_workspace(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("ai-chat-workspace")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_ai_chat_header(palette, context))
            .child(self.render_ai_chat_messages(palette, context))
            .child(self.render_ai_chat_input_bar(palette, context))
            .when(self.ai_chat.model_menu_open, |workspace| {
                workspace
                    .child(self.render_ai_chat_model_menu_dismiss_overlay(context))
                    .child(self.render_ai_chat_model_menu(palette, context))
            })
    }

    /// 渲染 AI 对话顶部栏。
    pub(in crate::app) fn render_ai_chat_header(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let title = self
            .active_ai_chat_conversation()
            .map(|conversation| conversation.title.clone())
            .unwrap_or_else(|| "AI对话".to_string());
        div()
            .id("ai-chat-header")
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .h(px(AI_CHAT_TOP_BAR_HEIGHT))
            .px_4()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.background))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(title),
                    )
                    .child(
                        div()
                            .id("ai-chat-delete-conversation")
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(26.0))
                            .h(px(26.0))
                            .rounded(px(5.0))
                            .cursor_pointer()
                            .hover(move |button| button.bg(rgb(palette.hover)))
                            .child(Self::render_lucide_icon(
                                Some(Icon::Trash2),
                                14.0,
                                14.0,
                                palette.muted_text,
                            ))
                            .on_click(context.listener(
                                |view, _event: &ClickEvent, _window, context| {
                                    view.delete_active_ai_chat_conversation(context);
                                },
                            )),
                    ),
            )
    }

    /// 渲染 AI 对话模型选择器。
    pub(in crate::app) fn render_ai_chat_model_selector(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let label = self
            .active_ai_chat_model_profile()
            .map(|profile| profile.name)
            .unwrap_or_else(|| "选择模型".to_string());
        div()
            .id("ai-chat-model-selector")
            .flex()
            .items_center()
            .gap_1()
            .h(px(AI_CHAT_MODEL_SELECTOR_HEIGHT))
            .px_3()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .text_xs()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(div().max_w(px(180.0)).truncate().child(label))
            .child(Self::render_lucide_icon(
                Some(Icon::ChevronDown),
                13.0,
                13.0,
                palette.muted_text,
            ))
            .on_click(
                context.listener(|view, _event: &ClickEvent, _window, context| {
                    view.toggle_ai_chat_model_menu(context);
                }),
            )
    }

    /// 渲染 AI 对话模型下拉菜单。
    ///
    /// UI 约束：
    /// - 菜单浮在消息区和输入区之上，外壳必须消费左右键事件，避免点击菜单空白处穿透到输入框或消息区域。
    pub(in crate::app) fn render_ai_chat_model_menu(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("ai-chat-model-menu")
            .absolute()
            .left(px(AI_CHAT_MODEL_MENU_LEFT_OFFSET))
            .bottom(px(AI_CHAT_MODEL_MENU_BOTTOM_OFFSET))
            .w(px(260.0))
            .max_h(px(260.0))
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 菜单空白区域需要停留在菜单层，不应触发底层输入框聚焦或消息区选择。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 右键同样不能穿透到底层聊天区域，避免后续新增右键菜单时出现误触。
                    context.stop_propagation();
                }),
            )
            .children(
                self.model_config
                    .model_config_profiles
                    .iter()
                    .map(|profile| {
                        let profile_id = profile.id.clone();
                        let selected =
                            self.active_ai_chat_model_profile_id() == Some(profile.id.as_str());
                        div()
                            .id(SharedString::from(format!("ai-chat-model-{profile_id}")))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .h(px(34.0))
                            .text_xs()
                            .text_color(rgb(if selected {
                                palette.accent
                            } else {
                                palette.text
                            }))
                            .cursor_pointer()
                            .hover(move |item| item.bg(rgb(palette.hover)))
                            .child(Self::render_lucide_icon(
                                Some(if selected {
                                    Icon::Check
                                } else {
                                    Icon::MonitorCog
                                }),
                                13.0,
                                13.0,
                                if selected {
                                    palette.accent
                                } else {
                                    palette.muted_text
                                },
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(format!("{} · {}", profile.name, profile.model)),
                            )
                            .on_click(context.listener(
                                move |view, _event: &ClickEvent, _window, context| {
                                    view.select_ai_chat_model_profile(&profile_id, context);
                                    context.stop_propagation();
                                },
                            ))
                    })
                    .collect::<Vec<_>>(),
            )
    }

    /// 渲染 AI 对话模型菜单的关闭遮罩。
    ///
    /// 业务意图：
    /// - 模型菜单打开后，点击菜单外区域应收起菜单，并且这次点击不能继续落到消息区、历史栏或输入框。
    pub(in crate::app) fn render_ai_chat_model_menu_dismiss_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("ai-chat-model-menu-dismiss-overlay")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.ai_chat.model_menu_open = false;
                    context.notify();
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.ai_chat.model_menu_open = false;
                    context.notify();
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染 AI 对话虚拟列表滚动条。
    ///
    /// 业务意图：
    /// - `ListState` 负责滚轮和虚拟渲染，自绘滚动条负责给用户明确的当前位置提示和拖动入口。
    /// - 只有内容高度超过视口高度时才显示滚动条，避免少量历史或消息时出现无效控件。
    pub(in crate::app) fn render_ai_chat_list_scrollbar(
        &self,
        area: AiChatScrollArea,
        list_state: &ListState,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) = Self::ai_chat_list_scrollbar_metrics(list_state) else {
            return div()
                .id(match area {
                    AiChatScrollArea::Conversations => "ai-chat-conversation-scrollbar-empty",
                    AiChatScrollArea::Messages => "ai-chat-message-scrollbar-empty",
                })
                .hidden();
        };
        let element_id = match area {
            AiChatScrollArea::Conversations => "ai-chat-conversation-scrollbar",
            AiChatScrollArea::Messages => "ai-chat-message-scrollbar",
        };

        div()
            .id(element_id)
            .absolute()
            .top(metrics.thumb_start)
            .right(px(AI_CHAT_SCROLLBAR_PADDING))
            .w(px(AI_CHAT_SCROLLBAR_WIDTH))
            .h(metrics.thumb_length)
            .rounded(px(AI_CHAT_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(palette.scrollbar))
            .cursor_pointer()
            .hover(move |thumb| thumb.bg(rgb(palette.scrollbar_hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.start_ai_chat_scrollbar_drag(area, event);
                    context.notify();
                    // 滚动条覆盖在虚拟列表上，按下事件必须在滑块处结束，避免同时触发会话切换或文本选择。
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染 AI 对话消息区。
    pub(in crate::app) fn render_ai_chat_messages(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mut messages = div()
            .id("ai-chat-messages")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .bg(rgb(palette.background));

        if self.model_config.model_config_profiles.is_empty() {
            messages = messages.child(self.render_ai_chat_empty_state(
                ai_chat_placeholder_description(&self.model_config.model_config_profiles),
                palette,
            ));
        } else if let Some(error) = &self.ai_chat.database_error {
            messages = messages.child(self.render_ai_chat_empty_state(error, palette));
        } else if self.ai_chat.messages.is_empty() {
            messages = messages
                .child(self.render_ai_chat_empty_state("输入问题后开始新的 AI 对话", palette));
        } else {
            messages = messages.child(
                div()
                    .id("ai-chat-message-list-viewport")
                    .relative()
                    .size_full()
                    .px_4()
                    .overflow_hidden()
                    .child(
                        list(
                            self.ai_chat.message_list_state.clone(),
                            context.processor(move |view, index: usize, _window, _context| {
                                view.render_ai_chat_message_at_index(index, palette)
                            }),
                        )
                        .size_full(),
                    )
                    .child(self.render_ai_chat_list_scrollbar(
                        AiChatScrollArea::Messages,
                        &self.ai_chat.message_list_state,
                        palette,
                        context,
                    )),
            );
        }
        messages
    }

    /// 渲染 AI 对话空态。
    pub(in crate::app) fn render_ai_chat_empty_state(
        &self,
        message: &str,
        palette: AppThemePalette,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("ai-chat-empty-state")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .size_full()
            .text_color(rgb(palette.muted_text))
            .child(Self::render_lucide_icon(
                Some(Icon::BotMessageSquare),
                34.0,
                30.0,
                palette.muted_text,
            ))
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child("AI对话"),
            )
            .child(div().text_sm().child(message.to_string()))
    }

    /// 按索引渲染 AI 消息行。
    ///
    /// 性能约束：
    /// - 该方法只由消息虚拟列表调用，避免历史消息很多时一次性创建所有气泡。
    pub(in crate::app) fn render_ai_chat_message_at_index(
        &self,
        index: usize,
        palette: AppThemePalette,
    ) -> gpui::AnyElement {
        self.ai_chat
            .messages
            .get(index)
            .map(|message| {
                let top_gap = if index == 0 {
                    AI_CHAT_FIRST_MESSAGE_TOP_GAP
                } else {
                    0.0
                };
                self.render_ai_chat_message(message, top_gap, palette)
                    .into_any_element()
            })
            .unwrap_or_else(|| {
                div()
                    .id("ai-chat-message-missing")
                    .hidden()
                    .into_any_element()
            })
    }

    /// 渲染单条 AI 对话消息。
    pub(in crate::app) fn render_ai_chat_message(
        &self,
        message: &AiChatMessage,
        top_gap: f32,
        palette: AppThemePalette,
    ) -> gpui::Stateful<gpui::Div> {
        let is_user = message.role == AiChatMessageRole::User;
        let status_text = match message.status {
            AiChatMessageStatus::Streaming => Some("生成中..."),
            AiChatMessageStatus::Stopped => Some("已停止"),
            AiChatMessageStatus::Failed => message.error_message.as_deref().or(Some("请求失败")),
            AiChatMessageStatus::Complete => None,
        };
        let message_content = if message.content.is_empty() {
            div()
                .mt_1()
                .w_full()
                .min_w_0()
                .text_sm()
                .line_height(px(21.0))
                .whitespace_normal()
                .text_color(rgb(palette.text))
                .child(status_text.unwrap_or("").to_string())
                .into_any_element()
        } else if is_user {
            div()
                .mt_1()
                .w_full()
                .min_w_0()
                .text_sm()
                .line_height(px(21.0))
                .whitespace_normal()
                .text_color(rgb(palette.text))
                .child(message.content.clone())
                .into_any_element()
        } else {
            self.render_ai_chat_markdown_message(message, palette)
        };
        div()
            .id(SharedString::from(format!(
                "ai-chat-message-{}",
                message.id
            )))
            .flex()
            .w_full()
            .min_w_0()
            .justify_end()
            .when(!is_user, |row| row.justify_start())
            .pt(px(top_gap))
            .pb(px(AI_CHAT_MESSAGE_ROW_GAP))
            .child(
                div()
                    .max_w(relative(0.74))
                    .min_w_0()
                    .overflow_hidden()
                    .rounded(px(8.0))
                    .px_3()
                    .py_2()
                    .bg(rgb(if is_user {
                        palette.selected
                    } else {
                        palette.surface
                    }))
                    .border_1()
                    .border_color(rgb(if is_user {
                        palette.accent
                    } else {
                        palette.border
                    }))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(if is_user {
                                palette.accent
                            } else {
                                palette.muted_text
                            }))
                            .child(if is_user { "你" } else { "助手" }),
                    )
                    .child(message_content)
                    .when(
                        status_text.is_some() && !message.content.is_empty(),
                        |bubble| {
                            bubble.child(
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(rgb(
                                        if message.status == AiChatMessageStatus::Failed {
                                            palette.error
                                        } else {
                                            palette.muted_text
                                        },
                                    ))
                                    .child(status_text.unwrap_or("").to_string()),
                            )
                        },
                    ),
            )
    }

    /// 渲染 AI 对话输入栏。
    pub(in crate::app) fn render_ai_chat_input_bar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let can_send = self.ai_chat_can_send();
        let is_streaming = self.ai_chat.streaming_task.is_some();
        div()
            .id("ai-chat-input-bar")
            .relative()
            .px(px(AI_CHAT_INPUT_BAR_PADDING))
            .py(px(AI_CHAT_INPUT_BAR_PADDING))
            .border_t_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .child(self.render_ai_chat_input_resize_handle(palette, context))
            .child(self.render_ai_chat_input(can_send, is_streaming, palette, context))
    }

    /// 渲染 AI 对话输入区高度拖拽条。
    ///
    /// UI 约束：
    /// - 拖拽条覆盖输入栏顶部边缘，只消费鼠标事件，不绘制额外线条，避免和输入栏原有顶部分割线形成双线。
    pub(in crate::app) fn render_ai_chat_input_resize_handle(
        &self,
        _palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("ai-chat-input-resize-handle")
            .absolute()
            .top(px(-(AI_CHAT_INPUT_RESIZE_HANDLE_HEIGHT / 2.0)))
            .left(px(0.0))
            .right(px(0.0))
            .h(px(AI_CHAT_INPUT_RESIZE_HANDLE_HEIGHT))
            .cursor_row_resize()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.start_ai_chat_input_resize(event);
                    context.notify();
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染 AI 对话多行输入框。
    pub(in crate::app) fn render_ai_chat_input(
        &self,
        can_send: bool,
        is_streaming: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("ai-chat-input")
            .relative()
            .w(relative(1.0))
            .h(px(self.ai_chat.input_height))
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.input))
            .track_focus(&self.ai_chat.input_focus)
            .key_context("ai-chat-input")
            .on_key_down(
                context.listener(|view, event: &KeyDownEvent, _window, context| {
                    view.handle_ai_chat_input_key_down(event, context);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, window, context| {
                    view.start_ai_chat_input_mouse_selection(event, context);
                    window.focus(&view.ai_chat.input_focus);
                    context.stop_propagation();
                }),
            )
            .on_mouse_move(
                context.listener(|view, event: &MouseMoveEvent, _window, context| {
                    view.update_ai_chat_input_mouse_selection(event.position, context);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    view.finish_ai_chat_input_mouse_selection(context);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    view.finish_ai_chat_input_mouse_selection(context);
                }),
            )
            .child(
                div()
                    .id("ai-chat-input-scroll")
                    .size_full()
                    .px_3()
                    .pt(px(8.0))
                    .pb(px(AI_CHAT_INPUT_CONTENT_BOTTOM_PADDING))
                    .overflow_y_scroll()
                    .scrollbar_width(px(6.0))
                    .text_size(px(14.0))
                    .line_height(px(AI_CHAT_INPUT_LINE_HEIGHT))
                    .text_color(rgb(palette.text))
                    .child(AiChatInputElement {
                        view: context.entity(),
                        focus_handle: self.ai_chat.input_focus.clone(),
                        placeholder: "输入问题，Enter 发送，Shift+Enter 换行",
                        palette,
                    }),
            )
            .child(self.render_ai_chat_input_floating_controls(
                can_send,
                is_streaming,
                palette,
                context,
            ))
    }

    /// 渲染 AI 对话输入框底部浮层控件。
    ///
    /// UI 约束：
    /// - 模型选择和发送按钮悬浮在输入区域底部，不能参与输入框外部布局计算，否则会重新挤压文本区高度。
    /// - 浮层需要消费鼠标按下事件，避免点击按钮时触发底层文本框选区和焦点逻辑。
    pub(in crate::app) fn render_ai_chat_input_floating_controls(
        &self,
        can_send: bool,
        is_streaming: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("ai-chat-input-floating-controls")
            .absolute()
            .left(px(AI_CHAT_INPUT_FLOATING_CONTROLS_HORIZONTAL_INSET))
            .right(px(AI_CHAT_INPUT_FLOATING_CONTROLS_HORIZONTAL_INSET))
            .bottom(px(AI_CHAT_INPUT_FLOATING_CONTROLS_BOTTOM_INSET))
            .flex()
            .items_center()
            .justify_between()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 浮层控件属于输入框内部，但点击它们不应移动输入光标或开始文本选择。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 右键同样停留在浮层，避免未来输入区右键菜单和控件点击互相影响。
                    context.stop_propagation();
                }),
            )
            .child(self.render_ai_chat_model_selector(palette, context))
            .child(self.render_ai_chat_send_button(can_send, is_streaming, palette, context))
    }

    /// 渲染 AI 对话发送或停止按钮。
    pub(in crate::app) fn render_ai_chat_send_button(
        &self,
        can_send: bool,
        is_streaming: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let enabled = can_send || is_streaming;
        div()
            .id("ai-chat-send-button")
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .h(px(34.0))
            .px_4()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(if enabled {
                palette.accent
            } else {
                palette.border
            }))
            .bg(rgb(if enabled {
                palette.accent
            } else {
                palette.panel
            }))
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(if enabled {
                palette.on_accent
            } else {
                palette.muted_text
            }))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.accent_hover)))
            })
            .when(!enabled, |button| button.opacity(0.55))
            .child(Self::render_lucide_icon(
                Some(if is_streaming { Icon::X } else { Icon::Check }),
                14.0,
                14.0,
                if enabled {
                    palette.on_accent
                } else {
                    palette.muted_text
                },
            ))
            .child(if is_streaming { "停止" } else { "发送" })
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    if is_streaming {
                        view.stop_ai_chat_streaming(context);
                    } else if can_send {
                        view.start_ai_chat_send(context);
                    }
                }),
            )
    }
}
