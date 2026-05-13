// 设置页输入状态与表单协调。
//
// 业务意图：
// - 本文件由原主窗口工作区实现机械拆分而来，只承载 GPUI 壳层内的 模型配置、快搜关键字、线程过滤和搜索输入的 UI 状态适配。
// - 拆分过程保持所有状态字段、事件时序和用户可见行为不变，便于后续按视图职责继续收束。

use super::*;

/// 搜索框当前文件轻量导航方向。
///
/// 业务意图：
/// - 输入关键字自动从顶部定位第一条命中，“上一个/下一个”则基于上一次命中行继续前后跳转。
/// - 用显式枚举避免多个布尔参数组合后难以判断“从顶部搜索”和“反向搜索”的真实含义。
#[derive(Clone, Copy)]
enum CurrentFileSearchNavigation {
    /// 从文件顶部开始查找第一条命中。
    FirstFromTop,
    /// 从上一次命中之后继续向后查找。
    Next,
    /// 从上一次命中之前继续向前查找。
    Previous,
}

impl MainView {
    /// 读取搜索输入框当前文本、选择范围和组合文本范围的快照。
    ///
    /// 业务意图：
    /// - `SearchTextInputElement` 在 GPUI 绘制阶段只拿到 `App` 上下文，不能直接借用搜索窗口视图状态。
    /// - 通过主视图提供只读快照，保证绘制、命中测试和 IME 状态都来自同一份业务状态。
    pub(in crate::app) fn search_text_snapshot(
        &self,
        input_kind: SearchTextInputKind,
    ) -> Option<SingleLineTextInputSnapshot> {
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, selection_range, marked_range, horizontal_scroll_px) =
            Self::search_text_state(dialog, input_kind);
        Some(SingleLineTextInputSnapshot {
            text: text.to_string(),
            selection_range,
            marked_range,
            horizontal_scroll_px,
        })
    }

    /// 返回当前应参与线程日志分析过滤的已保存文本。
    ///
    /// 业务意图：
    /// - 设置页编辑态中的内容属于草稿，必须等用户点击保存后才影响下一次线程分析。
    /// - 如果用户一边编辑设置一边从主窗口启动线程分析，这里仍使用进入编辑前的快照，避免半成品堆栈过滤掉真实线程。
    pub(in crate::app) fn thread_analysis_filter_effective_text(&self) -> &str {
        self.settings
            .thread_analysis_filter_saved_text_before_edit
            .as_deref()
            .unwrap_or(&self.settings.thread_analysis_filter_text)
    }

    /// 返回当前应参与快搜的已保存关键字文本。
    ///
    /// 业务意图：
    /// - 设置页编辑态中的快搜关键字属于草稿，必须等用户点击保存后才影响搜索对话框的快搜按钮。
    /// - 如果用户一边编辑设置一边执行快搜，这里继续使用进入编辑前的快照，避免半成品关键字触发大量无关命中。
    pub(in crate::app) fn quick_search_keywords_effective_text(&self) -> &str {
        self.settings
            .quick_search_keywords_saved_text_before_edit
            .as_deref()
            .unwrap_or(&self.settings.quick_search_keywords_input.text)
    }

    /// 返回当前已保存且可用于快搜的关键字列表。
    pub(in crate::app) fn effective_quick_search_keywords(&self) -> Vec<String> {
        parse_quick_search_keywords(self.quick_search_keywords_effective_text())
    }

    /// 返回当前模型配置持久化结构快照。
    ///
    /// 业务意图：
    /// - 保存、删除和设为默认都通过同一个结构写入 JSON，避免列表和默认 ID 分别落盘导致状态不一致。
    pub(in crate::app) fn model_configs_snapshot(&self) -> ModelConfigs {
        normalize_model_configs(ModelConfigs {
            profiles: self.model_config.model_config_profiles.clone(),
            default_profile_id: self.model_config.model_config_default_profile_id.clone(),
        })
    }

    /// 返回模型配置输入框状态。
    pub(in crate::app) fn model_config_input_state(
        &self,
        kind: ModelConfigInputKind,
    ) -> &ModelConfigTextFieldState {
        match kind {
            ModelConfigInputKind::Name => &self.model_config.model_config_name_input,
            ModelConfigInputKind::BaseUrl => &self.model_config.model_config_base_url_input,
            ModelConfigInputKind::ApiKey => &self.model_config.model_config_api_key_input,
            ModelConfigInputKind::Model => &self.model_config.model_config_model_input,
        }
    }

    /// 返回模型配置输入框可变状态。
    pub(in crate::app) fn model_config_input_state_mut(
        &mut self,
        kind: ModelConfigInputKind,
    ) -> &mut ModelConfigTextFieldState {
        match kind {
            ModelConfigInputKind::Name => &mut self.model_config.model_config_name_input,
            ModelConfigInputKind::BaseUrl => &mut self.model_config.model_config_base_url_input,
            ModelConfigInputKind::ApiKey => &mut self.model_config.model_config_api_key_input,
            ModelConfigInputKind::Model => &mut self.model_config.model_config_model_input,
        }
    }

    /// 返回模型配置输入框焦点句柄。
    pub(in crate::app) fn model_config_input_focus(
        &self,
        kind: ModelConfigInputKind,
    ) -> gpui::FocusHandle {
        self.model_config_input_state(kind).focus.clone()
    }

    /// 根据窗口焦点判断当前平台输入应写入哪个模型配置字段。
    pub(in crate::app) fn active_model_config_input_kind(
        &self,
        window: &Window,
    ) -> Option<ModelConfigInputKind> {
        [
            ModelConfigInputKind::Name,
            ModelConfigInputKind::BaseUrl,
            ModelConfigInputKind::ApiKey,
            ModelConfigInputKind::Model,
        ]
        .into_iter()
        .find(|kind| {
            self.model_config_input_state(*kind)
                .focus
                .is_focused(window)
        })
    }

    /// 返回模型配置输入框的可见文本。
    ///
    /// 安全边界：
    /// - API Key 默认使用同等 UTF-8 字节长度的星号掩码，既避免界面明文展示，也让选区和光标索引仍能映射到可见文本。
    pub(in crate::app) fn model_config_input_display_text(
        &self,
        kind: ModelConfigInputKind,
    ) -> String {
        let state = self.model_config_input_state(kind);
        if kind == ModelConfigInputKind::ApiKey
            && !self.model_config.model_config_api_key_visible
            && !state.text.is_empty()
        {
            "*".repeat(state.text.len())
        } else {
            state.text.clone()
        }
    }

    /// 读取模型配置输入框绘制快照。
    pub(in crate::app) fn model_config_input_text_snapshot(
        &self,
        kind: ModelConfigInputKind,
    ) -> ModelConfigInputSnapshot {
        let state = self.model_config_input_state(kind);
        ModelConfigInputSnapshot {
            text: state.text.clone(),
            display_text: self.model_config_input_display_text(kind),
            selection_range: Self::clamp_search_text_range(
                &state.text,
                state.selection_range.clone(),
            ),
            marked_range: state.marked_range.clone(),
            horizontal_scroll_px: state.horizontal_scroll_px,
        }
    }

    /// 保存模型配置输入框最近一次单行排版结果。
    pub(in crate::app) fn store_model_config_input_layout(
        &mut self,
        kind: ModelConfigInputKind,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
        horizontal_scroll_px: f32,
    ) {
        let state = self.model_config_input_state_mut(kind);
        state.last_layout = Some(line);
        state.last_bounds = Some(bounds);
        state.horizontal_scroll_px = horizontal_scroll_px;
    }

    /// 根据鼠标窗口坐标返回模型配置输入框中的 UTF-8 字节下标。
    pub(in crate::app) fn model_config_input_index_for_point(
        &self,
        kind: ModelConfigInputKind,
        position: Point<Pixels>,
    ) -> usize {
        let state = self.model_config_input_state(kind);
        let (Some(layout), Some(bounds)) = (state.last_layout.as_ref(), state.last_bounds.as_ref())
        else {
            return state.text.len();
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return state.text.len();
        }
        layout
            .closest_index_for_x(position.x - bounds.left() + px(state.horizontal_scroll_px))
            .min(state.text.len())
    }

    /// 开始模型配置输入框鼠标选择。
    pub(in crate::app) fn start_model_config_input_mouse_selection(
        &mut self,
        kind: ModelConfigInputKind,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let index = self.model_config_input_index_for_point(kind, event.position);
        let state = self.model_config_input_state_mut(kind);
        state.marked_range = None;
        match event.click_count {
            0 | 1 => {
                if event.modifiers.shift {
                    state.selection_range.end = index;
                    state.selection_range =
                        Self::clamp_search_text_range(&state.text, state.selection_range.clone());
                } else {
                    state.selection_range = index..index;
                }
                state.selection_drag = Some(state.selection_range.start);
            }
            2 => {
                state.selection_range = Self::search_text_word_range_for_index(&state.text, index);
                state.selection_drag = None;
            }
            _ => {
                state.selection_range = 0..state.text.len();
                state.selection_drag = None;
            }
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 鼠标拖拽时更新模型配置输入框选区。
    pub(in crate::app) fn update_model_config_input_mouse_selection(
        &mut self,
        kind: ModelConfigInputKind,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) {
        let index = self.model_config_input_index_for_point(kind, position);
        let state = self.model_config_input_state_mut(kind);
        let Some(anchor) = state.selection_drag else {
            return;
        };
        state.marked_range = None;
        state.selection_range = Self::clamp_search_text_range(&state.text, anchor..index);
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 结束模型配置输入框鼠标拖拽选择。
    pub(in crate::app) fn finish_model_config_input_mouse_selection(
        &mut self,
        kind: ModelConfigInputKind,
        context: &mut Context<Self>,
    ) {
        if self
            .model_config_input_state_mut(kind)
            .selection_drag
            .take()
            .is_some()
        {
            context.notify();
        }
    }

    /// 返回模型配置输入框当前选中文本。
    pub(in crate::app) fn selected_model_config_input_text(
        &self,
        kind: ModelConfigInputKind,
    ) -> Option<String> {
        let state = self.model_config_input_state(kind);
        let range = Self::clamp_search_text_range(&state.text, state.selection_range.clone());
        (range.start < range.end).then(|| state.text[range].to_string())
    }

    /// 用给定文本替换模型配置输入框当前选区。
    ///
    /// 边界条件：
    /// - 四个字段都是单行输入，粘贴或 IME 提交中的换行会被移除，避免保存 JSON 时出现不可见跨行配置。
    pub(in crate::app) fn replace_model_config_input_selection(
        &mut self,
        kind: ModelConfigInputKind,
        replacement: &str,
    ) {
        let replacement = Self::sanitize_search_input_text(replacement);
        let state = self.model_config_input_state_mut(kind);
        let range = state.marked_range.take().unwrap_or_else(|| {
            Self::clamp_search_text_range(&state.text, state.selection_range.clone())
        });
        state.text.replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        state.selection_range = cursor..cursor;
        state.clear_layout();
    }

    /// 处理模型配置单行输入框的基础编辑按键。
    ///
    /// 业务意图：
    /// - 模型设置页字段需要支持复制、粘贴、剪切、全选、删除和方向键，普通字符输入继续交给平台 IME 回调。
    pub(in crate::app) fn handle_model_config_input_key_down(
        &mut self,
        kind: ModelConfigInputKind,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_paste_keystroke(&event.keystroke) {
            if let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) {
                self.replace_model_config_input_selection(kind, &text);
                self.model_config.model_test_status = ModelTestStatus::Idle;
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if Self::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_model_config_input_text(kind) {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return;
        }

        if Self::is_cut_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_model_config_input_text(kind) {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                self.replace_model_config_input_selection(kind, "");
                self.model_config.model_test_status = ModelTestStatus::Idle;
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if Self::is_select_all_keystroke(&event.keystroke) {
            let state = self.model_config_input_state_mut(kind);
            state.marked_range = None;
            state.selection_range = 0..state.text.len();
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return;
        }

        match event.keystroke.key.as_str() {
            "left" => {
                let state = self.model_config_input_state_mut(kind);
                state.marked_range = None;
                if event.keystroke.modifiers.shift {
                    state.selection_range.end =
                        Self::previous_search_text_boundary(&state.text, state.selection_range.end);
                    state.selection_range =
                        Self::clamp_search_text_range(&state.text, state.selection_range.clone());
                } else if state.selection_range.start != state.selection_range.end {
                    state.selection_range =
                        state.selection_range.start..state.selection_range.start;
                } else {
                    let cursor =
                        Self::previous_search_text_boundary(&state.text, state.selection_range.end);
                    state.selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "right" => {
                let state = self.model_config_input_state_mut(kind);
                state.marked_range = None;
                if event.keystroke.modifiers.shift {
                    state.selection_range.end =
                        Self::next_search_text_boundary(&state.text, state.selection_range.end);
                    state.selection_range =
                        Self::clamp_search_text_range(&state.text, state.selection_range.clone());
                } else if state.selection_range.start != state.selection_range.end {
                    state.selection_range = state.selection_range.end..state.selection_range.end;
                } else {
                    let cursor =
                        Self::next_search_text_boundary(&state.text, state.selection_range.end);
                    state.selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "up" => {
                let state = self.model_config_input_state_mut(kind);
                state.marked_range = None;
                state.selection_range = 0..0;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "down" => {
                let state = self.model_config_input_state_mut(kind);
                state.marked_range = None;
                let cursor = state.text.len();
                state.selection_range = cursor..cursor;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "backspace" => {
                let should_replace_selection = {
                    let state = self.model_config_input_state(kind);
                    state.selection_range.start != state.selection_range.end
                        || state.marked_range.is_some()
                };
                if should_replace_selection {
                    self.replace_model_config_input_selection(kind, "");
                } else {
                    let state = self.model_config_input_state_mut(kind);
                    if let Some((previous_index, _)) = state.text[..state.selection_range.end]
                        .char_indices()
                        .next_back()
                    {
                        let cursor = state.selection_range.end;
                        state.text.replace_range(previous_index..cursor, "");
                        state.selection_range = previous_index..previous_index;
                        state.marked_range = None;
                        state.clear_layout();
                    }
                }
                self.model_config.model_test_status = ModelTestStatus::Idle;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "delete" => {
                let should_replace_selection = {
                    let state = self.model_config_input_state(kind);
                    state.selection_range.start != state.selection_range.end
                        || state.marked_range.is_some()
                };
                if should_replace_selection {
                    self.replace_model_config_input_selection(kind, "");
                } else {
                    let state = self.model_config_input_state_mut(kind);
                    if let Some((next_index, next_character)) = state.text
                        [state.selection_range.end..]
                        .char_indices()
                        .next()
                    {
                        let start = state.selection_range.end + next_index;
                        let end = start + next_character.len_utf8();
                        state.text.replace_range(start..end, "");
                        state.selection_range = start..start;
                        state.marked_range = None;
                        state.clear_layout();
                    }
                }
                self.model_config.model_test_status = ModelTestStatus::Idle;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "enter" => {
                context.stop_propagation();
            }
            "escape" => {}
            _ => {}
        }
    }

    /// 清空模型配置表单，进入新增未保存状态。
    pub(in crate::app) fn clear_model_config_form(&mut self) {
        self.model_config.model_config_selected_profile_id = None;
        self.model_config.model_config_form_profile_id = None;
        self.model_config
            .model_config_name_input
            .set_text(String::new());
        self.model_config
            .model_config_base_url_input
            .set_text(String::new());
        self.model_config
            .model_config_api_key_input
            .set_text(String::new());
        self.model_config
            .model_config_model_input
            .set_text(String::new());
        self.model_config.model_config_api_key_visible = false;
        self.model_config.model_test_status = ModelTestStatus::Idle;
    }

    /// 把已保存模型配置加载到表单。
    pub(in crate::app) fn load_model_profile_into_form(&mut self, profile: &ModelProfile) {
        self.model_config.model_config_selected_profile_id = Some(profile.id.clone());
        self.model_config.model_config_form_profile_id = Some(profile.id.clone());
        self.model_config
            .model_config_name_input
            .set_text(profile.name.clone());
        self.model_config
            .model_config_base_url_input
            .set_text(profile.base_url.clone());
        self.model_config
            .model_config_api_key_input
            .set_text(profile.api_key.clone());
        self.model_config
            .model_config_model_input
            .set_text(profile.model.clone());
        self.model_config.model_config_api_key_visible = false;
        self.model_config.model_test_status = ModelTestStatus::Idle;
    }

    /// 进入新增模型配置状态。
    pub(in crate::app) fn begin_new_model_profile(&mut self, context: &mut Context<Self>) {
        self.clear_model_config_form();
        context.notify();
    }

    /// 选择已保存模型配置。
    pub(in crate::app) fn select_model_profile(
        &mut self,
        profile_id: &str,
        context: &mut Context<Self>,
    ) {
        let Some(profile) = self
            .model_config
            .model_config_profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
        else {
            return;
        };
        self.load_model_profile_into_form(&profile);
        context.notify();
    }

    /// 返回当前表单字段构造的模型配置。
    pub(in crate::app) fn model_profile_from_form(
        &self,
        id: String,
    ) -> Result<ModelProfile, String> {
        let name = self
            .model_config
            .model_config_name_input
            .text
            .trim()
            .to_string();
        let base_url = self
            .model_config
            .model_config_base_url_input
            .text
            .trim()
            .to_string();
        let api_key = self
            .model_config
            .model_config_api_key_input
            .text
            .trim()
            .to_string();
        let model = self
            .model_config
            .model_config_model_input
            .text
            .trim()
            .to_string();
        validate_model_profile_fields(&name, &base_url, &model)?;
        Ok(ModelProfile {
            id,
            name,
            base_url,
            api_key,
            model,
        })
    }

    /// 生成新的模型配置 ID。
    ///
    /// 边界条件：
    /// - 使用系统时间纳秒作为主干，若极端情况下撞到已有 ID，则追加序号直到唯一。
    pub(in crate::app) fn new_model_profile_id(&self) -> String {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let mut suffix = 0usize;
        loop {
            let candidate = if suffix == 0 {
                format!("model-profile-{seed}")
            } else {
                format!("model-profile-{seed}-{suffix}")
            };
            if !self
                .model_config
                .model_config_profiles
                .iter()
                .any(|profile| profile.id == candidate)
            {
                return candidate;
            }
            suffix += 1;
        }
    }

    /// 保存当前模型配置表单。
    ///
    /// 业务意图：
    /// - 已保存配置走覆盖更新，新增配置分配稳定 ID 后追加到列表；保存成功后同步落盘并选中新配置。
    pub(in crate::app) fn save_current_model_profile(&mut self, context: &mut Context<Self>) {
        let id = self
            .model_config
            .model_config_form_profile_id
            .clone()
            .unwrap_or_else(|| self.new_model_profile_id());
        let profile = match self.model_profile_from_form(id) {
            Ok(profile) => profile,
            Err(message) => {
                self.model_config.model_test_status = ModelTestStatus::Failed(message);
                context.notify();
                return;
            }
        };

        if let Some(existing) = self
            .model_config
            .model_config_profiles
            .iter_mut()
            .find(|existing| existing.id == profile.id)
        {
            *existing = profile.clone();
        } else {
            self.model_config
                .model_config_profiles
                .push(profile.clone());
        }
        self.model_config.model_config_form_profile_id = Some(profile.id.clone());
        self.model_config.model_config_selected_profile_id = Some(profile.id.clone());
        self.model_config.model_test_status =
            ModelTestStatus::Success("模型配置已保存".to_string());
        save_model_configs_preference(&self.model_configs_snapshot());
        context.notify();
    }

    /// 删除当前选中的模型配置。
    ///
    /// 边界条件：
    /// - 未保存的新配置没有 ID，删除时只清空表单。
    /// - 删除默认配置必须同步清空默认 ID，避免后续功能引用悬空配置。
    pub(in crate::app) fn delete_current_model_profile(&mut self, context: &mut Context<Self>) {
        let Some(profile_id) = self.model_config.model_config_form_profile_id.clone() else {
            self.clear_model_config_form();
            context.notify();
            return;
        };
        self.model_config
            .model_config_profiles
            .retain(|profile| profile.id != profile_id);
        if self.model_config.model_config_default_profile_id.as_deref() == Some(profile_id.as_str())
        {
            self.model_config.model_config_default_profile_id = None;
        }

        let next_profile = self.model_config.model_config_profiles.first().cloned();
        if let Some(profile) = next_profile {
            self.load_model_profile_into_form(&profile);
            self.model_config.model_test_status =
                ModelTestStatus::Success("模型配置已删除".to_string());
        } else {
            self.clear_model_config_form();
            self.model_config.model_test_status =
                ModelTestStatus::Success("模型配置已删除".to_string());
        }
        save_model_configs_preference(&self.model_configs_snapshot());
        context.notify();
    }

    /// 将当前已保存配置设为默认模型。
    pub(in crate::app) fn set_current_model_profile_default(
        &mut self,
        context: &mut Context<Self>,
    ) {
        let Some(profile_id) = self.model_config.model_config_form_profile_id.clone() else {
            self.model_config.model_test_status =
                ModelTestStatus::Failed("请先保存模型配置，再设为默认".to_string());
            context.notify();
            return;
        };
        if !self
            .model_config
            .model_config_profiles
            .iter()
            .any(|profile| profile.id == profile_id)
        {
            self.model_config.model_test_status =
                ModelTestStatus::Failed("默认模型必须指向已保存配置".to_string());
            context.notify();
            return;
        }
        self.model_config.model_config_default_profile_id = Some(profile_id);
        self.model_config.model_test_status =
            ModelTestStatus::Success("已设为默认模型".to_string());
        save_model_configs_preference(&self.model_configs_snapshot());
        context.notify();
    }

    /// 切换 API Key 明文/掩码显示。
    pub(in crate::app) fn toggle_model_api_key_visibility(&mut self, context: &mut Context<Self>) {
        self.model_config.model_config_api_key_visible =
            !self.model_config.model_config_api_key_visible;
        self.model_config.model_config_api_key_input.clear_layout();
        context.notify();
    }

    /// 使用当前表单值测试 OpenAI 兼容模型接口。
    ///
    /// 业务意图：
    /// - 测试按钮使用未保存表单值，便于用户粘贴后先验证再决定是否保存。
    /// - 请求在后台执行，完成时用任务 ID 判断是否仍是最新测试，避免旧结果覆盖新表单状态。
    pub(in crate::app) fn start_model_profile_test(&mut self, context: &mut Context<Self>) {
        let id = self
            .model_config
            .model_config_form_profile_id
            .clone()
            .unwrap_or_else(|| "unsaved-model-profile".to_string());
        let profile = match self.model_profile_from_form(id) {
            Ok(profile) => profile,
            Err(message) => {
                self.model_config.model_test_status = ModelTestStatus::Failed(message);
                context.notify();
                return;
            }
        };
        let job_id = self.model_config.next_model_test_job_id;
        self.model_config.next_model_test_job_id =
            self.model_config.next_model_test_job_id.saturating_add(1);
        self.model_config.model_test_status = ModelTestStatus::Testing { job_id };
        context.notify();

        context
            .spawn(async move |view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move { test_openai_compatible_model(profile) })
                    .await;
                view.update(app, |view, context| {
                    if view.model_config.model_test_status == (ModelTestStatus::Testing { job_id })
                    {
                        view.model_config.model_test_status = match result {
                            Ok(message) => ModelTestStatus::Success(message),
                            Err(message) => ModelTestStatus::Failed(message),
                        };
                        context.notify();
                    }
                })
                .ok();
            })
            .detach();
    }

    /// 读取快搜关键字输入区当前文本、选择范围和组合文本范围的快照。
    pub(in crate::app) fn quick_search_keywords_text_snapshot(
        &self,
    ) -> SingleLineTextInputSnapshot {
        SingleLineTextInputSnapshot {
            text: self.settings.quick_search_keywords_input.text.clone(),
            selection_range: Self::clamp_search_text_range(
                &self.settings.quick_search_keywords_input.text,
                self.settings
                    .quick_search_keywords_input
                    .selection_range
                    .clone(),
            ),
            marked_range: self
                .settings
                .quick_search_keywords_input
                .marked_range
                .clone(),
            horizontal_scroll_px: self
                .settings
                .quick_search_keywords_input
                .horizontal_scroll_px,
        }
    }

    /// 保存快搜关键字输入区最近一次单行排版结果。
    pub(in crate::app) fn store_quick_search_keywords_layout(
        &mut self,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
        horizontal_scroll_px: f32,
    ) {
        self.settings.quick_search_keywords_last_layout = Some(line);
        self.settings.quick_search_keywords_last_bounds = Some(bounds);
        self.settings
            .quick_search_keywords_input
            .horizontal_scroll_px = horizontal_scroll_px;
    }

    /// 根据鼠标窗口坐标返回快搜关键字输入区中的 UTF-8 字节下标。
    pub(in crate::app) fn quick_search_keywords_index_for_point(
        &self,
        position: Point<Pixels>,
    ) -> usize {
        let text = &self.settings.quick_search_keywords_input.text;
        let (Some(layout), Some(bounds)) = (
            self.settings.quick_search_keywords_last_layout.as_ref(),
            self.settings.quick_search_keywords_last_bounds.as_ref(),
        ) else {
            return text.len();
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return text.len();
        }
        layout
            .closest_index_for_x(
                position.x - bounds.left()
                    + px(self
                        .settings
                        .quick_search_keywords_input
                        .horizontal_scroll_px),
            )
            .min(text.len())
    }

    /// 开始快搜关键字输入区的鼠标选择。
    pub(in crate::app) fn start_quick_search_keywords_mouse_selection(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let index = self.quick_search_keywords_index_for_point(event.position);
        self.settings.quick_search_keywords_input.marked_range = None;
        match event.click_count {
            0 | 1 => {
                if event.modifiers.shift {
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .end = index;
                    self.settings.quick_search_keywords_input.selection_range =
                        Self::clamp_search_text_range(
                            &self.settings.quick_search_keywords_input.text,
                            self.settings
                                .quick_search_keywords_input
                                .selection_range
                                .clone(),
                        );
                } else {
                    self.settings.quick_search_keywords_input.selection_range = index..index;
                }
                self.settings.quick_search_keywords_selection_drag = Some(
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .start,
                );
            }
            2 => {
                self.settings.quick_search_keywords_input.selection_range =
                    Self::search_text_word_range_for_index(
                        &self.settings.quick_search_keywords_input.text,
                        index,
                    );
                self.settings.quick_search_keywords_selection_drag = None;
            }
            _ => {
                self.settings.quick_search_keywords_input.selection_range =
                    0..self.settings.quick_search_keywords_input.text.len();
                self.settings.quick_search_keywords_selection_drag = None;
            }
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 鼠标拖拽时更新快搜关键字输入区选区终点。
    pub(in crate::app) fn update_quick_search_keywords_mouse_selection(
        &mut self,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) {
        let Some(anchor) = self.settings.quick_search_keywords_selection_drag else {
            return;
        };
        let index = self.quick_search_keywords_index_for_point(position);
        self.settings.quick_search_keywords_input.marked_range = None;
        self.settings.quick_search_keywords_input.selection_range = Self::clamp_search_text_range(
            &self.settings.quick_search_keywords_input.text,
            anchor..index,
        );
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 结束快搜关键字输入区鼠标拖拽选择。
    pub(in crate::app) fn finish_quick_search_keywords_mouse_selection(
        &mut self,
        context: &mut Context<Self>,
    ) {
        if self
            .settings
            .quick_search_keywords_selection_drag
            .take()
            .is_some()
        {
            context.notify();
        }
    }

    /// 读取线程日志分析过滤输入区当前文本、选择范围和组合文本范围的快照。
    ///
    /// 业务意图：
    /// - 设置窗口的多行输入元素在绘制阶段只持有 `MainView` 实体，需要通过只读快照拿到稳定文本状态。
    /// - 快照使用规范化后的 LF 文本，确保绘制行数、鼠标命中和后续过滤规则拆分一致。
    pub(in crate::app) fn thread_analysis_filter_text_snapshot(
        &self,
    ) -> (String, Range<usize>, Option<Range<usize>>) {
        (
            self.settings.thread_analysis_filter_text.clone(),
            Self::clamp_search_text_range(
                &self.settings.thread_analysis_filter_text,
                self.settings.thread_analysis_filter_selection_range.clone(),
            ),
            self.settings.thread_analysis_filter_marked_range.clone(),
        )
    }

    /// 保存线程日志分析过滤输入区最近一次多行排版结果。
    ///
    /// 业务意图：
    /// - 鼠标点击和拖拽需要用上一帧真实字形位置换算文本下标；该缓存只服务当前会话，不参与持久化。
    pub(in crate::app) fn store_thread_analysis_filter_text_layouts(
        &mut self,
        layouts: Vec<ThreadAnalysisFilterLineLayout>,
        bounds: Bounds<Pixels>,
    ) {
        self.settings.thread_analysis_filter_last_layouts = layouts;
        self.settings.thread_analysis_filter_last_bounds = Some(bounds);
    }

    /// 返回线程日志分析过滤文本的可视行范围。
    ///
    /// 业务意图：
    /// - 输入区按原始换行展示堆栈；空行也必须占一行，因为空行同时用于分隔多条过滤规则。
    /// - 返回范围不包含换行符本身，便于每行单独排版和命中。
    pub(in crate::app) fn thread_analysis_filter_line_ranges(text: &str) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        let mut start = 0usize;
        for (index, character) in text.char_indices() {
            if character == '\n' {
                ranges.push(start..index);
                start = index + character.len_utf8();
            }
        }
        ranges.push(start..text.len());
        ranges
    }

    /// 返回线程日志分析过滤输入区当前内容需要的可视行数。
    pub(in crate::app) fn thread_analysis_filter_visual_line_count(&self) -> usize {
        Self::thread_analysis_filter_line_ranges(&self.settings.thread_analysis_filter_text)
            .len()
            .max(1)
    }

    /// 开始线程日志分析过滤输入区的鼠标选择。
    ///
    /// 业务意图：
    /// - 单击定位光标，Shift+单击扩展选择，双击选中连续非空白片段，三连击全选，保持和搜索输入框一致的基础文本习惯。
    pub(in crate::app) fn start_thread_analysis_filter_mouse_selection(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let index = self.thread_analysis_filter_index_for_point(event.position);
        self.settings.thread_analysis_filter_marked_range = None;
        match event.click_count {
            0 | 1 => {
                if event.modifiers.shift {
                    self.settings.thread_analysis_filter_selection_range.end = index;
                    self.settings.thread_analysis_filter_selection_range =
                        Self::clamp_search_text_range(
                            &self.settings.thread_analysis_filter_text,
                            self.settings.thread_analysis_filter_selection_range.clone(),
                        );
                } else {
                    self.settings.thread_analysis_filter_selection_range = index..index;
                }
                self.settings.thread_analysis_filter_selection_drag =
                    Some(self.settings.thread_analysis_filter_selection_range.start);
            }
            2 => {
                self.settings.thread_analysis_filter_selection_range =
                    Self::search_text_word_range_for_index(
                        &self.settings.thread_analysis_filter_text,
                        index,
                    );
                self.settings.thread_analysis_filter_selection_drag = None;
            }
            _ => {
                self.settings.thread_analysis_filter_selection_range =
                    0..self.settings.thread_analysis_filter_text.len();
                self.settings.thread_analysis_filter_selection_drag = None;
            }
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 鼠标拖拽时更新线程日志分析过滤输入区选区终点。
    pub(in crate::app) fn update_thread_analysis_filter_mouse_selection(
        &mut self,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) {
        let Some(anchor) = self.settings.thread_analysis_filter_selection_drag else {
            return;
        };
        let index = self.thread_analysis_filter_index_for_point(position);
        self.settings.thread_analysis_filter_marked_range = None;
        self.settings.thread_analysis_filter_selection_range = Self::clamp_search_text_range(
            &self.settings.thread_analysis_filter_text,
            anchor..index,
        );
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 结束线程日志分析过滤输入区鼠标拖拽选择。
    pub(in crate::app) fn finish_thread_analysis_filter_mouse_selection(
        &mut self,
        context: &mut Context<Self>,
    ) {
        if self
            .settings
            .thread_analysis_filter_selection_drag
            .take()
            .is_some()
        {
            context.notify();
        }
    }

    /// 根据鼠标窗口坐标返回线程日志分析过滤输入区中的 UTF-8 字节下标。
    ///
    /// 边界条件：
    /// - 首帧尚未完成排版时回退到文本末尾，避免点击空布局导致越界。
    /// - 点击在整体输入区上方或下方时，分别夹到开头或末尾，符合多行文本框的常见行为。
    pub(in crate::app) fn thread_analysis_filter_index_for_point(
        &self,
        position: Point<Pixels>,
    ) -> usize {
        let Some(bounds) = self.settings.thread_analysis_filter_last_bounds.as_ref() else {
            return self.settings.thread_analysis_filter_text.len();
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.settings.thread_analysis_filter_text.len();
        }
        for layout in &self.settings.thread_analysis_filter_last_layouts {
            if position.y >= layout.bounds.top() && position.y <= layout.bounds.bottom() {
                let local_index = layout
                    .line
                    .closest_index_for_x(position.x - layout.bounds.left())
                    .min(
                        layout
                            .byte_range
                            .end
                            .saturating_sub(layout.byte_range.start),
                    );
                return layout.byte_range.start + local_index;
            }
        }
        self.settings.thread_analysis_filter_text.len()
    }

    /// 进入快搜关键字编辑状态。
    ///
    /// 业务意图：
    /// - 快搜配置默认只读展示，用户明确点击编辑后才允许修改，避免误触键盘或粘贴导致常用关键字被改写。
    /// - 光标放到文本末尾，便于用户继续追加英文逗号和新关键字。
    pub(in crate::app) fn begin_quick_search_keywords_edit(&mut self, context: &mut Context<Self>) {
        if self.settings.quick_search_keywords_is_editing {
            return;
        }
        self.settings.quick_search_keywords_saved_text_before_edit =
            Some(self.settings.quick_search_keywords_input.text.clone());
        self.settings.quick_search_keywords_is_editing = true;
        let cursor = self.settings.quick_search_keywords_input.text.len();
        self.settings.quick_search_keywords_input.selection_range = cursor..cursor;
        self.settings.quick_search_keywords_input.marked_range = None;
        self.settings.quick_search_keywords_selection_drag = None;
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 保存快搜关键字配置并退出编辑状态。
    ///
    /// 业务意图：
    /// - 点击保存后才把单行关键字配置写入配置目录，保证快搜行为只受明确保存过的规则影响。
    /// - 保存前移除换行但保留中文逗号等其它字符，因为用户已确认只有英文逗号具备分隔语义。
    pub(in crate::app) fn save_quick_search_keywords_edit(&mut self, context: &mut Context<Self>) {
        let normalized =
            normalize_quick_search_keywords_text(&self.settings.quick_search_keywords_input.text);
        if normalized != self.settings.quick_search_keywords_input.text {
            self.settings.quick_search_keywords_input.text = normalized;
        }
        self.settings.quick_search_keywords_input.selection_range = Self::clamp_search_text_range(
            &self.settings.quick_search_keywords_input.text,
            self.settings
                .quick_search_keywords_input
                .selection_range
                .clone(),
        );
        self.settings.quick_search_keywords_input.marked_range = None;
        self.settings.quick_search_keywords_selection_drag = None;
        self.settings.quick_search_keywords_is_editing = false;
        self.settings.quick_search_keywords_saved_text_before_edit = None;
        save_quick_search_keywords_preference(&self.settings.quick_search_keywords_input.text);
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 放弃快搜关键字草稿并恢复只读展示。
    ///
    /// 业务意图：
    /// - 设置窗口关闭时如果用户没有点击保存，本次编辑应视为未完成草稿，不能悄悄改变下一次快搜行为。
    pub(in crate::app) fn discard_quick_search_keywords_edit(
        &mut self,
        context: &mut Context<Self>,
    ) {
        if let Some(saved_text) = self
            .settings
            .quick_search_keywords_saved_text_before_edit
            .take()
        {
            self.settings.quick_search_keywords_input.text = saved_text;
        }
        self.settings.quick_search_keywords_is_editing = false;
        self.settings.quick_search_keywords_input.selection_range = Self::clamp_search_text_range(
            &self.settings.quick_search_keywords_input.text,
            self.settings
                .quick_search_keywords_input
                .selection_range
                .clone(),
        );
        self.settings.quick_search_keywords_input.marked_range = None;
        self.settings.quick_search_keywords_selection_drag = None;
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 进入线程日志分析过滤编辑状态。
    ///
    /// 业务意图：
    /// - 设置页默认只读展示过滤堆栈，用户明确点击编辑后才把多行文本框切换为可写，降低误粘贴和输入法误提交风险。
    /// - 光标放到文本末尾，便于用户继续追加新的过滤堆栈；已有选择和组合文本会被清理，避免从只读态遗留不可见编辑上下文。
    pub(in crate::app) fn begin_thread_analysis_filter_edit(
        &mut self,
        context: &mut Context<Self>,
    ) {
        if self.settings.thread_analysis_filter_is_editing {
            return;
        }
        self.settings.thread_analysis_filter_saved_text_before_edit =
            Some(self.settings.thread_analysis_filter_text.clone());
        self.settings.thread_analysis_filter_is_editing = true;
        let cursor = self.settings.thread_analysis_filter_text.len();
        self.settings.thread_analysis_filter_selection_range = cursor..cursor;
        self.settings.thread_analysis_filter_marked_range = None;
        self.settings.thread_analysis_filter_selection_drag = None;
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 保存线程日志分析过滤配置并退出编辑状态。
    ///
    /// 业务意图：
    /// - 用户点击保存时才把当前多行文本写入配置文件，符合“默认只读、显式编辑、显式保存”的设置语义。
    /// - 保存前再次规范化换行，确保从 Windows 粘贴的 CRLF 不会影响后续规则拆分和线程堆栈连续匹配。
    pub(in crate::app) fn save_thread_analysis_filter_edit(&mut self, context: &mut Context<Self>) {
        let normalized =
            normalize_thread_analysis_filter_text(&self.settings.thread_analysis_filter_text);
        if normalized != self.settings.thread_analysis_filter_text {
            self.settings.thread_analysis_filter_text = normalized;
        }
        self.settings.thread_analysis_filter_selection_range = Self::clamp_search_text_range(
            &self.settings.thread_analysis_filter_text,
            self.settings.thread_analysis_filter_selection_range.clone(),
        );
        self.settings.thread_analysis_filter_marked_range = None;
        self.settings.thread_analysis_filter_selection_drag = None;
        self.settings.thread_analysis_filter_is_editing = false;
        self.settings.thread_analysis_filter_saved_text_before_edit = None;
        save_thread_analysis_filter_preference(&self.settings.thread_analysis_filter_text);
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 放弃线程日志分析过滤草稿并恢复只读展示。
    ///
    /// 业务意图：
    /// - 设置窗口关闭时如果用户没有点击保存，本次编辑应视为未完成草稿，不能悄悄改变线程分析行为。
    /// - 恢复进入编辑前的快照，同时清理输入法组合文本和拖拽选择，避免下次打开设置窗口残留编辑态。
    pub(in crate::app) fn discard_thread_analysis_filter_edit(
        &mut self,
        context: &mut Context<Self>,
    ) {
        if let Some(saved_text) = self
            .settings
            .thread_analysis_filter_saved_text_before_edit
            .take()
        {
            self.settings.thread_analysis_filter_text = saved_text;
        }
        self.settings.thread_analysis_filter_is_editing = false;
        self.settings.thread_analysis_filter_selection_range = Self::clamp_search_text_range(
            &self.settings.thread_analysis_filter_text,
            self.settings.thread_analysis_filter_selection_range.clone(),
        );
        self.settings.thread_analysis_filter_marked_range = None;
        self.settings.thread_analysis_filter_selection_drag = None;
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 保存搜索输入框最近一次 GPUI 文本排版结果。
    ///
    /// 业务意图：
    /// - 鼠标点击和拖拽必须根据真实字形宽度转换成文本下标；缓存 `ShapedLine` 后可以复用 GPUI 的命中算法。
    /// - 分开保存关键字和目录输入框，避免两个输入框在同一帧绘制后互相覆盖命中数据。
    pub(in crate::app) fn store_search_text_layout(
        &mut self,
        input_kind: SearchTextInputKind,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
        horizontal_scroll_px: f32,
    ) {
        match input_kind {
            SearchTextInputKind::Query => {
                self.search.search_query_last_layout = Some(line);
                self.search.search_query_last_bounds = Some(bounds);
                if let Some(dialog) = self.search.search_dialog.as_mut() {
                    dialog.query_input.horizontal_scroll_px = horizontal_scroll_px;
                }
            }
            SearchTextInputKind::DirectoryTarget => {
                self.search.search_directory_last_layout = Some(line);
                self.search.search_directory_last_bounds = Some(bounds);
                if let Some(dialog) = self.search.search_dialog.as_mut() {
                    dialog.directory_input.horizontal_scroll_px = horizontal_scroll_px;
                }
            }
        }
    }

    /// 开始搜索输入框的鼠标选择。
    ///
    /// 业务意图：
    /// - 单击定位光标，Shift+单击扩展当前选择，双击选择当前词，三连击选中整段输入。
    /// - 选择逻辑写在 `MainView` 中，保证搜索关键字和目录目标输入框行为一致。
    pub(in crate::app) fn start_search_text_mouse_selection(
        &mut self,
        input_kind: SearchTextInputKind,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let index = self.search_text_index_for_point(input_kind, event.position);
        let Some(dialog) = self.search.search_dialog.as_mut() else {
            return;
        };
        let (text, selection_range, marked_range) = Self::search_text_state_mut(dialog, input_kind);
        *marked_range = None;
        match event.click_count {
            0 | 1 => {
                if event.modifiers.shift {
                    selection_range.end = index;
                    *selection_range = Self::clamp_search_text_range(text, selection_range.clone());
                } else {
                    *selection_range = index..index;
                }
                self.search.search_text_selection_drag = Some((input_kind, selection_range.start));
            }
            2 => {
                *selection_range = Self::search_text_word_range_for_index(text, index);
                self.search.search_text_selection_drag = None;
            }
            _ => {
                *selection_range = 0..text.len();
                self.search.search_text_selection_drag = None;
            }
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 鼠标拖拽时更新搜索输入框选区终点。
    pub(in crate::app) fn update_search_text_mouse_selection(
        &mut self,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) {
        let Some((input_kind, anchor)) = self.search.search_text_selection_drag else {
            return;
        };
        let index = self.search_text_index_for_point(input_kind, position);
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            let (text, selection_range, marked_range) =
                Self::search_text_state_mut(dialog, input_kind);
            *marked_range = None;
            *selection_range = Self::clamp_search_text_range(text, anchor..index);
            self.touch_search_text_cursor_activity();
            context.notify();
        }
    }

    /// 结束搜索输入框鼠标拖拽选择。
    pub(in crate::app) fn finish_search_text_mouse_selection(
        &mut self,
        context: &mut Context<Self>,
    ) {
        if self.search.search_text_selection_drag.take().is_some() {
            context.notify();
        }
    }

    /// 根据鼠标窗口坐标返回搜索输入框中的 UTF-8 字节下标。
    ///
    /// 边界条件：
    /// - 如果输入框尚未完成首次绘制，没有可用字形布局，则回退到文本末尾，避免点击导致越界。
    /// - 如果点击发生在文本区域上下之外，分别夹到开头和末尾，符合单行系统输入框的常见行为。
    pub(in crate::app) fn search_text_index_for_point(
        &self,
        input_kind: SearchTextInputKind,
        position: Point<Pixels>,
    ) -> usize {
        let Some(snapshot) = self.search_text_snapshot(input_kind) else {
            return 0;
        };
        let text = snapshot.text;
        let horizontal_scroll_px = snapshot.horizontal_scroll_px;
        let (layout, bounds) = match input_kind {
            SearchTextInputKind::Query => (
                self.search.search_query_last_layout.as_ref(),
                self.search.search_query_last_bounds.as_ref(),
            ),
            SearchTextInputKind::DirectoryTarget => (
                self.search.search_directory_last_layout.as_ref(),
                self.search.search_directory_last_bounds.as_ref(),
            ),
        };
        let (Some(layout), Some(bounds)) = (layout, bounds) else {
            return text.len();
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return text.len();
        }
        layout
            .closest_index_for_x(position.x - bounds.left() + px(horizontal_scroll_px))
            .min(text.len())
    }

    /// 返回双击时应选择的输入词范围。
    ///
    /// 业务意图：
    /// - 搜索关键字和目录路径中都可能出现中文、英文、数字和路径分隔符；双击选择连续非空白片段更符合搜索场景。
    /// - 如果点击在空白上，则选择连续空白，和常见文本输入框行为保持一致。
    pub(in crate::app) fn search_text_word_range_for_index(
        text: &str,
        index: usize,
    ) -> Range<usize> {
        if text.is_empty() {
            return 0..0;
        }
        let index = Self::clamp_search_text_range(text, index..index).start;
        let current = text[index..]
            .chars()
            .next()
            .or_else(|| text[..index].chars().next_back());
        let Some(current) = current else {
            return 0..0;
        };
        let select_whitespace = current.is_whitespace();
        let mut start = 0usize;
        for (byte_index, character) in text[..index].char_indices().rev() {
            if character.is_whitespace() != select_whitespace {
                start = byte_index + character.len_utf8();
                break;
            }
        }
        let mut end = text.len();
        for (relative_index, character) in text[index..].char_indices() {
            if character.is_whitespace() != select_whitespace {
                end = index + relative_index;
                break;
            }
        }
        start..end
    }

    /// 标记搜索输入框光标刚发生用户活动。
    ///
    /// 业务意图：
    /// - 输入、点击定位、拖拽和方向键移动都会改变用户对光标位置的关注点；这些动作之后光标需要保持常亮 1 秒。
    /// - 集中更新时间戳，避免键盘、鼠标和 IME 提交路径出现不同的闪烁节奏。
    pub(in crate::app) fn touch_search_text_cursor_activity(&mut self) {
        self.search.search_text_cursor_last_activity = Instant::now();
    }

    /// 判断当前输入框光标在本帧是否应显示。
    ///
    /// 业务意图：
    /// - 搜索输入框现在由 GPUI 文本元素绘制，不再使用普通 `div().with_animation(...)` 光标。
    /// - 输入或移动发生后的极短时间内保持可见，随后立即按 500ms 亮、500ms 灭的节奏闪烁。
    ///
    /// 边界条件：
    /// - `Instant` 是单调时间，适合处理系统时间调整、时区变化或休眠恢复后的相对时间判断。
    pub(in crate::app) fn search_text_cursor_visible(&self) -> bool {
        let elapsed = self.search.search_text_cursor_last_activity.elapsed();
        Self::search_text_cursor_visible_for_elapsed(elapsed)
    }

    /// 根据距离最近一次光标活动的时间计算光标可见性。
    ///
    /// 业务意图：
    /// - 将时间规则拆成纯函数，便于单元测试锁定“活动后 1 秒常亮，之后闪烁”的产品行为。
    pub(in crate::app) fn search_text_cursor_visible_for_elapsed(elapsed: Duration) -> bool {
        if elapsed < Duration::from_millis(120) {
            return true;
        }
        elapsed.as_millis() % 1000 < 500
    }

    /// 处理搜索对话框中任一文本输入框的基础编辑按键。
    ///
    /// 业务意图：
    /// - 查询词和目录目标都只需要单行文本能力，复用同一套退格、删除和组合文本清理逻辑可避免两处行为不一致。
    /// - 普通字符输入交给 `EntityInputHandler`，这里不处理 `key_char`，从而保留中文 IME 的平台提交路径。
    pub(in crate::app) fn handle_search_text_key_down(
        &mut self,
        input_kind: SearchTextInputKind,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_paste_keystroke(&event.keystroke) {
            let clipboard_text = context
                .read_from_clipboard()
                .and_then(|item| item.text())
                .map(|text| Self::sanitize_search_input_text(&text));
            if let Some(text) = clipboard_text {
                let text_is_not_empty = !text.is_empty();
                if text_is_not_empty {
                    let mut should_jump_from_top = false;
                    if let Some(dialog) = self.search.search_dialog.as_mut() {
                        if input_kind == SearchTextInputKind::Query {
                            dialog.query_history_menu_open = false;
                        }
                        let (input_text, selection_range, marked_range) =
                            Self::search_text_state_mut(dialog, input_kind);
                        Self::replace_search_text_selection(
                            input_text,
                            selection_range,
                            marked_range,
                            &text,
                        );
                        if input_kind == SearchTextInputKind::Query {
                            dialog.current_file_match_count = None;
                            dialog.current_file_navigation_match = None;
                            should_jump_from_top = true;
                        }
                    }
                    self.touch_search_text_cursor_activity();
                    if should_jump_from_top {
                        self.jump_search_query_in_current_file(true, context);
                    }
                    context.stop_propagation();
                    context.notify();
                    return;
                }
            }
            context.stop_propagation();
            return;
        }

        let Some(dialog) = self.search.search_dialog.as_mut() else {
            return;
        };
        if input_kind == SearchTextInputKind::Query {
            dialog.query_history_menu_open = false;
        }
        let (text, selection_range, marked_range) = Self::search_text_state_mut(dialog, input_kind);

        if Self::is_copy_keystroke(&event.keystroke) {
            if selection_range.start != selection_range.end {
                let range = Self::clamp_search_text_range(text, selection_range.clone());
                if range.start < range.end {
                    context.write_to_clipboard(ClipboardItem::new_string(text[range].to_string()));
                }
            }
            context.stop_propagation();
            return;
        }

        if Self::is_select_all_keystroke(&event.keystroke) {
            *marked_range = None;
            *selection_range = 0..text.len();
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return;
        }

        match event.keystroke.key.as_str() {
            "left" => {
                *marked_range = None;
                if event.keystroke.modifiers.shift {
                    selection_range.end =
                        Self::previous_search_text_boundary(text, selection_range.end);
                    *selection_range = Self::clamp_search_text_range(text, selection_range.clone());
                } else if selection_range.start != selection_range.end {
                    *selection_range = selection_range.start..selection_range.start;
                } else {
                    let cursor = Self::previous_search_text_boundary(text, selection_range.end);
                    *selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "right" => {
                *marked_range = None;
                if event.keystroke.modifiers.shift {
                    selection_range.end =
                        Self::next_search_text_boundary(text, selection_range.end);
                    *selection_range = Self::clamp_search_text_range(text, selection_range.clone());
                } else if selection_range.start != selection_range.end {
                    *selection_range = selection_range.end..selection_range.end;
                } else {
                    let cursor = Self::next_search_text_boundary(text, selection_range.end);
                    *selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "up" => {
                *marked_range = None;
                *selection_range = 0..0;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "down" => {
                *marked_range = None;
                let cursor = text.len();
                *selection_range = cursor..cursor;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "backspace" => {
                if let Some(range) = marked_range.take().or_else(|| {
                    (selection_range.start != selection_range.end).then(|| selection_range.clone())
                }) {
                    text.replace_range(range.clone(), "");
                    *selection_range = range.start..range.start;
                } else if let Some((previous_index, _)) =
                    text[..selection_range.end].char_indices().next_back()
                {
                    text.replace_range(previous_index..selection_range.end, "");
                    *selection_range = previous_index..previous_index;
                }
                if input_kind == SearchTextInputKind::Query {
                    self.clear_search_current_file_match_count();
                    self.jump_search_query_in_current_file(true, context);
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "delete" => {
                if let Some(range) = marked_range.take().or_else(|| {
                    (selection_range.start != selection_range.end).then(|| selection_range.clone())
                }) {
                    text.replace_range(range.clone(), "");
                    *selection_range = range.start..range.start;
                } else if let Some((next_index, next_character)) =
                    text[selection_range.end..].char_indices().next()
                {
                    let start = selection_range.end + next_index;
                    let end = start + next_character.len_utf8();
                    text.replace_range(start..end, "");
                    *selection_range = start..start;
                }
                if input_kind == SearchTextInputKind::Query {
                    self.clear_search_current_file_match_count();
                    self.jump_search_query_in_current_file(true, context);
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "enter" | "escape" => {
                // Enter 和 Escape 由全局快捷键处理，保持搜索框只负责文本编辑。
            }
            _ => {}
        }
    }

    /// 处理快搜关键字输入区的基础编辑按键。
    ///
    /// 业务意图：
    /// - 快搜配置是单行英文逗号分隔文本，默认只读；编辑态才允许粘贴、剪切、删除和普通 IME 提交。
    /// - 只读态仍允许复制、全选和方向键移动，方便用户核对当前已保存关键字。
    pub(in crate::app) fn handle_quick_search_keywords_key_down(
        &mut self,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_paste_keystroke(&event.keystroke) {
            if self.settings.quick_search_keywords_is_editing
                && let Some(text) = context.read_from_clipboard().and_then(|item| item.text())
            {
                let replacement = Self::sanitize_search_input_text(&text);
                if !replacement.is_empty() {
                    self.replace_quick_search_keywords_selection(&replacement);
                    self.touch_search_text_cursor_activity();
                    context.notify();
                }
            }
            context.stop_propagation();
            return;
        }

        if Self::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_quick_search_keywords_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return;
        }

        if Self::is_cut_keystroke(&event.keystroke) {
            if self.settings.quick_search_keywords_is_editing
                && let Some(text) = self.selected_quick_search_keywords_text()
            {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                self.replace_quick_search_keywords_selection("");
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if Self::is_select_all_keystroke(&event.keystroke) {
            self.settings.quick_search_keywords_input.marked_range = None;
            self.settings.quick_search_keywords_input.selection_range =
                0..self.settings.quick_search_keywords_input.text.len();
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return;
        }

        match event.keystroke.key.as_str() {
            "left" => {
                self.settings.quick_search_keywords_input.marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .end = Self::previous_search_text_boundary(
                        &self.settings.quick_search_keywords_input.text,
                        self.settings
                            .quick_search_keywords_input
                            .selection_range
                            .end,
                    );
                    self.settings.quick_search_keywords_input.selection_range =
                        Self::clamp_search_text_range(
                            &self.settings.quick_search_keywords_input.text,
                            self.settings
                                .quick_search_keywords_input
                                .selection_range
                                .clone(),
                        );
                } else if self
                    .settings
                    .quick_search_keywords_input
                    .selection_range
                    .start
                    != self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end
                {
                    self.settings.quick_search_keywords_input.selection_range = self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .start
                        ..self
                            .settings
                            .quick_search_keywords_input
                            .selection_range
                            .start;
                } else {
                    let cursor = Self::previous_search_text_boundary(
                        &self.settings.quick_search_keywords_input.text,
                        self.settings
                            .quick_search_keywords_input
                            .selection_range
                            .end,
                    );
                    self.settings.quick_search_keywords_input.selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "right" => {
                self.settings.quick_search_keywords_input.marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .end = Self::next_search_text_boundary(
                        &self.settings.quick_search_keywords_input.text,
                        self.settings
                            .quick_search_keywords_input
                            .selection_range
                            .end,
                    );
                    self.settings.quick_search_keywords_input.selection_range =
                        Self::clamp_search_text_range(
                            &self.settings.quick_search_keywords_input.text,
                            self.settings
                                .quick_search_keywords_input
                                .selection_range
                                .clone(),
                        );
                } else if self
                    .settings
                    .quick_search_keywords_input
                    .selection_range
                    .start
                    != self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end
                {
                    self.settings.quick_search_keywords_input.selection_range = self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end
                        ..self
                            .settings
                            .quick_search_keywords_input
                            .selection_range
                            .end;
                } else {
                    let cursor = Self::next_search_text_boundary(
                        &self.settings.quick_search_keywords_input.text,
                        self.settings
                            .quick_search_keywords_input
                            .selection_range
                            .end,
                    );
                    self.settings.quick_search_keywords_input.selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "up" => {
                self.settings.quick_search_keywords_input.marked_range = None;
                self.settings.quick_search_keywords_input.selection_range = 0..0;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "down" => {
                self.settings.quick_search_keywords_input.marked_range = None;
                let cursor = self.settings.quick_search_keywords_input.text.len();
                self.settings.quick_search_keywords_input.selection_range = cursor..cursor;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "backspace" => {
                if !self.settings.quick_search_keywords_is_editing {
                    context.stop_propagation();
                    return;
                }
                if self
                    .settings
                    .quick_search_keywords_input
                    .selection_range
                    .start
                    != self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end
                    || self
                        .settings
                        .quick_search_keywords_input
                        .marked_range
                        .is_some()
                {
                    self.replace_quick_search_keywords_selection("");
                } else if let Some((previous_index, _)) =
                    self.settings.quick_search_keywords_input.text[..self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end]
                        .char_indices()
                        .next_back()
                {
                    let cursor = self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end;
                    self.settings
                        .quick_search_keywords_input
                        .text
                        .replace_range(previous_index..cursor, "");
                    self.settings.quick_search_keywords_input.selection_range =
                        previous_index..previous_index;
                    self.settings.quick_search_keywords_input.marked_range = None;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "delete" => {
                if !self.settings.quick_search_keywords_is_editing {
                    context.stop_propagation();
                    return;
                }
                if self
                    .settings
                    .quick_search_keywords_input
                    .selection_range
                    .start
                    != self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end
                    || self
                        .settings
                        .quick_search_keywords_input
                        .marked_range
                        .is_some()
                {
                    self.replace_quick_search_keywords_selection("");
                } else if let Some((next_index, next_character)) =
                    self.settings.quick_search_keywords_input.text[self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end..]
                        .char_indices()
                        .next()
                {
                    let start = self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end
                        + next_index;
                    let end = start + next_character.len_utf8();
                    self.settings
                        .quick_search_keywords_input
                        .text
                        .replace_range(start..end, "");
                    self.settings.quick_search_keywords_input.selection_range = start..start;
                    self.settings.quick_search_keywords_input.marked_range = None;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "enter" => {
                context.stop_propagation();
            }
            "escape" => {}
            _ => {}
        }
    }

    /// 返回快搜关键字输入区当前选中文本。
    pub(in crate::app) fn selected_quick_search_keywords_text(&self) -> Option<String> {
        let range = Self::clamp_search_text_range(
            &self.settings.quick_search_keywords_input.text,
            self.settings
                .quick_search_keywords_input
                .selection_range
                .clone(),
        );
        (range.start < range.end)
            .then(|| self.settings.quick_search_keywords_input.text[range].to_string())
    }

    /// 用给定文本替换快搜关键字输入区当前选区。
    pub(in crate::app) fn replace_quick_search_keywords_selection(&mut self, replacement: &str) {
        let replacement = normalize_quick_search_keywords_text(replacement);
        let range = self
            .settings
            .quick_search_keywords_input
            .marked_range
            .take()
            .unwrap_or_else(|| {
                Self::clamp_search_text_range(
                    &self.settings.quick_search_keywords_input.text,
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .clone(),
                )
            });
        self.settings
            .quick_search_keywords_input
            .text
            .replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        self.settings.quick_search_keywords_input.selection_range = cursor..cursor;
    }

    /// 处理线程日志分析过滤多行输入区的基础编辑按键。
    ///
    /// 业务意图：
    /// - 该输入区用于粘贴完整线程堆栈，必须保留换行，并支持复制、剪切、粘贴、删除、全选和回车换行。
    /// - 设置页默认只读展示过滤规则，因此只有编辑态才允许修改文本；只读态仍允许选中和复制，方便用户核对内置规则。
    /// - 普通字符输入交给 `EntityInputHandler`，这里不处理 `key_char`，从而保留中文 IME 的平台提交路径。
    pub(in crate::app) fn handle_thread_analysis_filter_key_down(
        &mut self,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_paste_keystroke(&event.keystroke) {
            if self.settings.thread_analysis_filter_is_editing
                && let Some(text) = context.read_from_clipboard().and_then(|item| item.text())
            {
                let replacement = normalize_thread_analysis_filter_text(&text);
                self.replace_thread_analysis_filter_selection(&replacement);
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
                return;
            }
            context.stop_propagation();
            return;
        }

        if Self::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_thread_analysis_filter_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return;
        }

        if Self::is_cut_keystroke(&event.keystroke) {
            if self.settings.thread_analysis_filter_is_editing
                && let Some(text) = self.selected_thread_analysis_filter_text()
            {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                self.replace_thread_analysis_filter_selection("");
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if Self::is_select_all_keystroke(&event.keystroke) {
            self.settings.thread_analysis_filter_marked_range = None;
            self.settings.thread_analysis_filter_selection_range =
                0..self.settings.thread_analysis_filter_text.len();
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return;
        }

        match event.keystroke.key.as_str() {
            "left" => {
                self.settings.thread_analysis_filter_marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.settings.thread_analysis_filter_selection_range.end =
                        Self::previous_search_text_boundary(
                            &self.settings.thread_analysis_filter_text,
                            self.settings.thread_analysis_filter_selection_range.end,
                        );
                    self.settings.thread_analysis_filter_selection_range =
                        Self::clamp_search_text_range(
                            &self.settings.thread_analysis_filter_text,
                            self.settings.thread_analysis_filter_selection_range.clone(),
                        );
                } else if self.settings.thread_analysis_filter_selection_range.start
                    != self.settings.thread_analysis_filter_selection_range.end
                {
                    self.settings.thread_analysis_filter_selection_range =
                        self.settings.thread_analysis_filter_selection_range.start
                            ..self.settings.thread_analysis_filter_selection_range.start;
                } else {
                    let cursor = Self::previous_search_text_boundary(
                        &self.settings.thread_analysis_filter_text,
                        self.settings.thread_analysis_filter_selection_range.end,
                    );
                    self.settings.thread_analysis_filter_selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "right" => {
                self.settings.thread_analysis_filter_marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.settings.thread_analysis_filter_selection_range.end =
                        Self::next_search_text_boundary(
                            &self.settings.thread_analysis_filter_text,
                            self.settings.thread_analysis_filter_selection_range.end,
                        );
                    self.settings.thread_analysis_filter_selection_range =
                        Self::clamp_search_text_range(
                            &self.settings.thread_analysis_filter_text,
                            self.settings.thread_analysis_filter_selection_range.clone(),
                        );
                } else if self.settings.thread_analysis_filter_selection_range.start
                    != self.settings.thread_analysis_filter_selection_range.end
                {
                    self.settings.thread_analysis_filter_selection_range =
                        self.settings.thread_analysis_filter_selection_range.end
                            ..self.settings.thread_analysis_filter_selection_range.end;
                } else {
                    let cursor = Self::next_search_text_boundary(
                        &self.settings.thread_analysis_filter_text,
                        self.settings.thread_analysis_filter_selection_range.end,
                    );
                    self.settings.thread_analysis_filter_selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "up" => {
                self.settings.thread_analysis_filter_marked_range = None;
                self.settings.thread_analysis_filter_selection_range = 0..0;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "down" => {
                self.settings.thread_analysis_filter_marked_range = None;
                let cursor = self.settings.thread_analysis_filter_text.len();
                self.settings.thread_analysis_filter_selection_range = cursor..cursor;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "backspace" => {
                if !self.settings.thread_analysis_filter_is_editing {
                    context.stop_propagation();
                    return;
                }
                if self.settings.thread_analysis_filter_selection_range.start
                    != self.settings.thread_analysis_filter_selection_range.end
                    || self.settings.thread_analysis_filter_marked_range.is_some()
                {
                    self.replace_thread_analysis_filter_selection("");
                } else if let Some((previous_index, _)) = self.settings.thread_analysis_filter_text
                    [..self.settings.thread_analysis_filter_selection_range.end]
                    .char_indices()
                    .next_back()
                {
                    let cursor = self.settings.thread_analysis_filter_selection_range.end;
                    self.settings
                        .thread_analysis_filter_text
                        .replace_range(previous_index..cursor, "");
                    self.settings.thread_analysis_filter_selection_range =
                        previous_index..previous_index;
                    self.settings.thread_analysis_filter_marked_range = None;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "delete" => {
                if !self.settings.thread_analysis_filter_is_editing {
                    context.stop_propagation();
                    return;
                }
                if self.settings.thread_analysis_filter_selection_range.start
                    != self.settings.thread_analysis_filter_selection_range.end
                    || self.settings.thread_analysis_filter_marked_range.is_some()
                {
                    self.replace_thread_analysis_filter_selection("");
                } else if let Some((next_index, next_character)) =
                    self.settings.thread_analysis_filter_text
                        [self.settings.thread_analysis_filter_selection_range.end..]
                        .char_indices()
                        .next()
                {
                    let start =
                        self.settings.thread_analysis_filter_selection_range.end + next_index;
                    let end = start + next_character.len_utf8();
                    self.settings
                        .thread_analysis_filter_text
                        .replace_range(start..end, "");
                    self.settings.thread_analysis_filter_selection_range = start..start;
                    self.settings.thread_analysis_filter_marked_range = None;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "enter" => {
                if !self.settings.thread_analysis_filter_is_editing {
                    context.stop_propagation();
                    return;
                }
                self.replace_thread_analysis_filter_selection("\n");
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "escape" => {}
            _ => {}
        }
    }

    /// 返回线程日志分析过滤输入区当前选中文本。
    pub(in crate::app) fn selected_thread_analysis_filter_text(&self) -> Option<String> {
        let range = Self::clamp_search_text_range(
            &self.settings.thread_analysis_filter_text,
            self.settings.thread_analysis_filter_selection_range.clone(),
        );
        (range.start < range.end)
            .then(|| self.settings.thread_analysis_filter_text[range].to_string())
    }

    /// 用给定文本替换线程日志分析过滤输入区当前选区。
    ///
    /// 业务意图：
    /// - 平台 IME、快捷键粘贴和普通编辑都通过同一函数更新文本、组合范围和光标，保证多行输入状态一致。
    pub(in crate::app) fn replace_thread_analysis_filter_selection(&mut self, replacement: &str) {
        let replacement = normalize_thread_analysis_filter_text(replacement);
        let range = self
            .settings
            .thread_analysis_filter_marked_range
            .take()
            .unwrap_or_else(|| {
                Self::clamp_search_text_range(
                    &self.settings.thread_analysis_filter_text,
                    self.settings.thread_analysis_filter_selection_range.clone(),
                )
            });
        self.settings
            .thread_analysis_filter_text
            .replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        self.settings.thread_analysis_filter_selection_range = cursor..cursor;
    }

    /// 返回指定输入槽位的可变文本、选择范围和组合范围。
    ///
    /// 实现原因：
    /// - Rust 需要在同一分支中同时借用三个字段，封装后可以让按键处理和 IME 回调共享同一套字段选择逻辑。
    pub(in crate::app) fn search_text_state_mut(
        dialog: &mut SearchDialogState,
        input_kind: SearchTextInputKind,
    ) -> (&mut String, &mut Range<usize>, &mut Option<Range<usize>>) {
        match input_kind {
            SearchTextInputKind::Query => (
                &mut dialog.query_input.text,
                &mut dialog.query_input.selection_range,
                &mut dialog.query_input.marked_range,
            ),
            SearchTextInputKind::DirectoryTarget => (
                &mut dialog.directory_input.text,
                &mut dialog.directory_input.selection_range,
                &mut dialog.directory_input.marked_range,
            ),
        }
    }

    /// 返回指定输入槽位的只读文本、选择范围和组合范围。
    pub(in crate::app) fn search_text_state(
        dialog: &SearchDialogState,
        input_kind: SearchTextInputKind,
    ) -> (&str, Range<usize>, Option<Range<usize>>, f32) {
        match input_kind {
            SearchTextInputKind::Query => (
                &dialog.query_input.text,
                dialog.query_input.selection_range.clone(),
                dialog.query_input.marked_range.clone(),
                dialog.query_input.horizontal_scroll_px,
            ),
            SearchTextInputKind::DirectoryTarget => (
                &dialog.directory_input.text,
                dialog.directory_input.selection_range.clone(),
                dialog.directory_input.marked_range.clone(),
                dialog.directory_input.horizontal_scroll_px,
            ),
        }
    }

    /// 用给定文本替换搜索输入框当前选区。
    ///
    /// 业务意图：
    /// - 平台 IME 提交、快捷键粘贴和日志查看器粘贴到搜索框都需要同一套替换规则。
    /// - 统一处理组合文本、选区和光标位置，可以避免关键字输入框与目录输入框行为不一致。
    pub(in crate::app) fn replace_search_text_selection(
        text: &mut String,
        selection_range: &mut Range<usize>,
        marked_range: &mut Option<Range<usize>>,
        replacement: &str,
    ) {
        let range = marked_range
            .take()
            .unwrap_or_else(|| Self::clamp_search_text_range(text, selection_range.clone()));
        text.replace_range(range.clone(), replacement);
        let cursor = range.start + replacement.len();
        *selection_range = cursor..cursor;
    }

    /// 用剪贴板文本覆盖搜索关键字。
    ///
    /// 业务意图：
    /// - 日志查看器是只读区域，`Ctrl+V` 的产品语义是把剪贴板内容作为新的搜索关键字，而不是继续编辑
    ///   `prepare_search_dialog_state` 可能根据日志选区预填出来的旧文本。
    /// - 封装为纯状态辅助函数，便于测试“覆盖而非追加”的关键边界。
    pub(in crate::app) fn replace_search_query_with_clipboard_text(
        dialog: &mut SearchDialogState,
        text: String,
    ) {
        let cursor = text.len();
        dialog.query_input.text = text;
        dialog.query_input.selection_range = cursor..cursor;
        dialog.query_input.marked_range = None;
        dialog.query_input.horizontal_scroll_px = 0.0;
        dialog.query_history_menu_open = false;
        dialog.current_file_match_count = None;
        dialog.current_file_navigation_match = None;
        dialog.message = "已粘贴剪贴板文本，按 Enter 或点击搜索".to_string();
    }

    /// 计算单行自绘输入框的水平滚动偏移。
    ///
    /// 业务意图：
    /// - 搜索关键字、当前目录、模型配置和快搜关键字都使用完整文本排版后裁剪显示；当光标移动到可视区域外时，
    ///   需要调整绘制偏移，让光标重新回到输入框内，体验与系统单行输入框一致。
    ///
    /// 边界条件：
    /// - 内容宽度加上光标宽度仍能完整显示时始终返回 0，避免短文本出现非零偏移。
    /// - 输入框未聚焦时只夹紧已有偏移，不主动跟随光标，避免非编辑态文本在重绘时跳动。
    /// - 文本末尾允许额外滚出“右侧边距 + 光标宽度”的空白，确保光标停在最后一个字符后面时不会被裁剪。
    pub(in crate::app) fn single_line_horizontal_scroll_offset(
        current_scroll_px: f32,
        cursor_x: Pixels,
        content_width: Pixels,
        viewport_width: Pixels,
        keep_cursor_visible: bool,
    ) -> f32 {
        let viewport_width = f32::from(viewport_width).max(0.0);
        let content_width = f32::from(content_width).max(0.0);
        let caret_width = SINGLE_LINE_INPUT_CARET_WIDTH.min(viewport_width);
        if viewport_width <= 0.0 || content_width + caret_width <= viewport_width {
            return 0.0;
        }

        let margin = SINGLE_LINE_INPUT_SCROLL_MARGIN.min(viewport_width / 2.0);
        let right_guard = (margin + caret_width).min(viewport_width);
        let max_scroll = (content_width + right_guard - viewport_width).max(0.0);
        let mut scroll = current_scroll_px.clamp(0.0, max_scroll);
        if !keep_cursor_visible {
            return scroll;
        }

        let cursor_x = f32::from(cursor_x).clamp(0.0, content_width);
        let visible_left = scroll + margin;
        let visible_right = scroll + viewport_width - right_guard;
        if cursor_x < visible_left {
            scroll = (cursor_x - margin).max(0.0);
        } else if cursor_x > visible_right {
            scroll = (cursor_x - viewport_width + right_guard).min(max_scroll);
        }

        scroll
    }

    /// 判断是否为搜索输入框全选快捷键。
    ///
    /// 业务意图：
    /// - macOS 使用 `Cmd+A`，Windows 使用 `Ctrl+A`，部分输入路径会把 `Ctrl+A` 编码成 ASCII 控制字符。
    /// - 搜索关键字和目录输入框必须统一识别这些形态，避免只能输入却不能选择已有内容。
    pub(in crate::app) fn is_select_all_keystroke(keystroke: &Keystroke) -> bool {
        Self::keystroke_matches_letter_or_control_code(keystroke, "a", CONTROL_A_CODE)
            && (keystroke.modifiers.control
                || keystroke.modifiers.platform
                || Self::keystroke_matches_control_code(keystroke, CONTROL_A_CODE))
    }

    /// 将搜索输入框内部 UTF-8 范围夹到合法字符边界。
    ///
    /// 边界条件：
    /// - 鼠标命中、平台输入和快捷键都可能给出超过文本长度或反向的范围。
    /// - Rust `String::replace_range` 只能接受 UTF-8 字符边界，因此这里统一转换为最近的安全边界。
    pub(in crate::app) fn clamp_search_text_range(text: &str, range: Range<usize>) -> Range<usize> {
        let start = Self::search_input_clamp_byte_index(text, range.start);
        let end = Self::search_input_clamp_byte_index(text, range.end);
        start.min(end)..start.max(end)
    }

    /// 将任意字节下标夹到搜索输入文本的 UTF-8 字符边界。
    pub(in crate::app) fn search_input_clamp_byte_index(text: &str, index: usize) -> usize {
        if index >= text.len() {
            return text.len();
        }
        if text.is_char_boundary(index) {
            return index;
        }
        text.char_indices()
            .map(|(byte_index, _)| byte_index)
            .take_while(|byte_index| *byte_index < index)
            .last()
            .unwrap_or(0)
    }

    /// 返回当前光标左侧的前一个 UTF-8 字符边界。
    ///
    /// 业务意图：
    /// - 方向键移动必须按用户可见字符边界前进，不能把中文、emoji 或其它多字节字符切成非法 `String` 范围。
    pub(in crate::app) fn previous_search_text_boundary(text: &str, offset: usize) -> usize {
        let offset = Self::search_input_clamp_byte_index(text, offset);
        text[..offset]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    /// 返回当前光标右侧的下一个 UTF-8 字符边界。
    pub(in crate::app) fn next_search_text_boundary(text: &str, offset: usize) -> usize {
        let offset = Self::search_input_clamp_byte_index(text, offset);
        text[offset..]
            .char_indices()
            .nth(1)
            .map(|(index, _)| offset + index)
            .unwrap_or(text.len())
    }

    /// 根据当前窗口焦点判断平台输入应写入哪个搜索文本框。
    ///
    /// 边界条件：
    /// - 如果两个输入框都未聚焦，默认写入查询词，保证 `Ctrl+F` 打开后立即输入仍符合用户预期。
    pub(in crate::app) fn active_search_text_input_kind(
        &self,
        window: &Window,
    ) -> SearchTextInputKind {
        if self.search.search_directory_focus.is_focused(window) {
            SearchTextInputKind::DirectoryTarget
        } else {
            SearchTextInputKind::Query
        }
    }

    /// 将 UTF-16 范围转换为搜索查询词内部可安全切片的 UTF-8 字节范围。
    ///
    /// 业务意图：
    /// - 平台输入协议按 UTF-16 字符计数，Rust `String` 必须按 UTF-8 字节边界切片。
    /// - 所有中文输入、组合文本替换和候选词提交都必须通过该转换，避免把多字节字符切坏。
    pub(in crate::app) fn search_input_range_from_utf16(
        query: &str,
        range_utf16: Range<usize>,
    ) -> Range<usize> {
        single_line_range_from_utf16(query, range_utf16)
    }

    /// 将搜索查询词内部 UTF-8 字节范围转换为平台输入协议需要的 UTF-16 范围。
    pub(in crate::app) fn search_input_range_to_utf16(
        query: &str,
        range: Range<usize>,
    ) -> Range<usize> {
        single_line_range_to_utf16(query, range)
    }

    /// 把 UTF-8 字节边界映射到 UTF-16 偏移。
    pub(in crate::app) fn search_input_utf16_offset_from_byte(
        query: &str,
        byte_index: usize,
    ) -> usize {
        single_line_utf16_offset_from_byte(query, byte_index)
    }

    /// 清理平台输入文本，确保搜索框保持单行普通文本。
    pub(in crate::app) fn sanitize_search_input_text(text: &str) -> String {
        text.replace(['\n', '\r'], "")
    }

    /// 渲染搜索选项复选框。
    pub(in crate::app) fn render_checkbox(
        checked: bool,
        palette: AppThemePalette,
    ) -> gpui::Stateful<gpui::Div> {
        let checkbox = div()
            .id("search-checkbox")
            .flex()
            .items_center()
            .justify_center()
            .w(px(14.0))
            .h(px(14.0))
            .rounded(px(3.0))
            .border_1()
            .border_color(rgb(if checked {
                palette.accent
            } else {
                palette.border
            }))
            .bg(rgb(if checked {
                palette.accent
            } else {
                palette.input
            }));

        if checked {
            checkbox.child(Self::render_lucide_icon(
                Some(Icon::Check),
                10.0,
                10.0,
                palette.on_accent,
            ))
        } else {
            checkbox
        }
    }

    /// 判断搜索按钮是否具备基本启动条件。
    pub(in crate::app) fn search_can_start(&self, dialog: &SearchDialogState) -> bool {
        !dialog.query_input.text.trim().is_empty() && self.log.active_tab_id.is_some()
    }

    /// 判断快搜按钮是否具备基本启动条件。
    ///
    /// 业务意图：
    /// - 快搜不依赖搜索输入框内容，但必须存在已保存的有效关键字，并且当前不能已有搜索任务运行。
    /// - 当前 tab 的加载状态仍在启动时给出具体提示，按钮这里只做轻量可用性判断。
    pub(in crate::app) fn quick_search_can_start(&self, dialog: &SearchDialogState) -> bool {
        !dialog.is_searching
            && self.log.active_tab_id.is_some()
            && !self.effective_quick_search_keywords().is_empty()
    }

    /// 判断当前文件计数按钮是否可用。
    ///
    /// 业务意图：
    /// - 计数按钮只统计当前已经打开并成功解码的活动文件，不触发后台读取，也不扫描当前目录。
    /// - 空关键字没有统计意义；加载中或失败 tab 也不能提供可靠计数。
    pub(in crate::app) fn search_can_count_current_file(&self, dialog: &SearchDialogState) -> bool {
        !dialog.query_input.text.trim().is_empty() && self.active_log_tab_document().is_some()
    }

    /// 返回当前激活 tab 的文档。
    ///
    /// 边界条件：
    /// - 没有活动 tab、tab 已关闭、仍在加载或打开失败时都返回 `None`，调用方据此展示不可用状态。
    pub(in crate::app) fn active_log_tab_document(&self) -> Option<LogTabDocument> {
        let active_tab_id = self.log.active_tab_id?;
        let active_tab = self
            .log
            .open_tabs
            .iter()
            .find(|tab| tab.id == active_tab_id)?;
        match &active_tab.state {
            LogTabState::Ready { document } => Some(document.as_ref().clone()),
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => None,
        }
    }

    /// 返回当前激活 tab 的来源和文档。
    ///
    /// 业务意图：
    /// - 搜索框的“输入即定位”和“下一个”需要构造可复用的 `SearchResultItem`，其中既包含当前文档正文，
    ///   也包含文件来源、展示名称和稳定键。
    ///
    /// 边界条件：
    /// - 没有活动 tab、tab 仍在加载或打开失败时返回 `None`；调用方应保持按钮不可用或展示中文提示。
    pub(in crate::app) fn active_log_tab_source_and_document(
        &self,
    ) -> Option<(LogFileSource, LogTabDocument)> {
        let active_tab_id = self.log.active_tab_id?;
        let active_tab = self
            .log
            .open_tabs
            .iter()
            .find(|tab| tab.id == active_tab_id)?;
        match &active_tab.state {
            LogTabState::Ready { document } => {
                Some((active_tab.source.clone(), document.as_ref().clone()))
            }
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => None,
        }
    }

    /// 清理当前激活 tab 的搜索片段高亮。
    ///
    /// 业务意图：
    /// - 搜索框关键字、大小写、正则模式或当前文件变化后，旧片段高亮不再代表当前搜索条件。
    /// - 该清理只影响搜索关键字片段，不清行标记跳转的整行提示，避免破坏用户手动标记工作流。
    pub(in crate::app) fn clear_active_log_tab_search_match_highlight(&mut self) {
        Self::clear_log_tab_search_match_highlight_for_active(
            &mut self.log.open_tabs,
            self.log.active_tab_id,
        );
    }

    /// 按活动 tab ID 清理搜索片段高亮。
    ///
    /// 实现原因：
    /// - 将纯状态变更拆成静态 helper，测试无需构造完整 GPUI `MainView` 也能覆盖“只清当前 tab”的边界。
    pub(in crate::app) fn clear_log_tab_search_match_highlight_for_active(
        open_tabs: &mut [OpenLogTab],
        active_tab_id: Option<usize>,
    ) {
        let Some(active_tab_id) = active_tab_id else {
            return;
        };
        if let Some(tab) = open_tabs.iter_mut().find(|tab| tab.id == active_tab_id) {
            tab.highlighted_search_match = None;
        }
    }

    /// 将 UI 层高亮状态转换为搜索核心使用的轻量导航位置。
    ///
    /// 业务意图：
    /// - UI 渲染状态和搜索核心类型保持边界分离；跳转前只提取行号与 UTF-8 字节范围。
    fn search_match_position_from_highlight(
        highlight: &LogSearchMatchHighlight,
    ) -> SearchMatchPosition {
        SearchMatchPosition {
            line_index: highlight.line_index,
            match_range: highlight.match_range.clone(),
        }
    }

    /// 清空当前文件计数缓存。
    ///
    /// 业务意图：
    /// - 查询词、大小写选项、活动 tab 或解码内容变化后，旧计数和“下一个”起点不再代表当前条件，必须清空。
    pub(in crate::app) fn clear_search_current_file_match_count(&mut self) {
        self.search.current_file_navigation_request_id += 1;
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.current_file_match_count = None;
            dialog.current_file_navigation_match = None;
        }
        self.clear_active_log_tab_search_match_highlight();
    }

    /// 设置搜索对话框匹配模式并清理依赖旧条件的临时状态。
    ///
    /// 业务意图：
    /// - “正则”开关会改变同一查询词的解释方式，当前文件计数缓存和历史下拉都必须失效。
    /// - 正则模式不修改 `case_sensitive` 原值，只让 UI 禁用该开关，方便切回普通文本后恢复用户之前的大小写选择。
    pub(in crate::app) fn set_search_dialog_match_mode(
        dialog: &mut SearchDialogState,
        match_mode: SearchMatchMode,
    ) {
        if dialog.match_mode == match_mode {
            dialog.query_history_menu_open = false;
            return;
        }

        dialog.match_mode = match_mode;
        dialog.query_history_menu_open = false;
        dialog.current_file_match_count = None;
        dialog.current_file_navigation_match = None;
        dialog.message = match match_mode {
            SearchMatchMode::Literal => "已切换为普通文本搜索".to_string(),
            SearchMatchMode::Regex => "已切换为正则搜索，大小写由表达式控制".to_string(),
        };
    }

    /// 判断“下一个”按钮是否具备当前文件内跳转条件。
    ///
    /// 业务意图：
    /// - “下一个”是轻量当前文件导航，不启动后台搜索任务；只要查询词非空且当前文件已打开完成即可尝试定位。
    pub(in crate::app) fn search_can_jump_next_current_file(
        &self,
        dialog: &SearchDialogState,
    ) -> bool {
        !dialog.query_input.text.trim().is_empty() && self.active_log_tab_document().is_some()
    }

    /// 从当前搜索框条件生成当前文件导航选项。
    ///
    /// 实现原因：
    /// - 输入即定位、下一个和计数都必须共享大小写、正则模式和空查询校验，避免同一个搜索框出现三套匹配语义。
    fn current_file_navigation_options(
        &self,
    ) -> Option<(SearchOptions, Option<LogSearchMatchHighlight>, bool)> {
        let dialog = self.search.search_dialog.as_ref()?;
        let options = SearchOptions::single_with_mode(
            dialog.query_input.text.trim().to_string(),
            dialog.case_sensitive,
            dialog.match_mode,
        );
        let previous_match = dialog.current_file_navigation_match.clone();
        Some((options, previous_match, dialog.is_searching))
    }

    /// 根据当前搜索框条件定位当前文件中的命中行。
    ///
    /// 业务意图：
    /// - 输入关键字后从文件顶部定位第一条命中；点击“下一个”从上一次定位行之后继续查找，并在末尾自动绕回顶部。
    /// - 该路径只滚动当前已打开文件，不创建底部搜索结果记录，也不扫描当前目录。
    ///
    /// 边界条件：
    /// - 正则表达式无效时展示中文错误并清空轻量导航起点。
    /// - 当前文件尚未打开完成时展示提示，不触发文件读取。
    pub(in crate::app) fn jump_search_query_in_current_file(
        &mut self,
        start_from_top: bool,
        context: &mut Context<Self>,
    ) {
        let navigation = if start_from_top {
            CurrentFileSearchNavigation::FirstFromTop
        } else {
            CurrentFileSearchNavigation::Next
        };
        self.jump_search_query_in_current_file_with_direction(navigation, context);
    }

    /// 定位当前文件中的上一个搜索命中。
    ///
    /// 业务意图：
    /// - 搜索窗口上箭头按钮需要从当前命中向文件头方向查找，并在到达顶部后从文件末尾绕回。
    pub(in crate::app) fn jump_previous_search_query_in_current_file(
        &mut self,
        context: &mut Context<Self>,
    ) {
        self.jump_search_query_in_current_file_with_direction(
            CurrentFileSearchNavigation::Previous,
            context,
        );
    }

    /// 根据指定方向定位当前文件中的搜索命中。
    ///
    /// 业务意图：
    /// - 输入即定位、上一个和下一个共享同一套校验、分页后台执行和过期请求丢弃逻辑。
    fn jump_search_query_in_current_file_with_direction(
        &mut self,
        navigation: CurrentFileSearchNavigation,
        context: &mut Context<Self>,
    ) {
        let Some((options, previous_match, is_searching)) = self.current_file_navigation_options()
        else {
            return;
        };
        if is_searching {
            return;
        }
        if options.is_empty_query() {
            self.clear_search_current_file_match_count();
            context.notify();
            return;
        }
        if let Err(error) = options.validate() {
            self.clear_search_current_file_match_count();
            self.update_search_dialog_message(error.to_string(), context);
            return;
        }
        let Some((source, document)) = self.active_log_tab_source_and_document() else {
            self.update_search_dialog_message("当前文件未打开完成，无法定位", context);
            return;
        };

        let previous_position = previous_match
            .as_ref()
            .map(Self::search_match_position_from_highlight);
        let wrap = match navigation {
            CurrentFileSearchNavigation::FirstFromTop => false,
            CurrentFileSearchNavigation::Next | CurrentFileSearchNavigation::Previous => {
                previous_position.is_some()
            }
        };
        self.search.current_file_navigation_request_id += 1;
        let request_id = self.search.current_file_navigation_request_id;
        match document {
            LogTabDocument::InMemory(document) => {
                let result = match navigation {
                    CurrentFileSearchNavigation::FirstFromTop => find_search_result_after_position(
                        &source,
                        document.lines.as_ref(),
                        &options,
                        None,
                        false,
                    ),
                    CurrentFileSearchNavigation::Next => find_search_result_after_position(
                        &source,
                        document.lines.as_ref(),
                        &options,
                        previous_position.as_ref(),
                        wrap,
                    ),
                    CurrentFileSearchNavigation::Previous => find_search_result_before_position(
                        &source,
                        document.lines.as_ref(),
                        &options,
                        previous_position.as_ref(),
                        wrap,
                    ),
                };
                self.apply_current_file_navigation_result(request_id, result, context);
            }
            LogTabDocument::Paged(document) => {
                if let Some(dialog) = self.search.search_dialog.as_mut() {
                    dialog.message = "正在定位当前文件...".to_string();
                }
                context
                    .spawn(async move |view, app| {
                        let result = app
                            .background_executor()
                            .spawn(async move {
                                match navigation {
                                    CurrentFileSearchNavigation::FirstFromTop => {
                                        find_paged_search_result_after_position(
                                            &document, &options, None, false,
                                        )
                                    }
                                    CurrentFileSearchNavigation::Next => {
                                        find_paged_search_result_after_position(
                                            &document,
                                            &options,
                                            previous_position.as_ref(),
                                            wrap,
                                        )
                                    }
                                    CurrentFileSearchNavigation::Previous => {
                                        find_paged_search_result_before_position(
                                            &document,
                                            &options,
                                            previous_position.as_ref(),
                                            wrap,
                                        )
                                    }
                                }
                            })
                            .await;
                        view.update(app, move |view, context| {
                            view.apply_current_file_navigation_result(request_id, result, context);
                        })
                        .ok();
                    })
                    .detach();
                context.notify();
            }
        }
    }

    /// 应用当前文件轻量定位结果。
    ///
    /// 业务意图：
    /// - 内存日志会同步返回定位结果，分页日志会从后台线程回调；两条路径必须统一检查请求 ID、
    ///   更新搜索窗口提示，并复用搜索结果跳转逻辑滚动到命中行。
    ///
    /// 边界条件：
    /// - 如果用户继续输入、切换 tab、关闭搜索窗口或重新加载日志，请求 ID 会变化，旧结果必须直接丢弃。
    fn apply_current_file_navigation_result(
        &mut self,
        request_id: usize,
        result: Option<SearchResultItem>,
        context: &mut Context<Self>,
    ) {
        if self.search.current_file_navigation_request_id != request_id {
            return;
        }
        if let Some(result) = result {
            let line_index = result.line_index;
            let navigation_match = LogSearchMatchHighlight {
                line_index,
                match_range: result.match_range.clone(),
            };
            self.open_search_result(result, context);
            if let Some(dialog) = self.search.search_dialog.as_mut() {
                dialog.current_file_navigation_match = Some(navigation_match);
                dialog.message = format!("已定位到第 {} 行", line_index + 1);
            }
        } else if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.current_file_navigation_match = None;
            dialog.message = "当前文件未找到匹配项".to_string();
            self.clear_active_log_tab_search_match_highlight();
        }
        context.notify();
    }

    /// 统计搜索关键字在当前文件中的出现次数。
    ///
    /// 业务意图：
    /// - 该动作由搜索窗口“计数”按钮触发，只针对当前激活文件的已解码内容，给用户一个轻量的命中规模反馈。
    /// - 计数结果以片段出现次数为单位，同一行多次出现会累加；完整搜索按钮仍负责生成结果面板和跳转明细。
    pub(in crate::app) fn count_search_query_in_current_file(
        &mut self,
        context: &mut Context<Self>,
    ) {
        let Some(dialog) = self.search.search_dialog.as_ref() else {
            return;
        };
        let options = SearchOptions::single_with_mode(
            dialog.query_input.text.trim().to_string(),
            dialog.case_sensitive,
            dialog.match_mode,
        );
        if options.is_empty_query() {
            self.update_search_dialog_message("请输入要计数的关键字", context);
            return;
        }
        if let Err(error) = options.validate() {
            self.clear_search_current_file_match_count();
            self.update_search_dialog_message(error.to_string(), context);
            return;
        }
        let Some(document) = self.active_log_tab_document() else {
            self.update_search_dialog_message("当前文件未打开完成，无法计数", context);
            return;
        };
        let count = match document {
            LogTabDocument::InMemory(document) => {
                count_query_occurrences(&document.lines, &options)
            }
            LogTabDocument::Paged(document) => count_query_occurrences_paged(&document, &options),
        };
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.current_file_match_count = Some(count);
            dialog.message = format!("当前文件命中 {count} 次");
        }
        context.notify();
    }
}
