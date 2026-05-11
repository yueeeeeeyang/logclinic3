// AI 对话 MainView 状态方法。
//
// 业务意图：
// - 该文件集中维护 AI 对话会话切换、模型选择、发送停止、流式事件、滚动条和输入区拖拽等状态流转。
// - 方法仍实现到 MainView 上，保持和其它 app_impl 功能域一致的调用方式，不引入新的公开状态对象。

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use super::*;

impl MainView {
    /// 返回 AI 对话数据库路径，失败时同步记录错误。
    pub(in crate::app) fn ai_chat_database_path_or_error(&mut self) -> Option<PathBuf> {
        let path = ai_chat_database_path();
        if path.is_none() {
            self.ai_chat_database_error =
                Some("当前平台没有可用的应用配置目录，无法保存 AI 对话历史".to_string());
        }
        path
    }

    /// 返回当前 AI 对话会话。
    pub(in crate::app) fn active_ai_chat_conversation(&self) -> Option<&AiChatConversation> {
        let active_id = self.ai_chat_active_conversation_id.as_deref()?;
        self.ai_chat_conversations
            .iter()
            .find(|conversation| conversation.id == active_id)
    }

    /// 返回当前 AI 对话会话的可变引用。
    pub(in crate::app) fn active_ai_chat_conversation_mut(
        &mut self,
    ) -> Option<&mut AiChatConversation> {
        let active_id = self.ai_chat_active_conversation_id.as_deref()?;
        self.ai_chat_conversations
            .iter_mut()
            .find(|conversation| conversation.id == active_id)
    }

    /// 返回当前 AI 对话选择的模型配置 ID。
    pub(in crate::app) fn active_ai_chat_model_profile_id(&self) -> Option<&str> {
        self.active_ai_chat_conversation()
            .and_then(|conversation| conversation.model_profile_id.as_deref())
    }

    /// 返回当前 AI 对话选择的模型配置。
    pub(in crate::app) fn active_ai_chat_model_profile(&self) -> Option<ModelProfile> {
        let profile_id = self.active_ai_chat_model_profile_id()?;
        self.model_config_profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
    }

    /// 判断 AI 对话当前是否可以发送。
    pub(in crate::app) fn ai_chat_can_send(&self) -> bool {
        self.ai_chat_streaming_task.is_none()
            && self.ai_chat_database_error.is_none()
            && self.ai_chat_active_conversation_id.is_some()
            && self.active_ai_chat_model_profile().is_some()
            && !self.ai_chat_input_text.trim().is_empty()
    }

    /// 新建 AI 对话会话。
    ///
    /// 业务意图：
    /// - 用户点击“新对话”时立即创建持久化会话，后续输入和模型选择都可以稳定落到该会话 ID。
    pub(in crate::app) fn create_ai_chat_conversation(&mut self, context: &mut Context<Self>) {
        let Some(path) = self.ai_chat_database_path_or_error() else {
            context.notify();
            return;
        };
        let model_profile_id = ai_chat_default_model_profile_id(
            &self.model_config_profiles,
            self.model_config_default_profile_id.as_deref(),
        );
        let conversation = new_ai_chat_conversation(model_profile_id);
        match insert_ai_chat_conversation(&path, &conversation) {
            Ok(()) => {
                self.ai_chat_conversations.insert(0, conversation.clone());
                self.ai_chat_active_conversation_id = Some(conversation.id);
                self.ai_chat_messages.clear();
                self.ai_chat_input_text.clear();
                self.ai_chat_input_selection_range = 0..0;
                self.ai_chat_input_marked_range = None;
                self.ai_chat_model_menu_open = false;
                self.ai_chat_database_error = None;
                self.reset_ai_chat_conversation_list_state();
                self.reset_ai_chat_message_list_state();
            }
            Err(error) => self.ai_chat_database_error = Some(error),
        }
        context.notify();
    }

    /// 选择 AI 对话会话。
    pub(in crate::app) fn select_ai_chat_conversation(
        &mut self,
        conversation_id: &str,
        context: &mut Context<Self>,
    ) {
        if self.ai_chat_active_conversation_id.as_deref() == Some(conversation_id) {
            return;
        }
        let Some(path) = self.ai_chat_database_path_or_error() else {
            context.notify();
            return;
        };
        match load_ai_chat_messages(&path, conversation_id) {
            Ok(messages) => {
                self.stop_ai_chat_streaming_without_notify();
                self.ai_chat_active_conversation_id = Some(conversation_id.to_string());
                self.ai_chat_messages = messages;
                self.ai_chat_model_menu_open = false;
                self.ai_chat_database_error = None;
                self.reset_ai_chat_message_list_state();
            }
            Err(error) => self.ai_chat_database_error = Some(error),
        }
        context.notify();
    }

    /// 删除当前 AI 对话会话。
    ///
    /// 边界条件：
    /// - 删除最后一个会话后立即创建一个新的空会话，保证右侧工作区始终有明确落库目标。
    pub(in crate::app) fn delete_active_ai_chat_conversation(
        &mut self,
        context: &mut Context<Self>,
    ) {
        let Some(active_id) = self.ai_chat_active_conversation_id.clone() else {
            return;
        };
        let Some(path) = self.ai_chat_database_path_or_error() else {
            context.notify();
            return;
        };
        self.stop_ai_chat_streaming_without_notify();
        match delete_ai_chat_conversation(&path, &active_id) {
            Ok(()) => {
                self.ai_chat_conversations
                    .retain(|conversation| conversation.id != active_id);
                if self.ai_chat_conversations.is_empty() {
                    let model_profile_id = ai_chat_default_model_profile_id(
                        &self.model_config_profiles,
                        self.model_config_default_profile_id.as_deref(),
                    );
                    let conversation = new_ai_chat_conversation(model_profile_id);
                    match insert_ai_chat_conversation(&path, &conversation) {
                        Ok(()) => {
                            self.ai_chat_active_conversation_id = Some(conversation.id.clone());
                            self.ai_chat_conversations.push(conversation);
                            self.ai_chat_messages.clear();
                            self.ai_chat_database_error = None;
                            self.reset_ai_chat_conversation_list_state();
                            self.reset_ai_chat_message_list_state();
                        }
                        Err(error) => {
                            self.ai_chat_active_conversation_id = None;
                            self.ai_chat_messages.clear();
                            self.ai_chat_database_error = Some(error);
                            self.reset_ai_chat_conversation_list_state();
                            self.reset_ai_chat_message_list_state();
                        }
                    }
                } else {
                    let next_id = self.ai_chat_conversations[0].id.clone();
                    self.ai_chat_active_conversation_id = Some(next_id.clone());
                    self.ai_chat_messages =
                        load_ai_chat_messages(&path, &next_id).unwrap_or_else(|error| {
                            self.ai_chat_database_error = Some(error);
                            Vec::new()
                        });
                    self.reset_ai_chat_conversation_list_state();
                    self.reset_ai_chat_message_list_state();
                }
            }
            Err(error) => self.ai_chat_database_error = Some(error),
        }
        context.notify();
    }

    /// 切换 AI 对话模型下拉菜单。
    pub(in crate::app) fn toggle_ai_chat_model_menu(&mut self, context: &mut Context<Self>) {
        if self.model_config_profiles.is_empty() {
            self.ai_chat_model_menu_open = false;
        } else {
            self.ai_chat_model_menu_open = !self.ai_chat_model_menu_open;
        }
        context.notify();
    }

    /// 为当前 AI 对话选择模型配置。
    pub(in crate::app) fn select_ai_chat_model_profile(
        &mut self,
        profile_id: &str,
        context: &mut Context<Self>,
    ) {
        let Some(path) = self.ai_chat_database_path_or_error() else {
            context.notify();
            return;
        };
        if !self
            .model_config_profiles
            .iter()
            .any(|profile| profile.id == profile_id)
        {
            self.ai_chat_database_error = Some("选择的模型配置不存在".to_string());
            context.notify();
            return;
        }
        let mut updated = None;
        if let Some(conversation) = self.active_ai_chat_conversation_mut() {
            conversation.model_profile_id = Some(profile_id.to_string());
            conversation.updated_at_ms = current_unix_time_millis();
            updated = Some(conversation.clone());
        }
        if let Some(conversation) = updated {
            match update_ai_chat_conversation(&path, &conversation) {
                Ok(()) => {
                    self.ai_chat_database_error = None;
                    self.sort_ai_chat_conversations();
                    self.reset_ai_chat_conversation_list_state();
                }
                Err(error) => self.ai_chat_database_error = Some(error),
            }
        }
        self.ai_chat_model_menu_open = false;
        context.notify();
    }

    /// 根据更新时间重新排序 AI 会话列表。
    pub(in crate::app) fn sort_ai_chat_conversations(&mut self) {
        self.ai_chat_conversations.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
        });
    }

    /// 重建 AI 历史会话虚拟列表状态。
    ///
    /// 业务意图：
    /// - 新建、删除或排序会话后，左侧历史栏的虚拟列表条目数和索引内容必须与最新数据一致。
    pub(in crate::app) fn reset_ai_chat_conversation_list_state(&mut self) {
        self.ai_chat_conversation_list_state
            .reset(self.ai_chat_conversations.len());
        if self
            .ai_chat_scrollbar_drag
            .is_some_and(|drag| drag.area == AiChatScrollArea::Conversations)
        {
            self.ai_chat_scrollbar_drag = None;
        }
    }

    /// 重建 AI 消息虚拟列表状态。
    ///
    /// 业务意图：
    /// - 切换会话、删除会话或重新加载消息时，消息列表的行数和已测量高度都需要失效，避免旧会话的高度缓存影响新会话。
    pub(in crate::app) fn reset_ai_chat_message_list_state(&mut self) {
        self.ai_chat_message_list_state
            .reset(self.ai_chat_messages.len());
        if self
            .ai_chat_scrollbar_drag
            .is_some_and(|drag| drag.area == AiChatScrollArea::Messages)
        {
            self.ai_chat_scrollbar_drag = None;
        }
    }

    /// 标记单条 AI 消息的虚拟列表高度需要重新测量。
    ///
    /// 业务意图：
    /// - SSE 流式输出会持续改变助手气泡高度，只失效当前消息行可以避免每个增量都重建整个消息列表。
    pub(in crate::app) fn invalidate_ai_chat_message_row(&mut self, message_id: &str) {
        let Some(index) = self
            .ai_chat_messages
            .iter()
            .position(|message| message.id == message_id)
        else {
            return;
        };
        self.ai_chat_message_list_state
            .splice(index..index.saturating_add(1), 1);
        self.ai_chat_message_list_state.scroll_to_reveal_item(index);
    }

    /// 返回 AI 对话指定滚动区域的虚拟列表状态。
    pub(in crate::app) fn ai_chat_list_state_for_area(&self, area: AiChatScrollArea) -> ListState {
        match area {
            AiChatScrollArea::Conversations => self.ai_chat_conversation_list_state.clone(),
            AiChatScrollArea::Messages => self.ai_chat_message_list_state.clone(),
        }
    }

    /// 计算 AI 对话虚拟列表的纵向滚动条布局。
    ///
    /// 业务意图：
    /// - 滚动条直接读取 `ListState` 的视口、高度和偏移，保证滚轮滚动、虚拟渲染和滑块位置使用同一份状态。
    pub(in crate::app) fn ai_chat_list_scrollbar_metrics(
        list_state: &ListState,
    ) -> Option<LogScrollbarMetrics> {
        let viewport_bounds = list_state.viewport_bounds();
        let viewport_height = viewport_bounds.size.height;
        let max_scroll = list_state.max_offset_for_scrollbar().height;
        if viewport_height <= px(0.0) || max_scroll <= px(0.0) {
            return None;
        }

        let scroll_top =
            (-list_state.scroll_px_offset_for_scrollbar().y).clamp(px(0.0), max_scroll);
        let content_height = viewport_height + max_scroll;
        let track_start = px(AI_CHAT_SCROLLBAR_PADDING);
        let track_length = (viewport_height - track_start * 2.0).max(px(1.0));
        let min_thumb_length = px(AI_CHAT_SCROLLBAR_MIN_THUMB_HEIGHT).min(track_length);
        let thumb_length = (viewport_height * (viewport_height / content_height))
            .clamp(min_thumb_length, track_length);
        let movable_length = (track_length - thumb_length).max(px(0.0));
        let thumb_start = track_start + movable_length * (scroll_top / max_scroll);

        Some(LogScrollbarMetrics {
            thumb_start,
            thumb_length,
            track_start,
            track_length,
            max_scroll,
            max_scroll_px: f64::from(max_scroll),
        })
    }

    /// 开始拖动 AI 对话列表滚动条。
    pub(in crate::app) fn start_ai_chat_scrollbar_drag(
        &mut self,
        area: AiChatScrollArea,
        event: &MouseDownEvent,
    ) {
        let list_state = self.ai_chat_list_state_for_area(area);
        let Some(metrics) = Self::ai_chat_list_scrollbar_metrics(&list_state) else {
            return;
        };
        if metrics.max_scroll <= px(0.0) {
            return;
        }
        let viewport_top = list_state.viewport_bounds().top();
        list_state.scrollbar_drag_started();
        self.ai_chat_scrollbar_drag = Some(AiChatScrollbarDrag {
            area,
            cursor_offset: event.position.y - viewport_top - metrics.thumb_start,
        });
    }

    /// 根据鼠标移动更新 AI 对话列表滚动条拖动。
    ///
    /// 边界条件：
    /// - 拖动过程中如果列表因为切换会话被重建，或者鼠标已经释放，需要立即清理拖动状态。
    pub(in crate::app) fn update_ai_chat_scrollbar_drag(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.ai_chat_scrollbar_drag else {
            return;
        };
        if !event.dragging() {
            self.stop_ai_chat_scrollbar_drag(context);
            return;
        }
        let list_state = self.ai_chat_list_state_for_area(drag.area);
        let Some(metrics) = Self::ai_chat_list_scrollbar_metrics(&list_state) else {
            self.stop_ai_chat_scrollbar_drag(context);
            return;
        };
        let viewport_top = list_state.viewport_bounds().top();
        let movable_length = (metrics.track_length - metrics.thumb_length).max(px(0.0));
        if metrics.max_scroll <= px(0.0) || movable_length <= px(0.0) {
            return;
        }

        let requested_thumb_start = event.position.y - viewport_top - drag.cursor_offset;
        let thumb_start =
            requested_thumb_start.clamp(metrics.track_start, metrics.track_start + movable_length);
        let scroll_offset =
            metrics.max_scroll * ((thumb_start - metrics.track_start) / movable_length);
        list_state.set_offset_from_scrollbar(point(px(0.0), -scroll_offset));
        context.notify();
    }

    /// 结束 AI 对话列表滚动条拖动。
    pub(in crate::app) fn stop_ai_chat_scrollbar_drag(&mut self, context: &mut Context<Self>) {
        let Some(drag) = self.ai_chat_scrollbar_drag.take() else {
            return;
        };
        self.ai_chat_list_state_for_area(drag.area)
            .scrollbar_drag_ended();
        context.notify();
    }

    /// 开始拖拽调整 AI 对话输入区高度。
    ///
    /// 业务意图：
    /// - 拖拽条位于输入区顶部，向上拖动扩大输入区，向下拖动缩小输入区，符合底部编辑器面板的常见交互。
    pub(in crate::app) fn start_ai_chat_input_resize(&mut self, event: &MouseDownEvent) {
        self.ai_chat_input_resize_drag = Some(AiChatInputResizeDrag {
            start_y: event.position.y,
            start_height: self.ai_chat_input_height,
        });
        self.ai_chat_input_selection_drag = None;
        self.ai_chat_scrollbar_drag = None;
        self.ai_chat_model_menu_open = false;
    }

    /// 根据鼠标移动更新 AI 对话输入区高度。
    ///
    /// 边界条件：
    /// - 拖拽可能跨过消息区、历史栏或窗口外侧，因此在 AI 页面根节点统一接管鼠标移动。
    /// - 高度通过窗口视口和固定上下限双重约束，避免小屏下输入区覆盖消息区。
    pub(in crate::app) fn update_ai_chat_input_resize_drag(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.ai_chat_input_resize_drag else {
            return;
        };
        if !event.dragging() {
            self.stop_ai_chat_input_resize_drag(context);
            return;
        }
        let viewport_height = f32::from(window.viewport_size().height);
        let delta = f32::from(drag.start_y - event.position.y);
        self.ai_chat_input_height =
            clamp_ai_chat_input_height(drag.start_height + delta, viewport_height);
        context.notify();
    }

    /// 结束 AI 对话输入区高度拖拽。
    pub(in crate::app) fn stop_ai_chat_input_resize_drag(&mut self, context: &mut Context<Self>) {
        if self.ai_chat_input_resize_drag.take().is_some() {
            context.notify();
        }
    }

    /// 处理 AI 对话页鼠标移动事件。
    pub(in crate::app) fn handle_ai_chat_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.update_ai_chat_scrollbar_drag(event, context);
        self.update_ai_chat_input_resize_drag(event, window, context);
    }

    /// 处理 AI 对话页鼠标释放事件。
    pub(in crate::app) fn handle_ai_chat_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.stop_ai_chat_scrollbar_drag(context);
        self.stop_ai_chat_input_resize_drag(context);
    }

    /// 读取 AI 对话输入区当前文本、选择范围和组合文本范围的快照。
    pub(in crate::app) fn ai_chat_input_text_snapshot(
        &self,
    ) -> (String, Range<usize>, Option<Range<usize>>) {
        (
            self.ai_chat_input_text.clone(),
            Self::clamp_search_text_range(
                &self.ai_chat_input_text,
                self.ai_chat_input_selection_range.clone(),
            ),
            self.ai_chat_input_marked_range.clone(),
        )
    }

    /// 保存 AI 对话输入区最近一次多行排版结果。
    pub(in crate::app) fn store_ai_chat_input_text_layouts(
        &mut self,
        layouts: Vec<AiChatInputLineLayout>,
        bounds: Bounds<Pixels>,
    ) {
        self.ai_chat_input_last_layouts = layouts;
        self.ai_chat_input_last_bounds = Some(bounds);
    }

    /// 返回 AI 对话输入区当前内容需要的可视行数。
    pub(in crate::app) fn ai_chat_input_visual_line_count(&self) -> usize {
        Self::thread_analysis_filter_line_ranges(&self.ai_chat_input_text)
            .len()
            .max(1)
    }

    /// 根据窗口坐标返回 AI 对话输入区 UTF-8 字节下标。
    pub(in crate::app) fn ai_chat_input_index_for_point(&self, position: Point<Pixels>) -> usize {
        if self.ai_chat_input_text.is_empty() {
            return 0;
        }
        for layout in &self.ai_chat_input_last_layouts {
            if position.y >= layout.bounds.top() && position.y <= layout.bounds.bottom() {
                return layout
                    .byte_range
                    .start
                    .saturating_add(
                        layout
                            .line
                            .closest_index_for_x(position.x - layout.bounds.left()),
                    )
                    .min(layout.byte_range.end);
            }
        }
        if let Some(bounds) = &self.ai_chat_input_last_bounds
            && position.y < bounds.top()
        {
            return 0;
        }
        self.ai_chat_input_text.len()
    }

    /// 开始 AI 对话输入区鼠标选择。
    pub(in crate::app) fn start_ai_chat_input_mouse_selection(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let index = self.ai_chat_input_index_for_point(event.position);
        self.ai_chat_input_marked_range = None;
        match event.click_count {
            0 | 1 => {
                if event.modifiers.shift {
                    self.ai_chat_input_selection_range.end = index;
                    self.ai_chat_input_selection_range = Self::clamp_search_text_range(
                        &self.ai_chat_input_text,
                        self.ai_chat_input_selection_range.clone(),
                    );
                } else {
                    self.ai_chat_input_selection_range = index..index;
                }
                self.ai_chat_input_selection_drag = Some(self.ai_chat_input_selection_range.start);
            }
            2 => {
                self.ai_chat_input_selection_range =
                    Self::search_text_word_range_for_index(&self.ai_chat_input_text, index);
                self.ai_chat_input_selection_drag = None;
            }
            _ => {
                self.ai_chat_input_selection_range = 0..self.ai_chat_input_text.len();
                self.ai_chat_input_selection_drag = None;
            }
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 鼠标拖拽时更新 AI 对话输入区选区终点。
    pub(in crate::app) fn update_ai_chat_input_mouse_selection(
        &mut self,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) {
        let Some(anchor) = self.ai_chat_input_selection_drag else {
            return;
        };
        let index = self.ai_chat_input_index_for_point(position);
        self.ai_chat_input_marked_range = None;
        self.ai_chat_input_selection_range =
            Self::clamp_search_text_range(&self.ai_chat_input_text, anchor..index);
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 结束 AI 对话输入区鼠标拖拽选择。
    pub(in crate::app) fn finish_ai_chat_input_mouse_selection(
        &mut self,
        context: &mut Context<Self>,
    ) {
        if self.ai_chat_input_selection_drag.take().is_some() {
            context.notify();
        }
    }

    /// 返回 AI 对话输入区当前选中文本。
    pub(in crate::app) fn selected_ai_chat_input_text(&self) -> Option<String> {
        let range = Self::clamp_search_text_range(
            &self.ai_chat_input_text,
            self.ai_chat_input_selection_range.clone(),
        );
        (range.start < range.end).then(|| self.ai_chat_input_text[range].to_string())
    }

    /// 用给定文本替换 AI 对话输入区当前选区。
    pub(in crate::app) fn replace_ai_chat_input_selection(&mut self, replacement: &str) {
        let replacement = replacement.replace("\r\n", "\n").replace('\r', "\n");
        let range = self.ai_chat_input_marked_range.take().unwrap_or_else(|| {
            Self::clamp_search_text_range(
                &self.ai_chat_input_text,
                self.ai_chat_input_selection_range.clone(),
            )
        });
        self.ai_chat_input_text
            .replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        self.ai_chat_input_selection_range = cursor..cursor;
    }

    /// 处理 AI 对话输入区按键。
    ///
    /// 业务意图：
    /// - Enter 发送、Shift+Enter 换行；普通字符和中文 IME 提交继续交给平台输入协议，避免手写按键字符破坏输入法。
    pub(in crate::app) fn handle_ai_chat_input_key_down(
        &mut self,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_paste_keystroke(&event.keystroke) {
            if let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) {
                self.replace_ai_chat_input_selection(&text);
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if Self::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_ai_chat_input_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return;
        }

        if Self::is_cut_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_ai_chat_input_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                self.replace_ai_chat_input_selection("");
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if Self::is_select_all_keystroke(&event.keystroke) {
            self.ai_chat_input_marked_range = None;
            self.ai_chat_input_selection_range = 0..self.ai_chat_input_text.len();
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return;
        }

        match event.keystroke.key.as_str() {
            "enter" => {
                if event.keystroke.modifiers.shift {
                    self.replace_ai_chat_input_selection("\n");
                    self.touch_search_text_cursor_activity();
                } else {
                    self.start_ai_chat_send(context);
                }
                context.stop_propagation();
                context.notify();
            }
            "left" => {
                self.ai_chat_input_marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.ai_chat_input_selection_range.end = Self::previous_search_text_boundary(
                        &self.ai_chat_input_text,
                        self.ai_chat_input_selection_range.end,
                    );
                    self.ai_chat_input_selection_range = Self::clamp_search_text_range(
                        &self.ai_chat_input_text,
                        self.ai_chat_input_selection_range.clone(),
                    );
                } else if self.ai_chat_input_selection_range.start
                    != self.ai_chat_input_selection_range.end
                {
                    self.ai_chat_input_selection_range = self.ai_chat_input_selection_range.start
                        ..self.ai_chat_input_selection_range.start;
                } else {
                    let cursor = Self::previous_search_text_boundary(
                        &self.ai_chat_input_text,
                        self.ai_chat_input_selection_range.end,
                    );
                    self.ai_chat_input_selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "right" => {
                self.ai_chat_input_marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.ai_chat_input_selection_range.end = Self::next_search_text_boundary(
                        &self.ai_chat_input_text,
                        self.ai_chat_input_selection_range.end,
                    );
                    self.ai_chat_input_selection_range = Self::clamp_search_text_range(
                        &self.ai_chat_input_text,
                        self.ai_chat_input_selection_range.clone(),
                    );
                } else if self.ai_chat_input_selection_range.start
                    != self.ai_chat_input_selection_range.end
                {
                    self.ai_chat_input_selection_range = self.ai_chat_input_selection_range.end
                        ..self.ai_chat_input_selection_range.end;
                } else {
                    let cursor = Self::next_search_text_boundary(
                        &self.ai_chat_input_text,
                        self.ai_chat_input_selection_range.end,
                    );
                    self.ai_chat_input_selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "up" => {
                self.ai_chat_input_marked_range = None;
                self.ai_chat_input_selection_range = 0..0;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "down" => {
                self.ai_chat_input_marked_range = None;
                let cursor = self.ai_chat_input_text.len();
                self.ai_chat_input_selection_range = cursor..cursor;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "backspace" => {
                if self.ai_chat_input_selection_range.start
                    != self.ai_chat_input_selection_range.end
                    || self.ai_chat_input_marked_range.is_some()
                {
                    self.replace_ai_chat_input_selection("");
                } else if let Some((previous_index, _)) = self.ai_chat_input_text
                    [..self.ai_chat_input_selection_range.end]
                    .char_indices()
                    .next_back()
                {
                    let cursor = self.ai_chat_input_selection_range.end;
                    self.ai_chat_input_text
                        .replace_range(previous_index..cursor, "");
                    self.ai_chat_input_selection_range = previous_index..previous_index;
                    self.ai_chat_input_marked_range = None;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "delete" => {
                if self.ai_chat_input_selection_range.start
                    != self.ai_chat_input_selection_range.end
                    || self.ai_chat_input_marked_range.is_some()
                {
                    self.replace_ai_chat_input_selection("");
                } else if let Some((next_index, next_character)) = self.ai_chat_input_text
                    [self.ai_chat_input_selection_range.end..]
                    .char_indices()
                    .next()
                {
                    let start = self.ai_chat_input_selection_range.end + next_index;
                    let end = start + next_character.len_utf8();
                    self.ai_chat_input_text.replace_range(start..end, "");
                    self.ai_chat_input_selection_range = start..start;
                    self.ai_chat_input_marked_range = None;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "escape" => {}
            _ => {}
        }
    }

    /// 开始发送 AI 对话消息。
    pub(in crate::app) fn start_ai_chat_send(&mut self, context: &mut Context<Self>) {
        if !self.ai_chat_can_send() {
            return;
        }
        let Some(path) = self.ai_chat_database_path_or_error() else {
            context.notify();
            return;
        };
        let Some(conversation_id) = self.ai_chat_active_conversation_id.clone() else {
            return;
        };
        let Some(profile) = self.active_ai_chat_model_profile() else {
            self.ai_chat_database_error = Some("需要选择一个可用模型配置".to_string());
            context.notify();
            return;
        };

        let content = self.ai_chat_input_text.trim().to_string();
        let now = current_unix_time_millis();
        let next_sequence = self
            .ai_chat_messages
            .last()
            .map(|message| message.sequence.saturating_add(1))
            .unwrap_or(1);
        let user_message = AiChatMessage {
            id: new_ai_chat_entity_id("ai-message"),
            conversation_id: conversation_id.clone(),
            role: AiChatMessageRole::User,
            content,
            status: AiChatMessageStatus::Complete,
            error_message: None,
            sequence: next_sequence,
            created_at_ms: now,
            updated_at_ms: now,
        };
        let assistant_message = AiChatMessage {
            id: new_ai_chat_entity_id("ai-message"),
            conversation_id: conversation_id.clone(),
            role: AiChatMessageRole::Assistant,
            content: String::new(),
            status: AiChatMessageStatus::Streaming,
            error_message: None,
            sequence: next_sequence.saturating_add(1),
            created_at_ms: now,
            updated_at_ms: now,
        };

        if let Err(error) = insert_ai_chat_message(&path, &user_message)
            .and_then(|_| insert_ai_chat_message(&path, &assistant_message))
        {
            self.ai_chat_database_error = Some(error);
            context.notify();
            return;
        }

        let old_message_count = self.ai_chat_messages.len();
        self.ai_chat_messages.push(user_message.clone());
        self.ai_chat_messages.push(assistant_message.clone());
        self.ai_chat_message_list_state
            .splice(old_message_count..old_message_count, 2);
        self.ai_chat_message_list_state
            .scroll_to_reveal_item(self.ai_chat_messages.len().saturating_sub(1));
        self.ai_chat_input_text.clear();
        self.ai_chat_input_selection_range = 0..0;
        self.ai_chat_input_marked_range = None;
        self.ai_chat_database_error = None;

        let mut conversation_to_update = None;
        let should_update_title = self
            .active_ai_chat_conversation()
            .is_some_and(|conversation| conversation.title == "新对话");
        if let Some(conversation) = self.active_ai_chat_conversation_mut() {
            if should_update_title {
                conversation.title = ai_chat_title_from_user_message(&user_message.content);
            }
            conversation.updated_at_ms = now;
            conversation_to_update = Some(conversation.clone());
        }
        if let Some(conversation) = conversation_to_update {
            if let Err(error) = update_ai_chat_conversation(&path, &conversation) {
                self.ai_chat_database_error = Some(error);
            }
            self.sort_ai_chat_conversations();
            self.reset_ai_chat_conversation_list_state();
        }

        let request_messages = self.ai_chat_messages.clone();
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_for_task = cancel.clone();
        let job_id = self.next_ai_chat_job_id;
        self.next_ai_chat_job_id = self.next_ai_chat_job_id.saturating_add(1);
        self.ai_chat_streaming_task = Some(AiChatStreamingTask {
            job_id,
            conversation_id,
            assistant_message_id: assistant_message.id,
            receiver,
            cancel,
            last_persisted_len: 0,
        });

        context
            .background_spawn(async move {
                stream_openai_compatible_ai_chat(
                    profile,
                    request_messages,
                    cancel_for_task,
                    sender,
                );
            })
            .detach();
        self.schedule_ai_chat_stream_poll(context);
        context.notify();
    }

    /// 停止当前 AI 流式生成。
    pub(in crate::app) fn stop_ai_chat_streaming(&mut self, context: &mut Context<Self>) {
        self.finish_ai_chat_streaming_task_immediately(AiChatMessageStatus::Stopped, None);
        context.notify();
    }

    /// 停止 AI 流式生成但不触发重绘。
    pub(in crate::app) fn stop_ai_chat_streaming_without_notify(&mut self) {
        self.finish_ai_chat_streaming_task_immediately(AiChatMessageStatus::Stopped, None);
    }

    /// 立即结束当前 AI 流式任务并持久化助手消息状态。
    ///
    /// 业务意图：
    /// - 用户点击停止、切换会话或删除会话时，前台状态必须立即从 `streaming` 变为终态，不能等待后台阻塞读取返回。
    /// - 后台线程可能稍后才从网络读取中退出；这里先丢弃接收器并设置取消标记，后续旧事件不会再覆盖当前 UI 状态。
    pub(in crate::app) fn finish_ai_chat_streaming_task_immediately(
        &mut self,
        status: AiChatMessageStatus,
        error_message: Option<String>,
    ) {
        let Some(task) = self.ai_chat_streaming_task.take() else {
            return;
        };
        task.cancel.store(true, Ordering::Relaxed);
        self.finish_ai_chat_assistant_message(&task.assistant_message_id, status, error_message);
    }

    /// 安排前台轮询 AI 流式事件。
    pub(in crate::app) fn schedule_ai_chat_stream_poll(&self, context: &mut Context<Self>) {
        context
            .spawn(async move |view, app| {
                loop {
                    app.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                    let keep_polling = view
                        .update(app, |view, context| {
                            view.drain_ai_chat_stream_events(context);
                            view.ai_chat_streaming_task.is_some()
                        })
                        .unwrap_or(false);
                    if !keep_polling {
                        break;
                    }
                }
            })
            .detach();
    }

    /// 处理后台 AI 流式事件。
    pub(in crate::app) fn drain_ai_chat_stream_events(&mut self, context: &mut Context<Self>) {
        loop {
            let event = match self.ai_chat_streaming_task.as_ref() {
                Some(task) => match task.receiver.try_recv() {
                    Ok(event) => event,
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => AiChatStreamEvent::Done,
                },
                None => break,
            };
            self.apply_ai_chat_stream_event(event);
        }
        context.notify();
    }

    /// 应用单个 AI 流式事件。
    pub(in crate::app) fn apply_ai_chat_stream_event(&mut self, event: AiChatStreamEvent) {
        let Some(task_snapshot) = self.ai_chat_streaming_task.as_ref().map(|task| {
            (
                task.conversation_id.clone(),
                task.assistant_message_id.clone(),
                task.job_id,
            )
        }) else {
            return;
        };
        let (conversation_id, assistant_message_id, _job_id) = task_snapshot;
        if self.ai_chat_active_conversation_id.as_deref() != Some(conversation_id.as_str()) {
            return;
        }

        match event {
            AiChatStreamEvent::Delta(delta) => {
                let mut message_to_persist = None;
                let mut changed_message_id = None;
                if let Some(message) = self
                    .ai_chat_messages
                    .iter_mut()
                    .find(|message| message.id == assistant_message_id)
                {
                    message.content.push_str(&delta);
                    message.updated_at_ms = current_unix_time_millis();
                    changed_message_id = Some(message.id.clone());
                    if let Some(task) = self.ai_chat_streaming_task.as_mut()
                        && message
                            .content
                            .len()
                            .saturating_sub(task.last_persisted_len)
                            >= 512
                    {
                        task.last_persisted_len = message.content.len();
                        message_to_persist = Some(message.clone());
                    }
                }
                if let Some(message) = message_to_persist {
                    self.persist_ai_chat_message(&message);
                }
                if let Some(message_id) = changed_message_id {
                    self.invalidate_ai_chat_message_row(&message_id);
                }
            }
            AiChatStreamEvent::Done => {
                self.finish_ai_chat_assistant_message(
                    &assistant_message_id,
                    AiChatMessageStatus::Complete,
                    None,
                );
                self.ai_chat_streaming_task = None;
            }
            AiChatStreamEvent::Stopped => {
                self.finish_ai_chat_assistant_message(
                    &assistant_message_id,
                    AiChatMessageStatus::Stopped,
                    None,
                );
                self.ai_chat_streaming_task = None;
            }
            AiChatStreamEvent::Error(message) => {
                self.finish_ai_chat_assistant_message(
                    &assistant_message_id,
                    AiChatMessageStatus::Failed,
                    Some(message),
                );
                self.ai_chat_streaming_task = None;
            }
        }
    }

    /// 完成、停止或失败 AI 助手消息。
    pub(in crate::app) fn finish_ai_chat_assistant_message(
        &mut self,
        message_id: &str,
        status: AiChatMessageStatus,
        error_message: Option<String>,
    ) {
        let message_to_persist = finish_ai_chat_assistant_message_in_list(
            &mut self.ai_chat_messages,
            message_id,
            status,
            error_message,
        );
        if let Some(message) = message_to_persist {
            self.persist_ai_chat_message(&message);
        }
        self.invalidate_ai_chat_message_row(message_id);
    }

    /// 持久化 AI 消息并把错误写回页面状态。
    pub(in crate::app) fn persist_ai_chat_message(&mut self, message: &AiChatMessage) {
        let Some(path) = ai_chat_database_path() else {
            self.ai_chat_database_error =
                Some("当前平台没有可用的应用配置目录，无法保存 AI 对话历史".to_string());
            return;
        };
        if let Err(error) = update_ai_chat_message(&path, message) {
            self.ai_chat_database_error = Some(error);
        }
    }
}
