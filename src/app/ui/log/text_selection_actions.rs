// 日志正文文本选择辅助。
//
// 业务意图：
// - 从日志正文查看器中拆出 鼠标选区、单词/整行选择和指针坐标换算，让正文渲染、滚动、选区和菜单职责更清晰。
// - 本轮只移动方法边界并保留原有中文注释，不改变虚拟列表、分页日志、右键菜单或复制行为。

use super::*;

impl MainView {
    /// 开始选择日志正文文本。
    ///
    /// 业务意图：
    /// - 当前日志查看器是虚拟列表自绘，鼠标按下时需要主动记录选择锚点，后续拖动才能跨行扩展选区。
    /// - 点击日志正文同时关闭浮层菜单，避免选区操作和 tab 菜单、编码菜单叠加造成误操作。
    /// - 点击正文后主动聚焦主窗口根节点，让 `Ctrl/Cmd+C` 走根节点键盘兜底入口，从而复制自绘选区。
    ///
    /// 边界条件：
    /// - 只响应当前仍存在且已解码的 tab；加载中或失败状态没有可选择的正文。
    /// - 单击会形成空选择，视觉上不高亮，但会清理上一次选择，符合常见文本查看器行为。
    /// - 如果正在拖动日志滚动条或搜索结果面板高度，则正文不应进入选区模式，避免控件拖动被误解为文本拖选。
    pub(in crate::app) fn start_log_text_selection(
        &mut self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.note_keyboard_scroll_region(KeyboardScrollRegion::LogContent);
        if self.search.search_results_resize_drag.is_some()
            || self.log.log_scrollbar_drag.is_some()
            || self.log.log_minimap_drag.is_some()
        {
            return;
        }

        let Some(position) =
            self.log_text_position_from_pointer(tab_id, line_index, line, event.position.x, window)
        else {
            return;
        };
        let Some(tab) = self.log.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        if !matches!(&tab.state, LogTabState::Ready { .. }) {
            return;
        }

        window.focus(&self.root_focus_handle);
        let text_selection = match event.click_count {
            0 | 1 => LogTextSelection {
                anchor: position,
                focus: position,
            },
            2 => Self::word_selection_for_position(line_index, line, position).unwrap_or(
                LogTextSelection {
                    anchor: position,
                    focus: position,
                },
            ),
            _ => Self::line_selection_for_line(line_index, line),
        };

        tab.text_selection = Some(text_selection);
        tab.selection_drag_anchor = (event.click_count <= 1).then_some(position);
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        context.notify();
    }

    /// 构造整行选择范围。
    ///
    /// 业务意图：
    /// - 三连击是日志查看器中快速复制当前行的常见操作，需要直接选中当前可见行的全部真实字符。
    /// - 这里只选择行内文本，不主动附加换行符；跨行换行仍由 `selected_log_text` 在多行选择时统一处理。
    pub(in crate::app) fn line_selection_for_line(
        line_index: usize,
        line: &str,
    ) -> LogTextSelection {
        LogTextSelection {
            anchor: LogTextPosition {
                line_index,
                column: 0,
            },
            focus: LogTextPosition {
                line_index,
                column: line.chars().count(),
            },
        }
    }

    /// 根据点击位置构造当前日志 token 的选择范围。
    ///
    /// 业务意图：
    /// - 双击应选中“当前单词”，用户期望的是两个符号之间的连续文字，而不是整段包名、路径或线程名。
    /// - 因此 `.`、`-`、`_`、`:`、`/`、括号、引号等所有符号都作为边界，只保留连续字母/数字内容。
    ///
    /// 边界条件：
    /// - 如果用户双击在 token 右边界附近，命中列可能落在 token 后一列，此时优先回退到前一个字符。
    /// - 如果双击在纯空白或结构分隔符上，则返回 `None`，调用方退化为单点选择。
    pub(in crate::app) fn word_selection_for_position(
        line_index: usize,
        line: &str,
        position: LogTextPosition,
    ) -> Option<LogTextSelection> {
        let chars = line.chars().collect::<Vec<_>>();
        if chars.is_empty() {
            return None;
        }

        let original_column = position.column;
        let mut token_column = original_column.min(chars.len().saturating_sub(1));
        if !Self::is_log_word_char(chars[token_column])
            && token_column > 0
            && (original_column >= chars.len()
                || chars[token_column].is_whitespace()
                || Self::is_log_word_right_boundary_char(chars[token_column]))
            && Self::is_log_word_char(chars[token_column - 1])
        {
            token_column -= 1;
        }
        if !Self::is_log_word_char(chars[token_column]) {
            return None;
        }

        let mut start_column = token_column;
        while start_column > 0 && Self::is_log_word_char(chars[start_column - 1]) {
            start_column -= 1;
        }

        let mut end_column = token_column + 1;
        while end_column < chars.len() && Self::is_log_word_char(chars[end_column]) {
            end_column += 1;
        }

        Some(LogTextSelection {
            anchor: LogTextPosition {
                line_index,
                column: start_column,
            },
            focus: LogTextPosition {
                line_index,
                column: end_column,
            },
        })
    }

    /// 判断字符是否属于日志双击选择 token。
    ///
    /// 业务意图：
    /// - 双击选词按“两个符号之间的文字”处理，所有标点、路径分隔符、类名分隔符和键值分隔符都不能进入选区。
    /// - `is_alphanumeric` 覆盖英文、数字和中文等 Unicode 字母数字，避免中文日志中的普通词被拆坏。
    pub(in crate::app) fn is_log_word_char(character: char) -> bool {
        character.is_alphanumeric()
    }

    /// 判断当前符号是否允许按右边界回退到前一个单词。
    ///
    /// 业务意图：
    /// - GPUI 文本命中测试在用户双击单词右边缘时，可能返回后一个符号的插入列；Java 堆栈里的 `.` 最常见。
    /// - 这类符号仍然是单词边界，不进入选区，但允许回退到左侧单词，避免双击 `AESCipher.` 边缘时什么都不选。
    ///
    /// 边界条件：
    /// - `=` 等键值分隔符不做回退，用户双击字段分隔符时不应误选左侧 key。
    pub(in crate::app) fn is_log_word_right_boundary_char(character: char) -> bool {
        matches!(character, '.' | ')' | ']' | '}' | '>' | '"' | '\'')
    }

    /// 根据鼠标拖动更新日志正文选区。
    ///
    /// 业务意图：
    /// - 拖动过程中锚点保持不变，只更新焦点位置，从而支持任意方向选择。
    /// - 该函数只更新可见行上的拖动结果；虚拟列表外自动滚动选择后续需要单独定义交互规则。
    /// - 结果面板调高时，鼠标可能经过日志行，此时必须忽略底层正文选择，避免事件穿透造成误选。
    /// - 拖动日志正文滚动条时也必须忽略正文选择，避免滚动条拖动过程中出现选区和 hover 闪动。
    pub(in crate::app) fn update_log_text_selection(
        &mut self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.search.search_results_resize_drag.is_some()
            || self.log.log_scrollbar_drag.is_some()
            || self.log.log_minimap_drag.is_some()
        {
            self.stop_log_text_selection(context);
            return;
        }

        if !event.dragging() {
            self.stop_log_text_selection(context);
            return;
        }

        let Some(anchor) = self
            .log
            .open_tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| tab.selection_drag_anchor)
        else {
            return;
        };
        let Some(position) =
            self.log_text_position_from_pointer(tab_id, line_index, line, event.position.x, window)
        else {
            return;
        };
        let Some(tab) = self.log.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };

        tab.text_selection = Some(LogTextSelection {
            anchor,
            focus: position,
        });
        context.notify();
    }

    /// 结束日志正文选区拖动。
    ///
    /// 业务意图：
    /// - 鼠标释放后保留最终选区用于复制和搜索预填，但清理拖动锚点，避免下一次鼠标移动继续扩展旧选区。
    pub(in crate::app) fn stop_log_text_selection(&mut self, context: &mut Context<Self>) {
        let mut changed = false;
        for tab in &mut self.log.open_tabs {
            if tab.selection_drag_anchor.take().is_some() {
                changed = true;
            }
        }
        if changed {
            context.notify();
        }
    }

    /// 将鼠标横坐标换算为日志正文字符位置。
    ///
    /// 业务意图：
    /// - GPUI 鼠标事件提供窗口坐标，而日志正文在虚拟列表内会随横向滚动平移；命中测试必须把行号列、正文内边距和滚动偏移统一扣除。
    /// - 输出仍是字符列，复制和高亮时再按真实字符串转换为 UTF-8 字节范围。
    /// - 鼠标列计算必须复用 GPUI 文本系统的真实 shaping 结果，不能再用固定字符宽度估算；
    ///   否则行首空格、缩进、字体实际 advance 或平台字体渲染差异都会让视觉选区和真实文本列错位。
    ///
    /// 边界条件：
    /// - 指针落在正文起点左侧时归到第 0 列，落在行尾右侧时归到最后一列，避免越界。
    /// - `LineLayout::closest_index_for_x` 返回 UTF-8 字节下标，必须再转换为字符列，保证中文和 emoji 不会被切断。
    /// - shape 结果由 GPUI 按文本内容和字体缓存；拖动选择同一行时不会每帧都完整重建字形布局。
    pub(in crate::app) fn log_text_position_from_pointer(
        &self,
        tab_id: usize,
        line_index: usize,
        line: &str,
        pointer_x: Pixels,
        window: &mut Window,
    ) -> Option<LogTextPosition> {
        let tab = self.log.open_tabs.iter().find(|tab| tab.id == tab_id)?;
        let LogTabState::Ready { document } = &tab.state else {
            return None;
        };

        let line_number_width = Self::log_viewer_line_number_width(document.line_count());
        let (bounds, horizontal_offset) = match document.as_ref() {
            LogTabDocument::Paged(_) => (
                tab.paged_viewport_handle.bounds(),
                px(-(tab.paged_scroll.left_px as f32)),
            ),
            LogTabDocument::InMemory(_) => {
                let scroll_state = tab.scroll_handle.0.borrow();
                (
                    scroll_state.base_handle.bounds(),
                    scroll_state.base_handle.offset().x,
                )
            }
        };
        if bounds.size.width <= px(0.0) {
            return Some(LogTextPosition {
                line_index,
                column: 0,
            });
        }

        let text_origin_x = bounds.left()
            + horizontal_offset
            + px(line_number_width + LOG_VIEWER_TEXT_LEFT_PADDING);
        let text_relative_x = pointer_x - text_origin_x;
        if line.is_empty() || text_relative_x <= px(0.0) {
            return Some(LogTextPosition {
                line_index,
                column: 0,
            });
        }

        let mut text_style = window.text_style();
        text_style.font_family = LOG_VIEWER_FONT_FAMILY.into();
        text_style.font_size = px(self.settings.log_viewer_font_size).into();
        let expanded_line = Self::expanded_log_line_for_display(line);
        let run = TextRun {
            len: expanded_line.text.len(),
            font: text_style.font(),
            color: text_style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let shaped_line = window.text_system().shape_line(
            SharedString::from(expanded_line.text.clone()),
            font_size,
            &[run],
            None,
        );
        let display_byte_index = shaped_line.closest_index_for_x(text_relative_x);
        let original_byte_index =
            Self::original_byte_index_for_display_byte(&expanded_line, display_byte_index);
        let column = Self::char_column_for_byte_index(line, original_byte_index);

        Some(LogTextPosition { line_index, column })
    }

    /// 构造日志正文选区高亮样式。
    ///
    /// 业务意图：
    /// - 语法高亮按文字颜色表达，选区属于交互反馈，需要在不同主题下使用足够清晰的背景。
    /// - 亮色主题保留浅蓝背景和原有前景色，避免改变现有阅读习惯；暗色主题改用深蓝背景和浅色前景，解决绿色日志文本落在选区上对比度不足的问题。
    pub(in crate::app) fn log_text_selection_highlight_style(
        palette: AppThemePalette,
    ) -> gpui::HighlightStyle {
        if palette.background == AppThemePalette::for_theme(EffectiveTheme::Dark).background {
            gpui::HighlightStyle {
                color: Some(rgb(0xf8fafc).into()),
                background_color: Some(rgb(0x2563eb).into()),
                ..Default::default()
            }
        } else {
            gpui::HighlightStyle {
                background_color: Some(rgb(0xcfe8ff).into()),
                ..Default::default()
            }
        }
    }
}
