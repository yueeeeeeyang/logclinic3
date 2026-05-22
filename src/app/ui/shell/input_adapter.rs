// 主窗口输入、剪贴板和文本选择适配。
//
// 业务意图：
// - 集中维护 GPUI EntityInputHandler、IME marked range、UTF-8/UTF-16 转换、搜索输入和设置页输入框适配。
// - 该模块只处理 UI 文本编辑状态，不改变搜索语义、线程过滤规则或模型配置持久化格式。

use super::*;

impl EntityInputHandler for MainView {
    /// 返回指定 UTF-16 范围内的搜索框文本。
    ///
    /// 业务意图：
    /// - 平台输入法需要查询当前文本片段以管理候选词、组合文本和替换范围。
    /// - 搜索框内部保存 UTF-8 字符串，因此这里必须进行 UTF-16 到 UTF-8 的安全转换。
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<String> {
        if let Some(tab) = self.focused_terminal_tab(window) {
            let ime = &tab.ime;
            let range = Self::search_input_range_from_utf16(&ime.text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(&ime.text, range.clone()));
            return Some(ime.text[range].to_string());
        }
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let state = self.model_config_input_state(kind);
            let range = Self::search_input_range_from_utf16(&state.text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &state.text,
                range.clone(),
            ));
            return Some(state.text[range].to_string());
        }
        if self.notes.title_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(&self.notes.editor_title, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &self.notes.editor_title,
                range.clone(),
            ));
            return Some(self.notes.editor_title[range].to_string());
        }
        if self.notes.rich_editor.focus.is_focused(window) {
            let text = self.notes.rich_editor.plain_text();
            let range = Self::search_input_range_from_utf16(&text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(&text, range.clone()));
            return Some(text[range].to_string());
        }
        if self.notes.ai.input_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(&self.notes.ai.input_text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &self.notes.ai.input_text,
                range.clone(),
            ));
            return Some(self.notes.ai.input_text[range].to_string());
        }
        if self.ai_chat.input_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(&self.ai_chat.input_text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &self.ai_chat.input_text,
                range.clone(),
            ));
            return Some(self.ai_chat.input_text[range].to_string());
        }
        if self.log.log_tree_search.focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(
                &self.log.log_tree_search.input.text,
                range_utf16,
            );
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &self.log.log_tree_search.input.text,
                range.clone(),
            ));
            return Some(self.log.log_tree_search.input.text[range].to_string());
        }
        if self.notes.tree_search.focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(
                &self.notes.tree_search.input.text,
                range_utf16,
            );
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &self.notes.tree_search.input.text,
                range.clone(),
            ));
            return Some(self.notes.tree_search.input.text[range].to_string());
        }
        if self.connections.tree_search.focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(
                &self.connections.tree_search.input.text,
                range_utf16,
            );
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &self.connections.tree_search.input.text,
                range.clone(),
            ));
            return Some(self.connections.tree_search.input.text[range].to_string());
        }
        if let Some(field) = self.active_connection_form_field(window) {
            let state = self.connections.dialog.as_ref()?.field(field);
            let range = Self::search_input_range_from_utf16(&state.input.text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &state.input.text,
                range.clone(),
            ));
            return Some(state.input.text[range].to_string());
        }
        if let Some(field) = self.active_smb_connection_form_field(window) {
            let state = self.connections.smb_dialog.as_ref()?.field(field);
            let range = Self::search_input_range_from_utf16(&state.input.text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &state.input.text,
                range.clone(),
            ));
            return Some(state.input.text[range].to_string());
        }
        if self.connection_category_name_focused(window) {
            let state = &self.connections.category_dialog.as_ref()?.name;
            let range = Self::search_input_range_from_utf16(&state.input.text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &state.input.text,
                range.clone(),
            ));
            return Some(state.input.text[range].to_string());
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(
                &self.settings.quick_search_keywords_input.text,
                range_utf16,
            );
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &self.settings.quick_search_keywords_input.text,
                range.clone(),
            ));
            return Some(self.settings.quick_search_keywords_input.text[range].to_string());
        }
        if let Some(index) = self.active_plugin_pattern_setting_index(window) {
            let state = self.settings.plugin_pattern_inputs.get(index)?;
            let range = Self::search_input_range_from_utf16(&state.input.text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &state.input.text,
                range.clone(),
            ));
            return Some(state.input.text[range].to_string());
        }
        if let Some(kind) = self.active_thread_analysis_filter_input_kind(window) {
            let text = self.thread_analysis_filter_input_text(kind);
            let range = Self::search_input_range_from_utf16(text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(text, range.clone()));
            return Some(text[range].to_string());
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, _, _, _) = Self::search_text_state(dialog, input_kind);
        let range = Self::search_input_range_from_utf16(text, range_utf16);
        adjusted_range.replace(Self::search_input_range_to_utf16(text, range.clone()));
        Some(text[range].to_string())
    }

    /// 返回搜索框当前选择范围。
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if let Some(tab) = self.focused_terminal_tab(window) {
            let ime = &tab.ime;
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(&ime.text, ime.selection_range.clone()),
                reversed: false,
            });
        }
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let state = self.model_config_input_state(kind);
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &state.text,
                    state.selection_range.clone(),
                ),
                reversed: false,
            });
        }
        if self.notes.title_focus.is_focused(window) {
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &self.notes.editor_title,
                    self.notes.title_selection_range.clone(),
                ),
                reversed: false,
            });
        }
        if self.notes.rich_editor.focus.is_focused(window) {
            let text = self.notes.rich_editor.plain_text();
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &text,
                    self.notes.rich_editor.clamped_selection(),
                ),
                reversed: false,
            });
        }
        if self.notes.ai.input_focus.is_focused(window) {
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &self.notes.ai.input_text,
                    self.notes.ai.input_selection_range.clone(),
                ),
                reversed: false,
            });
        }
        if self.ai_chat.input_focus.is_focused(window) {
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &self.ai_chat.input_text,
                    self.ai_chat.input_selection_range.clone(),
                ),
                reversed: false,
            });
        }
        if self.log.log_tree_search.focus.is_focused(window) {
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &self.log.log_tree_search.input.text,
                    self.log.log_tree_search.input.selection_range.clone(),
                ),
                reversed: false,
            });
        }
        if self.notes.tree_search.focus.is_focused(window) {
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &self.notes.tree_search.input.text,
                    self.notes.tree_search.input.selection_range.clone(),
                ),
                reversed: false,
            });
        }
        if self.connections.tree_search.focus.is_focused(window) {
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &self.connections.tree_search.input.text,
                    self.connections.tree_search.input.selection_range.clone(),
                ),
                reversed: false,
            });
        }
        if let Some(field) = self.active_connection_form_field(window) {
            let state = self.connections.dialog.as_ref()?.field(field);
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &state.input.text,
                    state.input.selection_range.clone(),
                ),
                reversed: false,
            });
        }
        if let Some(field) = self.active_smb_connection_form_field(window) {
            let state = self.connections.smb_dialog.as_ref()?.field(field);
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &state.input.text,
                    state.input.selection_range.clone(),
                ),
                reversed: false,
            });
        }
        if self.connection_category_name_focused(window) {
            let state = &self.connections.category_dialog.as_ref()?.name;
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &state.input.text,
                    state.input.selection_range.clone(),
                ),
                reversed: false,
            });
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &self.settings.quick_search_keywords_input.text,
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .clone(),
                ),
                reversed: false,
            });
        }
        if let Some(index) = self.active_plugin_pattern_setting_index(window) {
            let state = self.settings.plugin_pattern_inputs.get(index)?;
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &state.input.text,
                    state.input.selection_range.clone(),
                ),
                reversed: false,
            });
        }
        if let Some(kind) = self.active_thread_analysis_filter_input_kind(window) {
            let text = self.thread_analysis_filter_input_text(kind);
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    text,
                    self.thread_analysis_filter_input_selection_range(kind),
                ),
                reversed: false,
            });
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, selection_range, _, _) = Self::search_text_state(dialog, input_kind);
        Some(UTF16Selection {
            range: Self::search_input_range_to_utf16(text, selection_range),
            reversed: false,
        })
    }

    /// 返回输入法当前组合文本范围。
    fn marked_text_range(
        &self,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        if let Some(tab) = self.focused_terminal_tab(window) {
            let ime = &tab.ime;
            return ime
                .marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&ime.text, range));
        }
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let state = self.model_config_input_state(kind);
            return state
                .marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&state.text, range));
        }
        if self.notes.title_focus.is_focused(window) {
            return self
                .notes
                .title_marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&self.notes.editor_title, range));
        }
        if self.notes.rich_editor.focus.is_focused(window) {
            let text = self.notes.rich_editor.plain_text();
            return self
                .notes
                .rich_editor
                .marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&text, range));
        }
        if self.notes.ai.input_focus.is_focused(window) {
            return self
                .notes
                .ai
                .input_marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&self.notes.ai.input_text, range));
        }
        if self.ai_chat.input_focus.is_focused(window) {
            return self
                .ai_chat
                .input_marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&self.ai_chat.input_text, range));
        }
        if self.log.log_tree_search.focus.is_focused(window) {
            return self
                .log
                .log_tree_search
                .input
                .marked_range
                .clone()
                .map(|range| {
                    Self::search_input_range_to_utf16(&self.log.log_tree_search.input.text, range)
                });
        }
        if self.notes.tree_search.focus.is_focused(window) {
            return self
                .notes
                .tree_search
                .input
                .marked_range
                .clone()
                .map(|range| {
                    Self::search_input_range_to_utf16(&self.notes.tree_search.input.text, range)
                });
        }
        if self.connections.tree_search.focus.is_focused(window) {
            return self
                .connections
                .tree_search
                .input
                .marked_range
                .clone()
                .map(|range| {
                    Self::search_input_range_to_utf16(
                        &self.connections.tree_search.input.text,
                        range,
                    )
                });
        }
        if let Some(field) = self.active_connection_form_field(window) {
            let state = self.connections.dialog.as_ref()?.field(field);
            return state
                .input
                .marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&state.input.text, range));
        }
        if let Some(field) = self.active_smb_connection_form_field(window) {
            let state = self.connections.smb_dialog.as_ref()?.field(field);
            return state
                .input
                .marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&state.input.text, range));
        }
        if self.connection_category_name_focused(window) {
            let state = &self.connections.category_dialog.as_ref()?.name;
            return state
                .input
                .marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&state.input.text, range));
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            return self
                .settings
                .quick_search_keywords_input
                .marked_range
                .clone()
                .map(|range| {
                    Self::search_input_range_to_utf16(
                        &self.settings.quick_search_keywords_input.text,
                        range,
                    )
                });
        }
        if let Some(index) = self.active_plugin_pattern_setting_index(window) {
            let state = self.settings.plugin_pattern_inputs.get(index)?;
            return state
                .input
                .marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&state.input.text, range));
        }
        if let Some(kind) = self.active_thread_analysis_filter_input_kind(window) {
            let text = self.thread_analysis_filter_input_text(kind);
            return self
                .thread_analysis_filter_input_marked_range(kind)
                .map(|range| Self::search_input_range_to_utf16(text, range));
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, _, marked_range, _) = Self::search_text_state(dialog, input_kind);
        marked_range.map(|range| Self::search_input_range_to_utf16(text, range))
    }

    /// 清除输入法组合文本状态。
    fn unmark_text(&mut self, window: &mut Window, context: &mut Context<Self>) {
        if self.terminal_input_focused_without_modal(window) {
            if let Some(tab) = self.focused_terminal_tab_mut(window) {
                tab.clear_ime();
            }
            context.notify();
            return;
        }
        if let Some(kind) = self.active_model_config_input_kind(window) {
            self.model_config_input_state_mut(kind).marked_range = None;
            context.notify();
            return;
        }
        if self.notes.title_focus.is_focused(window) {
            self.notes.title_marked_range = None;
            context.notify();
            return;
        }
        if self.notes.rich_editor.focus.is_focused(window) {
            self.notes.rich_editor.marked_range = None;
            context.notify();
            return;
        }
        if self.notes.ai.input_focus.is_focused(window) {
            self.notes.ai.input_marked_range = None;
            context.notify();
            return;
        }
        if self.ai_chat.input_focus.is_focused(window) {
            self.ai_chat.input_marked_range = None;
            context.notify();
            return;
        }
        if self.log.log_tree_search.focus.is_focused(window) {
            self.log.log_tree_search.input.marked_range = None;
            context.notify();
            return;
        }
        if self.notes.tree_search.focus.is_focused(window) {
            self.notes.tree_search.input.marked_range = None;
            self.notes.tree_search.input.selection_drag = None;
            context.notify();
            return;
        }
        if self.connections.tree_search.focus.is_focused(window) {
            self.connections.tree_search.input.marked_range = None;
            self.connections.tree_search.input.selection_drag = None;
            context.notify();
            return;
        }
        if let Some(field) = self.active_connection_form_field(window) {
            if let Some(dialog) = self.connections.dialog.as_mut() {
                let input = &mut dialog.field_mut(field).input;
                input.marked_range = None;
                input.selection_drag = None;
            }
            context.notify();
            return;
        }
        if let Some(field) = self.active_smb_connection_form_field(window) {
            if let Some(dialog) = self.connections.smb_dialog.as_mut() {
                let input = &mut dialog.field_mut(field).input;
                input.marked_range = None;
                input.selection_drag = None;
            }
            context.notify();
            return;
        }
        if self.connection_category_name_focused(window) {
            if let Some(dialog) = self.connections.category_dialog.as_mut() {
                dialog.name.input.marked_range = None;
                dialog.name.input.selection_drag = None;
            }
            context.notify();
            return;
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            self.settings.quick_search_keywords_input.marked_range = None;
            context.notify();
            return;
        }
        if let Some(index) = self.active_plugin_pattern_setting_index(window) {
            if let Some(state) = self.settings.plugin_pattern_inputs.get_mut(index) {
                state.input.marked_range = None;
                state.input.selection_drag = None;
            }
            context.notify();
            return;
        }
        if let Some(kind) = self.active_thread_analysis_filter_input_kind(window) {
            self.set_thread_analysis_filter_input_marked_range(kind, None);
            context.notify();
            return;
        }
        let input_kind = self.active_search_text_input_kind(window);
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            let (_, _, marked_range) = Self::search_text_state_mut(dialog, input_kind);
            *marked_range = None;
        }
        context.notify();
    }

    /// 用平台提交文本替换搜索框中的指定范围。
    ///
    /// 业务意图：
    /// - 中文 IME 候选词确认后会通过该入口提交最终文本，不能再依赖按键字符。
    /// - 替换范围优先采用平台指定范围，其次采用组合文本范围，最后使用普通选择范围。
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.terminal_input_focused_without_modal(window) {
            if let Some(tab) = self.focused_terminal_tab_mut(window) {
                tab.clear_ime();
                if !text.is_empty() {
                    // 终端输入必须按 UTF-8 原样发送给 PTY/SSH；中文 IME 最终提交会走到这里。
                    tab.write_input(text.as_bytes().to_vec());
                }
            }
            context.notify();
            return;
        }
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let replacement = Self::sanitize_search_input_text(text);
            let state = self.model_config_input_state_mut(kind);
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&state.text, range))
                .or_else(|| state.marked_range.clone())
                .unwrap_or_else(|| state.selection_range.clone());
            let range = Self::clamp_search_text_range(&state.text, range);
            state.text.replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            state.selection_range = cursor..cursor;
            state.marked_range = None;
            state.clear_layout();
            self.model_config.model_test_status = ModelTestStatus::Idle;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.notes.title_focus.is_focused(window) {
            if !self.notes.is_editing && self.notes.rename_dialog.is_none() {
                return;
            }
            let replacement = Self::sanitize_search_input_text(text);
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&self.notes.editor_title, range))
                .or_else(|| self.notes.title_marked_range.clone())
                .unwrap_or_else(|| self.notes.title_selection_range.clone());
            let range = Self::clamp_search_text_range(&self.notes.editor_title, range);
            self.notes
                .editor_title
                .replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            self.notes.title_selection_range = cursor..cursor;
            self.notes.title_marked_range = None;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.notes.rich_editor.focus.is_focused(window) {
            if !self.notes.is_editing {
                return;
            }
            let plain_text = self.notes.rich_editor.plain_text();
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&plain_text, range))
                .or_else(|| self.notes.rich_editor.marked_range.clone())
                .unwrap_or_else(|| self.notes.rich_editor.clamped_selection());
            self.notes
                .rich_editor
                .replace_range_with_text(Some(range), text);
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.notes.ai.input_focus.is_focused(window) {
            let replacement = text.replace("\r\n", "\n").replace('\r', "\n");
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&self.notes.ai.input_text, range))
                .or_else(|| self.notes.ai.input_marked_range.clone())
                .unwrap_or_else(|| self.notes.ai.input_selection_range.clone());
            let range = Self::clamp_search_text_range(&self.notes.ai.input_text, range);
            self.notes
                .ai
                .input_text
                .replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            self.notes.ai.input_selection_range = cursor..cursor;
            self.notes.ai.input_marked_range = None;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.ai_chat.input_focus.is_focused(window) {
            let replacement = text.replace("\r\n", "\n").replace('\r', "\n");
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&self.ai_chat.input_text, range))
                .or_else(|| self.ai_chat.input_marked_range.clone())
                .unwrap_or_else(|| self.ai_chat.input_selection_range.clone());
            let range = Self::clamp_search_text_range(&self.ai_chat.input_text, range);
            self.ai_chat
                .input_text
                .replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            self.ai_chat.input_selection_range = cursor..cursor;
            self.ai_chat.input_marked_range = None;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.log.log_tree_search.focus.is_focused(window) {
            let replacement = Self::sanitize_search_input_text(text);
            let input = &mut self.log.log_tree_search.input;
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&input.text, range))
                .or_else(|| input.marked_range.clone())
                .unwrap_or_else(|| input.selection_range.clone());
            let range = Self::clamp_search_text_range(&input.text, range);
            input.text.replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            input.selection_range = cursor..cursor;
            input.marked_range = None;
            input.horizontal_scroll_px = 0.0;
            self.log.log_tree_search.clear_layout();
            self.touch_search_text_cursor_activity();
            self.refresh_log_tree_search_results(true, context);
            return;
        }
        if self.notes.tree_search.focus.is_focused(window) {
            let replacement = Self::sanitize_search_input_text(text);
            let input = &mut self.notes.tree_search.input;
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&input.text, range))
                .or_else(|| input.marked_range.clone())
                .unwrap_or_else(|| input.selection_range.clone());
            let range = Self::clamp_search_text_range(&input.text, range);
            input.text.replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            input.selection_range = cursor..cursor;
            input.marked_range = None;
            input.selection_drag = None;
            input.horizontal_scroll_px = 0.0;
            self.after_notes_tree_search_changed();
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.connections.tree_search.focus.is_focused(window) {
            let replacement = Self::sanitize_search_input_text(text);
            let input = &mut self.connections.tree_search.input;
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&input.text, range))
                .or_else(|| input.marked_range.clone())
                .unwrap_or_else(|| input.selection_range.clone());
            let range = Self::clamp_search_text_range(&input.text, range);
            input.text.replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            input.selection_range = cursor..cursor;
            input.marked_range = None;
            input.selection_drag = None;
            input.horizontal_scroll_px = 0.0;
            self.connections.tree_search.clear_layout();
            self.connections.create_menu_open = false;
            self.connections.profile_context_menu = None;
            self.connections.category_context_menu = None;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if let Some(field) = self.active_connection_form_field(window) {
            let replacement = Self::sanitize_search_input_text(text);
            if let Some(dialog) = self.connections.dialog.as_mut() {
                let field_state = dialog.field_mut(field);
                let range = range_utf16
                    .map(|range| {
                        Self::search_input_range_from_utf16(&field_state.input.text, range)
                    })
                    .or_else(|| field_state.input.marked_range.clone())
                    .unwrap_or_else(|| field_state.input.selection_range.clone());
                let range = Self::clamp_search_text_range(&field_state.input.text, range);
                field_state
                    .input
                    .text
                    .replace_range(range.clone(), &replacement);
                let cursor = range.start + replacement.len();
                field_state.input.selection_range = cursor..cursor;
                field_state.input.marked_range = None;
                field_state.input.selection_drag = None;
                field_state.clear_layout();
                dialog.error = None;
            }
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if let Some(field) = self.active_smb_connection_form_field(window) {
            let replacement = Self::sanitize_search_input_text(text);
            if let Some(dialog) = self.connections.smb_dialog.as_mut() {
                let field_state = dialog.field_mut(field);
                let range = range_utf16
                    .map(|range| {
                        Self::search_input_range_from_utf16(&field_state.input.text, range)
                    })
                    .or_else(|| field_state.input.marked_range.clone())
                    .unwrap_or_else(|| field_state.input.selection_range.clone());
                let range = Self::clamp_search_text_range(&field_state.input.text, range);
                field_state
                    .input
                    .text
                    .replace_range(range.clone(), &replacement);
                let cursor = range.start + replacement.len();
                field_state.input.selection_range = cursor..cursor;
                field_state.input.marked_range = None;
                field_state.input.selection_drag = None;
                field_state.clear_layout();
                dialog.error = None;
            }
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.connection_category_name_focused(window) {
            let replacement = Self::sanitize_search_input_text(text);
            if let Some(dialog) = self.connections.category_dialog.as_mut() {
                let input = &mut dialog.name.input;
                let range = range_utf16
                    .map(|range| Self::search_input_range_from_utf16(&input.text, range))
                    .or_else(|| input.marked_range.clone())
                    .unwrap_or_else(|| input.selection_range.clone());
                let range = Self::clamp_search_text_range(&input.text, range);
                input.text.replace_range(range.clone(), &replacement);
                let cursor = range.start + replacement.len();
                input.selection_range = cursor..cursor;
                input.marked_range = None;
                input.selection_drag = None;
                dialog.name.clear_layout();
                dialog.error = None;
            }
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            if !self.settings.quick_search_keywords_is_editing {
                return;
            }
            let replacement = Self::sanitize_search_input_text(text);
            let range = range_utf16
                .map(|range| {
                    Self::search_input_range_from_utf16(
                        &self.settings.quick_search_keywords_input.text,
                        range,
                    )
                })
                .or_else(|| {
                    self.settings
                        .quick_search_keywords_input
                        .marked_range
                        .clone()
                })
                .unwrap_or_else(|| {
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .clone()
                });
            let range = Self::clamp_search_text_range(
                &self.settings.quick_search_keywords_input.text,
                range,
            );
            self.settings
                .quick_search_keywords_input
                .text
                .replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            self.settings.quick_search_keywords_input.selection_range = cursor..cursor;
            self.settings.quick_search_keywords_input.marked_range = None;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if let Some(index) = self.active_plugin_pattern_setting_index(window) {
            let replacement = Self::sanitize_search_input_text(text);
            if let Some(state) = self.settings.plugin_pattern_inputs.get_mut(index) {
                let range = range_utf16
                    .map(|range| Self::search_input_range_from_utf16(&state.input.text, range))
                    .or_else(|| state.input.marked_range.clone())
                    .unwrap_or_else(|| state.input.selection_range.clone());
                let range = Self::clamp_search_text_range(&state.input.text, range);
                state.input.text.replace_range(range.clone(), &replacement);
                let cursor = range.start + replacement.len();
                state.input.selection_range = cursor..cursor;
                state.input.marked_range = None;
                state.input.selection_drag = None;
                state.last_layout = None;
                state.last_bounds = None;
            }
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if let Some(kind) = self.active_thread_analysis_filter_input_kind(window) {
            if !self.settings.thread_analysis_filter_is_editing {
                return;
            }
            let replacement = normalize_thread_analysis_filter_text(text);
            let text = self.thread_analysis_filter_input_text(kind);
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(text, range))
                .or_else(|| self.thread_analysis_filter_input_marked_range(kind))
                .unwrap_or_else(|| self.thread_analysis_filter_input_selection_range(kind));
            let range = Self::clamp_search_text_range(text, range);
            self.replace_thread_analysis_filter_byte_range(kind, range, &replacement);
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        let input_kind = self.active_search_text_input_kind(window);
        let Some(dialog) = self.search.search_dialog.as_mut() else {
            return;
        };
        let replacement = Self::sanitize_search_input_text(text);
        let (target_text, selection_range, marked_range) =
            Self::search_text_state_mut(dialog, input_kind);
        let range = range_utf16
            .map(|range| Self::search_input_range_from_utf16(target_text, range))
            .or_else(|| marked_range.clone())
            .unwrap_or_else(|| selection_range.clone());
        let range = Self::clamp_search_text_range(target_text, range);
        target_text.replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        *selection_range = cursor..cursor;
        *marked_range = None;
        if input_kind == SearchTextInputKind::Query {
            dialog.query_history_menu_open = false;
            self.clear_search_current_file_match_count();
        }
        self.touch_search_text_cursor_activity();
        if input_kind == SearchTextInputKind::Query {
            self.jump_search_query_in_current_file(true, context);
        }
        context.notify();
    }

    /// 用平台组合文本替换搜索框中的指定范围，并保留组合状态。
    ///
    /// 边界条件：
    /// - 输入法可能多次更新同一段组合文本，必须优先替换旧 `marked_range`，避免拼音或候选词重复追加。
    /// - 搜索框只支持单行文本，因此组合文本中的换行会被移除。
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.terminal_input_focused_without_modal(window) {
            if let Some(tab) = self.focused_terminal_tab_mut(window) {
                let ime = &mut tab.ime;
                let range = range_utf16
                    .map(|range| Self::search_input_range_from_utf16(&ime.text, range))
                    .or_else(|| ime.marked_range.clone())
                    .unwrap_or_else(|| ime.selection_range.clone());
                let range = Self::clamp_search_text_range(&ime.text, range);
                ime.text.replace_range(range.clone(), new_text);

                if new_text.is_empty() {
                    ime.marked_range = None;
                } else {
                    ime.marked_range = Some(range.start..range.start + new_text.len());
                }

                ime.selection_range = new_selected_range_utf16
                    .map(|utf16_range| Self::search_input_range_from_utf16(new_text, utf16_range))
                    .map(|relative_range| {
                        range.start + relative_range.start..range.start + relative_range.end
                    })
                    .unwrap_or_else(|| {
                        let cursor = range.start + new_text.len();
                        cursor..cursor
                    });
            }
            context.notify();
            return;
        }
        if let Some(field) = self.active_smb_connection_form_field(window) {
            let replacement = Self::sanitize_search_input_text(new_text);
            if let Some(dialog) = self.connections.smb_dialog.as_mut() {
                let field_state = dialog.field_mut(field);
                let range = range_utf16
                    .map(|range| {
                        Self::search_input_range_from_utf16(&field_state.input.text, range)
                    })
                    .or_else(|| field_state.input.marked_range.clone())
                    .unwrap_or_else(|| field_state.input.selection_range.clone());
                let range = Self::clamp_search_text_range(&field_state.input.text, range);
                field_state
                    .input
                    .text
                    .replace_range(range.clone(), &replacement);

                if replacement.is_empty() {
                    field_state.input.marked_range = None;
                } else {
                    field_state.input.marked_range =
                        Some(range.start..range.start + replacement.len());
                }

                let selected_range = new_selected_range_utf16
                    .map(|utf16_range| {
                        Self::search_input_range_from_utf16(&replacement, utf16_range)
                    })
                    .map(|relative_range| {
                        range.start + relative_range.start..range.start + relative_range.end
                    })
                    .unwrap_or_else(|| {
                        let cursor = range.start + replacement.len();
                        cursor..cursor
                    });
                field_state.input.selection_range = selected_range;
                field_state.input.selection_drag = None;
                field_state.clear_layout();
                dialog.error = None;
            }
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let replacement = Self::sanitize_search_input_text(new_text);
            let state = self.model_config_input_state_mut(kind);
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&state.text, range))
                .or_else(|| state.marked_range.clone())
                .unwrap_or_else(|| state.selection_range.clone());
            let range = Self::clamp_search_text_range(&state.text, range);
            state.text.replace_range(range.clone(), &replacement);

            if replacement.is_empty() {
                state.marked_range = None;
            } else {
                state.marked_range = Some(range.start..range.start + replacement.len());
            }

            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            state.selection_range = selected_range;
            state.clear_layout();
            self.model_config.model_test_status = ModelTestStatus::Idle;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.notes.title_focus.is_focused(window) {
            if !self.notes.is_editing && self.notes.rename_dialog.is_none() {
                return;
            }
            let replacement = Self::sanitize_search_input_text(new_text);
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&self.notes.editor_title, range))
                .or_else(|| self.notes.title_marked_range.clone())
                .unwrap_or_else(|| self.notes.title_selection_range.clone());
            let range = Self::clamp_search_text_range(&self.notes.editor_title, range);
            self.notes
                .editor_title
                .replace_range(range.clone(), &replacement);
            if replacement.is_empty() {
                self.notes.title_marked_range = None;
            } else {
                self.notes.title_marked_range = Some(range.start..range.start + replacement.len());
            }
            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            self.notes.title_selection_range = selected_range;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.notes.rich_editor.focus.is_focused(window) {
            if !self.notes.is_editing {
                return;
            }
            let replacement = new_text.replace("\r\n", "\n").replace('\r', "\n");
            let plain_text = self.notes.rich_editor.plain_text();
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&plain_text, range))
                .or_else(|| self.notes.rich_editor.marked_range.clone())
                .unwrap_or_else(|| self.notes.rich_editor.clamped_selection());
            let range = clamp_note_rich_text_range(&plain_text, range);
            self.notes
                .rich_editor
                .replace_range_with_text(Some(range.clone()), &replacement);
            if replacement.is_empty() {
                self.notes.rich_editor.marked_range = None;
            } else {
                self.notes.rich_editor.marked_range =
                    Some(range.start..range.start + replacement.len());
            }
            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            self.notes.rich_editor.selection_range = selected_range;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.notes.ai.input_focus.is_focused(window) {
            let replacement = new_text.replace("\r\n", "\n").replace('\r', "\n");
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&self.notes.ai.input_text, range))
                .or_else(|| self.notes.ai.input_marked_range.clone())
                .unwrap_or_else(|| self.notes.ai.input_selection_range.clone());
            let range = Self::clamp_search_text_range(&self.notes.ai.input_text, range);
            self.notes
                .ai
                .input_text
                .replace_range(range.clone(), &replacement);

            if replacement.is_empty() {
                self.notes.ai.input_marked_range = None;
            } else {
                self.notes.ai.input_marked_range =
                    Some(range.start..range.start + replacement.len());
            }

            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            self.notes.ai.input_selection_range = selected_range;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.ai_chat.input_focus.is_focused(window) {
            let replacement = new_text.replace("\r\n", "\n").replace('\r', "\n");
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&self.ai_chat.input_text, range))
                .or_else(|| self.ai_chat.input_marked_range.clone())
                .unwrap_or_else(|| self.ai_chat.input_selection_range.clone());
            let range = Self::clamp_search_text_range(&self.ai_chat.input_text, range);
            self.ai_chat
                .input_text
                .replace_range(range.clone(), &replacement);

            if replacement.is_empty() {
                self.ai_chat.input_marked_range = None;
            } else {
                self.ai_chat.input_marked_range =
                    Some(range.start..range.start + replacement.len());
            }

            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            self.ai_chat.input_selection_range = selected_range;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.log.log_tree_search.focus.is_focused(window) {
            let replacement = Self::sanitize_search_input_text(new_text);
            let input = &mut self.log.log_tree_search.input;
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&input.text, range))
                .or_else(|| input.marked_range.clone())
                .unwrap_or_else(|| input.selection_range.clone());
            let range = Self::clamp_search_text_range(&input.text, range);
            input.text.replace_range(range.clone(), &replacement);

            if replacement.is_empty() {
                input.marked_range = None;
            } else {
                input.marked_range = Some(range.start..range.start + replacement.len());
            }

            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            input.selection_range = selected_range;
            input.horizontal_scroll_px = 0.0;
            self.log.log_tree_search.clear_layout();
            self.touch_search_text_cursor_activity();
            self.refresh_log_tree_search_results(true, context);
            return;
        }
        if self.notes.tree_search.focus.is_focused(window) {
            let replacement = Self::sanitize_search_input_text(new_text);
            let input = &mut self.notes.tree_search.input;
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&input.text, range))
                .or_else(|| input.marked_range.clone())
                .unwrap_or_else(|| input.selection_range.clone());
            let range = Self::clamp_search_text_range(&input.text, range);
            input.text.replace_range(range.clone(), &replacement);

            if replacement.is_empty() {
                input.marked_range = None;
            } else {
                input.marked_range = Some(range.start..range.start + replacement.len());
            }

            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            input.selection_range = selected_range;
            input.selection_drag = None;
            input.horizontal_scroll_px = 0.0;
            self.after_notes_tree_search_changed();
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.connections.tree_search.focus.is_focused(window) {
            let replacement = Self::sanitize_search_input_text(new_text);
            let input = &mut self.connections.tree_search.input;
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&input.text, range))
                .or_else(|| input.marked_range.clone())
                .unwrap_or_else(|| input.selection_range.clone());
            let range = Self::clamp_search_text_range(&input.text, range);
            input.text.replace_range(range.clone(), &replacement);

            if replacement.is_empty() {
                input.marked_range = None;
            } else {
                input.marked_range = Some(range.start..range.start + replacement.len());
            }

            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            input.selection_range = selected_range;
            input.selection_drag = None;
            input.horizontal_scroll_px = 0.0;
            self.connections.tree_search.clear_layout();
            self.connections.create_menu_open = false;
            self.connections.profile_context_menu = None;
            self.connections.category_context_menu = None;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if let Some(field) = self.active_connection_form_field(window) {
            let replacement = Self::sanitize_search_input_text(new_text);
            if let Some(dialog) = self.connections.dialog.as_mut() {
                let field_state = dialog.field_mut(field);
                let range = range_utf16
                    .map(|range| {
                        Self::search_input_range_from_utf16(&field_state.input.text, range)
                    })
                    .or_else(|| field_state.input.marked_range.clone())
                    .unwrap_or_else(|| field_state.input.selection_range.clone());
                let range = Self::clamp_search_text_range(&field_state.input.text, range);
                field_state
                    .input
                    .text
                    .replace_range(range.clone(), &replacement);

                if replacement.is_empty() {
                    field_state.input.marked_range = None;
                } else {
                    field_state.input.marked_range =
                        Some(range.start..range.start + replacement.len());
                }

                let selected_range = new_selected_range_utf16
                    .map(|utf16_range| {
                        Self::search_input_range_from_utf16(&replacement, utf16_range)
                    })
                    .map(|relative_range| {
                        range.start + relative_range.start..range.start + relative_range.end
                    })
                    .unwrap_or_else(|| {
                        let cursor = range.start + replacement.len();
                        cursor..cursor
                    });
                field_state.input.selection_range = selected_range;
                field_state.input.selection_drag = None;
                field_state.clear_layout();
                dialog.error = None;
            }
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if let Some(field) = self.active_smb_connection_form_field(window) {
            let replacement = Self::sanitize_search_input_text(new_text);
            if let Some(dialog) = self.connections.smb_dialog.as_mut() {
                let field_state = dialog.field_mut(field);
                let range = range_utf16
                    .map(|range| {
                        Self::search_input_range_from_utf16(&field_state.input.text, range)
                    })
                    .or_else(|| field_state.input.marked_range.clone())
                    .unwrap_or_else(|| field_state.input.selection_range.clone());
                let range = Self::clamp_search_text_range(&field_state.input.text, range);
                field_state
                    .input
                    .text
                    .replace_range(range.clone(), &replacement);

                if replacement.is_empty() {
                    field_state.input.marked_range = None;
                } else {
                    field_state.input.marked_range =
                        Some(range.start..range.start + replacement.len());
                }

                let selected_range = new_selected_range_utf16
                    .map(|utf16_range| {
                        Self::search_input_range_from_utf16(&replacement, utf16_range)
                    })
                    .map(|relative_range| {
                        range.start + relative_range.start..range.start + relative_range.end
                    })
                    .unwrap_or_else(|| {
                        let cursor = range.start + replacement.len();
                        cursor..cursor
                    });
                field_state.input.selection_range = selected_range;
                field_state.input.selection_drag = None;
                field_state.clear_layout();
                dialog.error = None;
            }
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.connection_category_name_focused(window) {
            let replacement = Self::sanitize_search_input_text(new_text);
            if let Some(dialog) = self.connections.category_dialog.as_mut() {
                let input = &mut dialog.name.input;
                let range = range_utf16
                    .map(|range| Self::search_input_range_from_utf16(&input.text, range))
                    .or_else(|| input.marked_range.clone())
                    .unwrap_or_else(|| input.selection_range.clone());
                let range = Self::clamp_search_text_range(&input.text, range);
                input.text.replace_range(range.clone(), &replacement);

                if replacement.is_empty() {
                    input.marked_range = None;
                } else {
                    input.marked_range = Some(range.start..range.start + replacement.len());
                }

                let selected_range = new_selected_range_utf16
                    .map(|utf16_range| {
                        Self::search_input_range_from_utf16(&replacement, utf16_range)
                    })
                    .map(|relative_range| {
                        range.start + relative_range.start..range.start + relative_range.end
                    })
                    .unwrap_or_else(|| {
                        let cursor = range.start + replacement.len();
                        cursor..cursor
                    });
                input.selection_range = selected_range;
                input.selection_drag = None;
                dialog.name.clear_layout();
                dialog.error = None;
            }
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            if !self.settings.quick_search_keywords_is_editing {
                return;
            }
            let replacement = Self::sanitize_search_input_text(new_text);
            let range = range_utf16
                .map(|range| {
                    Self::search_input_range_from_utf16(
                        &self.settings.quick_search_keywords_input.text,
                        range,
                    )
                })
                .or_else(|| {
                    self.settings
                        .quick_search_keywords_input
                        .marked_range
                        .clone()
                })
                .unwrap_or_else(|| {
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .clone()
                });
            let range = Self::clamp_search_text_range(
                &self.settings.quick_search_keywords_input.text,
                range,
            );
            self.settings
                .quick_search_keywords_input
                .text
                .replace_range(range.clone(), &replacement);

            if replacement.is_empty() {
                self.settings.quick_search_keywords_input.marked_range = None;
            } else {
                self.settings.quick_search_keywords_input.marked_range =
                    Some(range.start..range.start + replacement.len());
            }

            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            self.settings.quick_search_keywords_input.selection_range = selected_range;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if let Some(index) = self.active_plugin_pattern_setting_index(window) {
            let replacement = Self::sanitize_search_input_text(new_text);
            if let Some(state) = self.settings.plugin_pattern_inputs.get_mut(index) {
                let range = range_utf16
                    .map(|range| Self::search_input_range_from_utf16(&state.input.text, range))
                    .or_else(|| state.input.marked_range.clone())
                    .unwrap_or_else(|| state.input.selection_range.clone());
                let range = Self::clamp_search_text_range(&state.input.text, range);
                state.input.text.replace_range(range.clone(), &replacement);

                if replacement.is_empty() {
                    state.input.marked_range = None;
                } else {
                    state.input.marked_range = Some(range.start..range.start + replacement.len());
                }

                state.input.selection_range = new_selected_range_utf16
                    .map(|utf16_range| {
                        Self::search_input_range_from_utf16(&replacement, utf16_range)
                    })
                    .map(|relative_range| {
                        range.start + relative_range.start..range.start + relative_range.end
                    })
                    .unwrap_or_else(|| {
                        let cursor = range.start + replacement.len();
                        cursor..cursor
                    });
                state.input.selection_drag = None;
                state.last_layout = None;
                state.last_bounds = None;
            }
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if let Some(kind) = self.active_thread_analysis_filter_input_kind(window) {
            if !self.settings.thread_analysis_filter_is_editing {
                return;
            }
            let replacement = normalize_thread_analysis_filter_text(new_text);
            let text = self.thread_analysis_filter_input_text(kind);
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(text, range))
                .or_else(|| self.thread_analysis_filter_input_marked_range(kind))
                .unwrap_or_else(|| self.thread_analysis_filter_input_selection_range(kind));
            let range = Self::clamp_search_text_range(text, range);
            self.thread_analysis_filter_input_text_mut(kind)
                .replace_range(range.clone(), &replacement);

            if replacement.is_empty() {
                self.set_thread_analysis_filter_input_marked_range(kind, None);
            } else {
                self.set_thread_analysis_filter_input_marked_range(
                    kind,
                    Some(range.start..range.start + replacement.len()),
                );
            }

            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            self.set_thread_analysis_filter_input_selection_range(kind, selected_range);
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        let input_kind = self.active_search_text_input_kind(window);
        let Some(dialog) = self.search.search_dialog.as_mut() else {
            return;
        };
        let replacement = Self::sanitize_search_input_text(new_text);
        let (target_text, selection_range, marked_range) =
            Self::search_text_state_mut(dialog, input_kind);
        let range = range_utf16
            .map(|range| Self::search_input_range_from_utf16(target_text, range))
            .or_else(|| marked_range.clone())
            .unwrap_or_else(|| selection_range.clone());
        let range = Self::clamp_search_text_range(target_text, range);
        target_text.replace_range(range.clone(), &replacement);

        if replacement.is_empty() {
            *marked_range = None;
        } else {
            *marked_range = Some(range.start..range.start + replacement.len());
        }

        let selected_range = new_selected_range_utf16
            .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
            .map(|relative_range| {
                range.start + relative_range.start..range.start + relative_range.end
            })
            .unwrap_or_else(|| {
                let cursor = range.start + replacement.len();
                cursor..cursor
            });
        *selection_range = selected_range;
        if input_kind == SearchTextInputKind::Query {
            dialog.query_history_menu_open = false;
            self.clear_search_current_file_match_count();
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 返回指定文本范围在屏幕上的边界，用于放置 IME 候选窗口。
    ///
    /// 实现原因：
    /// - 搜索输入框采用 GPUI 官方示例的自定义文本元素实现，最近一次 `ShapedLine` 可以提供真实字符位置。
    /// - 如果首次绘制前布局不可用，则回退到输入框整体边界，保证候选窗口仍贴近控件。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if let Some(tab) = self.focused_terminal_tab(window) {
            let bounds = tab.content_bounds.unwrap_or(element_bounds);
            let cursor = tab.emulator.cursor_point();
            let column = cursor.column.0 as f32;
            let line = cursor.line.0.max(0) as f32;
            let cell_width = tab.cell_width.max(1.0);
            let ime = &tab.ime;
            let range = Self::search_input_range_from_utf16(&ime.text, range_utf16);
            let start_offset = ime.text[..range.start].chars().count() as f32;
            let end_offset = ime.text[..range.end]
                .chars()
                .count()
                .max(ime.text[..range.start].chars().count().saturating_add(1))
                as f32;
            let start_column = column + start_offset;
            let end_column = column + end_offset;
            return Some(Bounds::from_corners(
                point(
                    bounds.left() + px(start_column * cell_width),
                    bounds.top() + px(line * CONNECTION_TERMINAL_ROW_HEIGHT),
                ),
                point(
                    bounds.left() + px(end_column * cell_width),
                    bounds.top() + px((line + 1.0) * CONNECTION_TERMINAL_ROW_HEIGHT),
                ),
            ));
        }
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let state = self.model_config_input_state(kind);
            let range = Self::search_input_range_from_utf16(&state.text, range_utf16);
            let Some(layout) = state.last_layout.as_ref() else {
                return Some(element_bounds);
            };
            return Some(Bounds::from_corners(
                point(
                    element_bounds.left() + layout.x_for_index(range.start)
                        - px(state.horizontal_scroll_px),
                    element_bounds.top(),
                ),
                point(
                    element_bounds.left() + layout.x_for_index(range.end)
                        - px(state.horizontal_scroll_px),
                    element_bounds.bottom(),
                ),
            ));
        }
        if self.notes.title_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(&self.notes.editor_title, range_utf16);
            let Some(layout) = self.notes.title_last_layout.as_ref() else {
                return Some(element_bounds);
            };
            return Some(Bounds::from_corners(
                point(
                    element_bounds.left() + layout.x_for_index(range.start),
                    element_bounds.top(),
                ),
                point(
                    element_bounds.left() + layout.x_for_index(range.end),
                    element_bounds.bottom(),
                ),
            ));
        }
        if self.notes.rich_editor.focus.is_focused(window) {
            let text = self.notes.rich_editor.plain_text();
            let range = Self::search_input_range_from_utf16(&text, range_utf16);
            return Some(
                self.notes
                    .rich_editor
                    .bounds_for_range(range, element_bounds),
            );
        }
        if self.notes.ai.input_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(&self.notes.ai.input_text, range_utf16);
            let cursor = range.start;
            for layout in &self.notes.ai.input_last_layouts {
                if cursor >= layout.byte_range.start && cursor <= layout.byte_range.end {
                    let x = layout
                        .line
                        .x_for_index(cursor.saturating_sub(layout.byte_range.start));
                    return Some(Bounds::new(
                        point(layout.bounds.left() + x, layout.bounds.top()),
                        size(px(1.0), layout.bounds.bottom() - layout.bounds.top()),
                    ));
                }
            }
            return Some(element_bounds);
        }
        if self.ai_chat.input_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(&self.ai_chat.input_text, range_utf16);
            let cursor = range.start;
            for layout in &self.ai_chat.input_last_layouts {
                if cursor >= layout.byte_range.start && cursor <= layout.byte_range.end {
                    let x = layout
                        .line
                        .x_for_index(cursor.saturating_sub(layout.byte_range.start));
                    return Some(Bounds::new(
                        point(layout.bounds.left() + x, layout.bounds.top()),
                        size(px(1.0), layout.bounds.bottom() - layout.bounds.top()),
                    ));
                }
            }
            return Some(element_bounds);
        }
        if self.log.log_tree_search.focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(
                &self.log.log_tree_search.input.text,
                range_utf16,
            );
            let Some(layout) = self.log.log_tree_search.last_layout.as_ref() else {
                return Some(element_bounds);
            };
            let horizontal_scroll_px = self.log.log_tree_search.input.horizontal_scroll_px;
            return Some(Bounds::from_corners(
                point(
                    element_bounds.left() + layout.x_for_index(range.start)
                        - px(horizontal_scroll_px),
                    element_bounds.top(),
                ),
                point(
                    element_bounds.left() + layout.x_for_index(range.end)
                        - px(horizontal_scroll_px),
                    element_bounds.bottom(),
                ),
            ));
        }
        if self.notes.tree_search.focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(
                &self.notes.tree_search.input.text,
                range_utf16,
            );
            let Some(layout) = self.notes.tree_search.last_layout.as_ref() else {
                return Some(element_bounds);
            };
            let horizontal_scroll_px = self.notes.tree_search.input.horizontal_scroll_px;
            return Some(Bounds::from_corners(
                point(
                    element_bounds.left() + layout.x_for_index(range.start)
                        - px(horizontal_scroll_px),
                    element_bounds.top(),
                ),
                point(
                    element_bounds.left() + layout.x_for_index(range.end)
                        - px(horizontal_scroll_px),
                    element_bounds.bottom(),
                ),
            ));
        }
        if self.connections.tree_search.focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(
                &self.connections.tree_search.input.text,
                range_utf16,
            );
            let Some(layout) = self.connections.tree_search.last_layout.as_ref() else {
                return Some(element_bounds);
            };
            let horizontal_scroll_px = self.connections.tree_search.input.horizontal_scroll_px;
            return Some(Bounds::from_corners(
                point(
                    element_bounds.left() + layout.x_for_index(range.start)
                        - px(horizontal_scroll_px),
                    element_bounds.top(),
                ),
                point(
                    element_bounds.left() + layout.x_for_index(range.end)
                        - px(horizontal_scroll_px),
                    element_bounds.bottom(),
                ),
            ));
        }
        if let Some(field) = self.active_connection_form_field(window) {
            let state = self.connections.dialog.as_ref()?.field(field);
            let range = Self::search_input_range_from_utf16(&state.input.text, range_utf16);
            let Some(layout) = state.last_layout.as_ref() else {
                return Some(element_bounds);
            };
            return Some(Bounds::from_corners(
                point(
                    element_bounds.left() + layout.x_for_index(range.start)
                        - px(state.input.horizontal_scroll_px),
                    element_bounds.top(),
                ),
                point(
                    element_bounds.left() + layout.x_for_index(range.end)
                        - px(state.input.horizontal_scroll_px),
                    element_bounds.bottom(),
                ),
            ));
        }
        if let Some(field) = self.active_smb_connection_form_field(window) {
            let state = self.connections.smb_dialog.as_ref()?.field(field);
            let range = Self::search_input_range_from_utf16(&state.input.text, range_utf16);
            let Some(layout) = state.last_layout.as_ref() else {
                return Some(element_bounds);
            };
            return Some(Bounds::from_corners(
                point(
                    element_bounds.left() + layout.x_for_index(range.start)
                        - px(state.input.horizontal_scroll_px),
                    element_bounds.top(),
                ),
                point(
                    element_bounds.left() + layout.x_for_index(range.end)
                        - px(state.input.horizontal_scroll_px),
                    element_bounds.bottom(),
                ),
            ));
        }
        if self.connection_category_name_focused(window) {
            let state = &self.connections.category_dialog.as_ref()?.name;
            let range = Self::search_input_range_from_utf16(&state.input.text, range_utf16);
            let Some(layout) = state.last_layout.as_ref() else {
                return Some(element_bounds);
            };
            return Some(Bounds::from_corners(
                point(
                    element_bounds.left() + layout.x_for_index(range.start)
                        - px(state.input.horizontal_scroll_px),
                    element_bounds.top(),
                ),
                point(
                    element_bounds.left() + layout.x_for_index(range.end)
                        - px(state.input.horizontal_scroll_px),
                    element_bounds.bottom(),
                ),
            ));
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(
                &self.settings.quick_search_keywords_input.text,
                range_utf16,
            );
            let Some(layout) = self.settings.quick_search_keywords_last_layout.as_ref() else {
                return Some(element_bounds);
            };
            let horizontal_scroll_px = self
                .settings
                .quick_search_keywords_input
                .horizontal_scroll_px;
            return Some(Bounds::from_corners(
                point(
                    element_bounds.left() + layout.x_for_index(range.start)
                        - px(horizontal_scroll_px),
                    element_bounds.top(),
                ),
                point(
                    element_bounds.left() + layout.x_for_index(range.end)
                        - px(horizontal_scroll_px),
                    element_bounds.bottom(),
                ),
            ));
        }
        if let Some(index) = self.active_plugin_pattern_setting_index(window) {
            let state = self.settings.plugin_pattern_inputs.get(index)?;
            let range = Self::search_input_range_from_utf16(&state.input.text, range_utf16);
            let Some(layout) = state.last_layout.as_ref() else {
                return Some(element_bounds);
            };
            let horizontal_scroll_px = state.input.horizontal_scroll_px;
            return Some(Bounds::from_corners(
                point(
                    element_bounds.left() + layout.x_for_index(range.start)
                        - px(horizontal_scroll_px),
                    element_bounds.top(),
                ),
                point(
                    element_bounds.left() + layout.x_for_index(range.end)
                        - px(horizontal_scroll_px),
                    element_bounds.bottom(),
                ),
            ));
        }
        if let Some(kind) = self.active_thread_analysis_filter_input_kind(window) {
            let text = self.thread_analysis_filter_input_text(kind);
            let range = Self::search_input_range_from_utf16(text, range_utf16);
            let cursor = range.start;
            let layouts = match kind {
                ThreadAnalysisFilterInputKind::ThreadName => {
                    &self.settings.thread_analysis_name_filter_last_layouts
                }
                ThreadAnalysisFilterInputKind::Stack => {
                    &self.settings.thread_analysis_filter_last_layouts
                }
            };
            for layout in layouts {
                if cursor >= layout.byte_range.start && cursor <= layout.byte_range.end {
                    let x = layout
                        .line
                        .x_for_index(cursor.saturating_sub(layout.byte_range.start));
                    return Some(Bounds::new(
                        point(layout.bounds.left() + x, layout.bounds.top()),
                        size(px(1.0), layout.bounds.bottom() - layout.bounds.top()),
                    ));
                }
            }
            return Some(element_bounds);
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, _, _, horizontal_scroll_px) = Self::search_text_state(dialog, input_kind);
        let range = Self::search_input_range_from_utf16(text, range_utf16);
        let layout = match input_kind {
            SearchTextInputKind::Query => self.search.search_query_last_layout.as_ref(),
            SearchTextInputKind::DirectoryTarget => {
                self.search.search_directory_last_layout.as_ref()
            }
        };
        let Some(layout) = layout else {
            return Some(element_bounds);
        };
        Some(Bounds::from_corners(
            point(
                element_bounds.left() + layout.x_for_index(range.start) - px(horizontal_scroll_px),
                element_bounds.top(),
            ),
            point(
                element_bounds.left() + layout.x_for_index(range.end) - px(horizontal_scroll_px),
                element_bounds.bottom(),
            ),
        ))
    }

    /// 根据鼠标位置返回文本插入点。
    ///
    /// 边界条件：
    /// - 平台 IME 可能通过该入口查询鼠标位置对应的字符；这里复用 GPUI 文本布局命中逻辑。
    /// - 如果布局尚不可用，则回退到文本末尾，避免平台输入协议收到非法下标。
    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<usize> {
        if self.terminal_input_focused_without_modal(window) {
            // 终端 IME 没有可编辑文档坐标，平台只需要一个合法插入点来维持候选窗口交互。
            return Some(0);
        }
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let utf8_index = self.model_config_input_index_for_point(kind, point);
            let state = self.model_config_input_state(kind);
            return Some(Self::search_input_utf16_offset_from_byte(
                &state.text,
                utf8_index,
            ));
        }
        if self.notes.title_focus.is_focused(window) {
            let utf8_index = self.note_title_index_for_point(point);
            return Some(Self::search_input_utf16_offset_from_byte(
                &self.notes.editor_title,
                utf8_index,
            ));
        }
        if self.notes.rich_editor.focus.is_focused(window) {
            let utf8_index = self.notes.rich_editor.index_for_point(point);
            let text = self.notes.rich_editor.plain_text();
            return Some(Self::search_input_utf16_offset_from_byte(&text, utf8_index));
        }
        if self.ai_chat.input_focus.is_focused(window) {
            let utf8_index = self.ai_chat_input_index_for_point(point);
            return Some(Self::search_input_utf16_offset_from_byte(
                &self.ai_chat.input_text,
                utf8_index,
            ));
        }
        if self.log.log_tree_search.focus.is_focused(window) {
            let utf8_index = self.log_tree_search_index_for_point(point);
            return Some(Self::search_input_utf16_offset_from_byte(
                &self.log.log_tree_search.input.text,
                utf8_index,
            ));
        }
        if self.notes.tree_search.focus.is_focused(window) {
            let utf8_index = self.notes_tree_search_text_index_for_point(point);
            return Some(Self::search_input_utf16_offset_from_byte(
                &self.notes.tree_search.input.text,
                utf8_index,
            ));
        }
        if let Some(field) = self.active_connection_form_field(window) {
            let utf8_index = self.connection_form_text_index_for_point(field, point);
            let state = self.connections.dialog.as_ref()?.field(field);
            return Some(Self::search_input_utf16_offset_from_byte(
                &state.input.text,
                utf8_index,
            ));
        }
        if let Some(field) = self.active_smb_connection_form_field(window) {
            let utf8_index = self.smb_connection_form_text_index_for_point(field, point);
            let state = self.connections.smb_dialog.as_ref()?.field(field);
            return Some(Self::search_input_utf16_offset_from_byte(
                &state.input.text,
                utf8_index,
            ));
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            let utf8_index = self.quick_search_keywords_index_for_point(point);
            return Some(Self::search_input_utf16_offset_from_byte(
                &self.settings.quick_search_keywords_input.text,
                utf8_index,
            ));
        }
        if let Some(index) = self.active_plugin_pattern_setting_index(window) {
            let utf8_index = self.plugin_pattern_setting_text_index_for_point(index, point);
            let state = self.settings.plugin_pattern_inputs.get(index)?;
            return Some(Self::search_input_utf16_offset_from_byte(
                &state.input.text,
                utf8_index,
            ));
        }
        if let Some(kind) = self.active_thread_analysis_filter_input_kind(window) {
            let utf8_index = self.thread_analysis_filter_index_for_point(kind, point);
            return Some(Self::search_input_utf16_offset_from_byte(
                self.thread_analysis_filter_input_text(kind),
                utf8_index,
            ));
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, _, _, _) = Self::search_text_state(dialog, input_kind);
        let utf8_index = self.search_text_index_for_point(input_kind, point);
        Some(Self::search_input_utf16_offset_from_byte(text, utf8_index))
    }
}

impl MainView {
    /// 返回当前聚焦的插件声明式设置输入框下标。
    ///
    /// 业务意图：
    /// - 插件设置输入框数量由 manifest 决定，平台输入法回调需要先定位当前焦点属于哪一条规则。
    /// - 下标只用于当前 `plugin_pattern_inputs` 列表，不跨插件重载或设置页重建保存。
    fn active_plugin_pattern_setting_index(&self, window: &Window) -> Option<usize> {
        self.settings
            .plugin_pattern_inputs
            .iter()
            .position(|input| input.focus.is_focused(window))
    }
}
