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
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let state = self.model_config_input_state(kind);
            let range = Self::search_input_range_from_utf16(&state.text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &state.text,
                range.clone(),
            ));
            return Some(state.text[range].to_string());
        }
        if self.ai_chat.input_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(&self.ai_chat.input_text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &self.ai_chat.input_text,
                range.clone(),
            ));
            return Some(self.ai_chat.input_text[range].to_string());
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
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            let range = Self::search_input_range_from_utf16(
                &self.settings.thread_analysis_filter_text,
                range_utf16,
            );
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &self.settings.thread_analysis_filter_text,
                range.clone(),
            ));
            return Some(self.settings.thread_analysis_filter_text[range].to_string());
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, _, _) = Self::search_text_state(dialog, input_kind);
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
        if self.ai_chat.input_focus.is_focused(window) {
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &self.ai_chat.input_text,
                    self.ai_chat.input_selection_range.clone(),
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
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &self.settings.thread_analysis_filter_text,
                    self.settings.thread_analysis_filter_selection_range.clone(),
                ),
                reversed: false,
            });
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, selection_range, _) = Self::search_text_state(dialog, input_kind);
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
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let state = self.model_config_input_state(kind);
            return state
                .marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&state.text, range));
        }
        if self.ai_chat.input_focus.is_focused(window) {
            return self
                .ai_chat
                .input_marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&self.ai_chat.input_text, range));
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
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            return self
                .settings
                .thread_analysis_filter_marked_range
                .clone()
                .map(|range| {
                    Self::search_input_range_to_utf16(
                        &self.settings.thread_analysis_filter_text,
                        range,
                    )
                });
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, _, marked_range) = Self::search_text_state(dialog, input_kind);
        marked_range.map(|range| Self::search_input_range_to_utf16(text, range))
    }

    /// 清除输入法组合文本状态。
    fn unmark_text(&mut self, window: &mut Window, context: &mut Context<Self>) {
        if let Some(kind) = self.active_model_config_input_kind(window) {
            self.model_config_input_state_mut(kind).marked_range = None;
            context.notify();
            return;
        }
        if self.ai_chat.input_focus.is_focused(window) {
            self.ai_chat.input_marked_range = None;
            context.notify();
            return;
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            self.settings.quick_search_keywords_input.marked_range = None;
            context.notify();
            return;
        }
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            self.settings.thread_analysis_filter_marked_range = None;
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
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            if !self.settings.thread_analysis_filter_is_editing {
                return;
            }
            let replacement = normalize_thread_analysis_filter_text(text);
            let range = range_utf16
                .map(|range| {
                    Self::search_input_range_from_utf16(
                        &self.settings.thread_analysis_filter_text,
                        range,
                    )
                })
                .or_else(|| self.settings.thread_analysis_filter_marked_range.clone())
                .unwrap_or_else(|| self.settings.thread_analysis_filter_selection_range.clone());
            let range =
                Self::clamp_search_text_range(&self.settings.thread_analysis_filter_text, range);
            self.settings
                .thread_analysis_filter_text
                .replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            self.settings.thread_analysis_filter_selection_range = cursor..cursor;
            self.settings.thread_analysis_filter_marked_range = None;
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
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            if !self.settings.thread_analysis_filter_is_editing {
                return;
            }
            let replacement = normalize_thread_analysis_filter_text(new_text);
            let range = range_utf16
                .map(|range| {
                    Self::search_input_range_from_utf16(
                        &self.settings.thread_analysis_filter_text,
                        range,
                    )
                })
                .or_else(|| self.settings.thread_analysis_filter_marked_range.clone())
                .unwrap_or_else(|| self.settings.thread_analysis_filter_selection_range.clone());
            let range =
                Self::clamp_search_text_range(&self.settings.thread_analysis_filter_text, range);
            self.settings
                .thread_analysis_filter_text
                .replace_range(range.clone(), &replacement);

            if replacement.is_empty() {
                self.settings.thread_analysis_filter_marked_range = None;
            } else {
                self.settings.thread_analysis_filter_marked_range =
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
            self.settings.thread_analysis_filter_selection_range = selected_range;
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
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let state = self.model_config_input_state(kind);
            let range = Self::search_input_range_from_utf16(&state.text, range_utf16);
            let Some(layout) = state.last_layout.as_ref() else {
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
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(
                &self.settings.quick_search_keywords_input.text,
                range_utf16,
            );
            let Some(layout) = self.settings.quick_search_keywords_last_layout.as_ref() else {
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
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            let range = Self::search_input_range_from_utf16(
                &self.settings.thread_analysis_filter_text,
                range_utf16,
            );
            let cursor = range.start;
            for layout in &self.settings.thread_analysis_filter_last_layouts {
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
        let (text, _, _) = Self::search_text_state(dialog, input_kind);
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
                element_bounds.left() + layout.x_for_index(range.start),
                element_bounds.top(),
            ),
            point(
                element_bounds.left() + layout.x_for_index(range.end),
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
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let utf8_index = self.model_config_input_index_for_point(kind, point);
            let state = self.model_config_input_state(kind);
            return Some(Self::search_input_utf16_offset_from_byte(
                &state.text,
                utf8_index,
            ));
        }
        if self.ai_chat.input_focus.is_focused(window) {
            let utf8_index = self.ai_chat_input_index_for_point(point);
            return Some(Self::search_input_utf16_offset_from_byte(
                &self.ai_chat.input_text,
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
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            let utf8_index = self.thread_analysis_filter_index_for_point(point);
            return Some(Self::search_input_utf16_offset_from_byte(
                &self.settings.thread_analysis_filter_text,
                utf8_index,
            ));
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, _, _) = Self::search_text_state(dialog, input_kind);
        let utf8_index = self.search_text_index_for_point(input_kind, point);
        Some(Self::search_input_utf16_offset_from_byte(text, utf8_index))
    }
}
