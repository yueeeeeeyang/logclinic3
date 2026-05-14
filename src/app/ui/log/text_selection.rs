// 日志正文选择与复制辅助。
//
// 业务意图：
// - 本文件由原主窗口工作区实现机械拆分而来，只承载 GPUI 壳层内的 日志正文选区、剪贴板复制和高亮坐标映射。
// - 拆分过程保持所有状态字段、事件时序和用户可见行为不变，便于后续按视图职责继续收束。

use super::*;

impl MainView {
    /// 复制当前激活日志 tab 的选中文本。
    ///
    /// 业务意图：
    /// - 日志正文是只读查看器，复制操作应从已解码的真实文本行中提取，而不是从屏幕像素或渲染元素反推。
    /// - 只有存在非空选择时才写入剪贴板，避免用户在搜索框等其它控件内按复制键时被日志查看器误拦截。
    ///
    /// 边界条件：
    /// - 复制文本保留跨行选择中的换行符，满足从日志中截取堆栈片段或多行上下文的常见需求。
    /// - 选择范围使用字符列，截取前会转换为 UTF-8 字节边界，中文不会被截断为非法字符串。
    pub(in crate::app) fn copy_selected_log_text(&self, context: &mut Context<Self>) -> bool {
        let Some(active_tab_id) = self.log.active_tab_id else {
            return false;
        };
        self.copy_selected_log_text_for_tab(active_tab_id, context)
    }

    /// 复制指定 tab 的日志正文选区。
    ///
    /// 业务意图：
    /// - 右键菜单打开时会绑定具体 tab，复制时不应受后续焦点或激活状态变化影响。
    pub(in crate::app) fn copy_selected_log_text_for_tab(
        &self,
        tab_id: usize,
        context: &mut Context<Self>,
    ) -> bool {
        let Some(text) = self.selected_log_text_for_tab(tab_id) else {
            return false;
        };
        if text.is_empty() {
            return false;
        }

        context.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// 取得当前激活日志 tab 的选中文本。
    pub(in crate::app) fn selected_log_text(&self) -> Option<String> {
        let active_tab_id = self.log.active_tab_id?;
        self.selected_log_text_for_tab(active_tab_id)
    }

    /// 取得指定日志 tab 的选中文本。
    ///
    /// 边界条件：
    /// - tab 不存在、尚未加载成功或选区为空时返回 `None`，用于禁用右键菜单“复制”。
    pub(in crate::app) fn selected_log_text_for_tab(&self, tab_id: usize) -> Option<String> {
        let tab = self.log.open_tabs.iter().find(|tab| tab.id == tab_id)?;
        let selection = tab.text_selection.as_ref()?;
        if selection.is_empty() {
            return None;
        }
        let LogTabState::Ready { document } = &tab.state else {
            return None;
        };

        let (start, end) = selection.normalized();
        let line_count = document.line_count();
        if line_count == 0 || start.line_index >= line_count {
            return None;
        }
        let end_line_index = end.line_index.min(line_count.saturating_sub(1));
        if start.line_index > end_line_index {
            return None;
        }

        let mut selected_text = String::new();
        for line_index in start.line_index..=end_line_index {
            let Some(line) = Self::log_document_line_text(document, line_index) else {
                continue;
            };
            if line_index > start.line_index {
                selected_text.push('\n');
            }
            let Some((start_column, end_column)) =
                Self::selection_columns_for_line(selection, line_index, &line)
            else {
                continue;
            };
            let start_byte = Self::byte_index_for_char_column(&line, start_column);
            let end_byte = Self::byte_index_for_char_column(&line, end_column);
            if start_byte < end_byte {
                selected_text.push_str(&line[start_byte..end_byte]);
            }
        }

        Some(selected_text)
    }

    /// 取得可填入搜索框的日志选中文本。
    ///
    /// 业务意图：
    /// - 当前搜索引擎是逐行普通文本搜索，搜索框也是单行输入；当用户选中多行日志时，直接填入换行会让搜索条件不可用。
    /// - 因此这里取第一段非空行作为搜索关键字，复制功能仍保留完整多行内容。
    pub(in crate::app) fn selected_log_text_for_search_query(&self) -> Option<String> {
        self.selected_log_text()
            .and_then(|text| {
                text.lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(ToOwned::to_owned)
            })
            .filter(|query| !query.is_empty())
    }

    /// 把剪贴板文本粘贴到搜索关键字输入框。
    ///
    /// 业务意图：
    /// - 日志正文是只读查看器，`Ctrl+V` 不能修改日志内容；用户通常是想把剪贴板里的关键字拿来搜索。
    /// - 如果搜索窗口尚未打开，则先创建搜索状态并排队打开窗口；如果已经打开，则替换当前关键字选区。
    ///
    /// 边界条件：
    /// - 非文本剪贴板、空文本或只有换行的文本不处理，避免清空用户当前搜索条件。
    /// - 粘贴文本按搜索框单行规则清理换行，和平台输入法提交路径保持一致。
    pub(in crate::app) fn paste_clipboard_text_into_search_dialog(
        &mut self,
        window: &mut Window,
        context: &mut Context<Self>,
    ) -> bool {
        let Some(text) = context
            .read_from_clipboard()
            .and_then(|item| item.text())
            .map(|text| Self::sanitize_search_input_text(&text))
            .filter(|text| !text.trim().is_empty())
        else {
            return false;
        };

        self.prepare_search_dialog_state(SearchDialogOpenPreset::Default);
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            Self::replace_search_query_with_clipboard_text(dialog, text);
        }
        self.schedule_open_search_dialog(window, context);
        self.touch_search_text_cursor_activity();
        context.notify();
        true
    }

    /// 按统一文档模型读取指定行文本。
    ///
    /// 业务意图：
    /// - 小文件行文本来自内存 `Vec<String>`，超大文件行文本来自分页 seek 读取；复制、渲染和命中定位需要共享同一入口。
    /// - 分页读取失败时返回 `None`，避免复制或临时渲染路径因单行 I/O 错误直接崩溃；真正的打开错误仍在后台任务阶段展示。
    pub(in crate::app) fn log_document_line_text(
        document: &LogTabDocument,
        line_index: usize,
    ) -> Option<String> {
        match document {
            LogTabDocument::InMemory(document) => document.lines.get(line_index).cloned(),
            LogTabDocument::Paged(document) => document
                .read_line(line_index)
                .ok()
                .flatten()
                .map(|line| line.text),
        }
    }

    /// 返回某一行被当前选择覆盖的字符列范围。
    ///
    /// 业务意图：
    /// - 渲染选区和复制文本都需要同一套范围规则，避免屏幕高亮和复制结果不一致。
    /// - 这里输出字符列而不是字节范围，由调用方按具体字符串转换为 UTF-8 安全字节边界。
    pub(in crate::app) fn selection_columns_for_line(
        selection: &LogTextSelection,
        line_index: usize,
        line: &str,
    ) -> Option<(usize, usize)> {
        let (start, end) = selection.normalized();
        if line_index < start.line_index || line_index > end.line_index {
            return None;
        }

        let line_char_count = line.chars().count();
        let start_column = if line_index == start.line_index {
            start.column.min(line_char_count)
        } else {
            0
        };
        let end_column = if line_index == end.line_index {
            end.column.min(line_char_count)
        } else {
            line_char_count
        };

        Some((start_column, end_column))
    }

    /// 返回某一行被选择覆盖的 UTF-8 字节范围。
    ///
    /// 边界条件：
    /// - 空范围不参与渲染高亮，但复制跨行选择时仍会通过行间换行保留空行语义。
    pub(in crate::app) fn selected_byte_range_for_line(
        selection: &LogTextSelection,
        line_index: usize,
        line: &str,
    ) -> Option<Range<usize>> {
        let (start_column, end_column) =
            Self::selection_columns_for_line(selection, line_index, line)?;
        let start_byte = Self::byte_index_for_char_column(line, start_column);
        let end_byte = Self::byte_index_for_char_column(line, end_column);
        (start_byte < end_byte).then_some(start_byte..end_byte)
    }

    /// 将字符列转换为字符串的 UTF-8 字节下标。
    ///
    /// 业务意图：
    /// - 鼠标选择以字符列表达，`String::replace_range`、切片和 `StyledText` 高亮范围都要求 UTF-8 字节边界。
    /// - 统一转换函数可以避免中文、全角字符或 emoji 出现在选区边缘时产生非法切片。
    pub(in crate::app) fn byte_index_for_char_column(text: &str, column: usize) -> usize {
        if column == 0 {
            return 0;
        }
        text.char_indices()
            .nth(column)
            .map(|(index, _)| index)
            .unwrap_or(text.len())
    }

    /// 将 UTF-8 字节下标转换为字符列。
    ///
    /// 业务意图：
    /// - GPUI 文本 shaping 的命中结果以字节下标表达，而日志选区状态使用字符列保存。
    /// - 统一转换后，鼠标命中、选区高亮、复制和搜索预填可以继续共用字符列模型。
    ///
    /// 边界条件：
    /// - 正常情况下 GPUI 返回的字节下标位于字符边界；这里仍做边界回退，避免未来字体 shaping 或
    ///   组合字符场景返回中间下标时导致字符串切片 panic。
    pub(in crate::app) fn char_column_for_byte_index(text: &str, byte_index: usize) -> usize {
        let byte_index = byte_index.min(text.len());
        let safe_byte_index = if text.is_char_boundary(byte_index) {
            byte_index
        } else {
            text.char_indices()
                .map(|(index, _)| index)
                .take_while(|index| *index < byte_index)
                .last()
                .unwrap_or(0)
        };

        text[..safe_byte_index].chars().count()
    }

    /// 将日志行中的制表符展开为显示用空格，并保留原始文本到显示文本的字节映射。
    ///
    /// 业务意图：
    /// - 日志字段常用 `\t` 分隔，直接渲染会导致不同平台或 GPUI 文本系统下间隔过窄，字段看起来粘连。
    /// - 展开时按固定 4 列 tab stop 计算空格数，而不是简单替换成 4 个空格，才能让后续列落在稳定边界。
    ///
    /// 边界条件：
    /// - 这里只处理单行文本，换行已经由日志解码阶段拆分。
    /// - UTF-8 多字节字符在 JetBrains Mono 下仍按一个字符列推进；中文全角宽度不在本次 tab 对齐规则内扩展。
    pub(in crate::app) fn expanded_log_line_for_display(line: &str) -> ExpandedLogLine {
        let mut text = String::with_capacity(line.len());
        let mut original_to_display_bytes = vec![0; line.len() + 1];
        let mut display_to_original_bytes = vec![0];
        let mut display_column = 0usize;

        for (original_start, character) in line.char_indices() {
            let original_end = original_start + character.len_utf8();
            let display_start = text.len();

            if character == '\t' {
                let spaces = LOG_VIEWER_TAB_WIDTH - (display_column % LOG_VIEWER_TAB_WIDTH);
                for offset in 0..spaces {
                    text.push(' ');
                    display_to_original_bytes.push(if offset == 0 {
                        original_start
                    } else {
                        original_end
                    });
                }
                display_column += spaces;
            } else {
                text.push(character);
                for _ in display_start..text.len() {
                    display_to_original_bytes.push(original_start);
                }
                display_column += 1;
            }

            let display_end = text.len();
            for mapped_display_start in original_to_display_bytes
                .iter_mut()
                .take(original_end)
                .skip(original_start)
            {
                *mapped_display_start = display_start;
            }
            original_to_display_bytes[original_end] = display_end;
            display_to_original_bytes[display_end] = original_end;
        }

        original_to_display_bytes[line.len()] = text.len();
        display_to_original_bytes[text.len()] = line.len();

        ExpandedLogLine {
            text,
            original_to_display_bytes,
            display_to_original_bytes,
        }
    }

    /// 将原始日志字节下标换算为展开后的显示字节下标。
    ///
    /// 业务意图：
    /// - 语法高亮、搜索高亮和选区高亮都是基于原始日志文本计算的，渲染前必须同步平移到显示文本。
    pub(in crate::app) fn display_byte_index_for_original_byte(
        expanded: &ExpandedLogLine,
        byte_index: usize,
    ) -> usize {
        expanded
            .original_to_display_bytes
            .get(byte_index)
            .copied()
            .unwrap_or(expanded.text.len())
    }

    /// 将显示文本字节下标换算回原始日志字节下标。
    ///
    /// 业务意图：
    /// - 鼠标命中测试发生在展开后的显示文本上，但选区状态保存原始文本字符列；这里负责把二者接回同一坐标系。
    pub(in crate::app) fn original_byte_index_for_display_byte(
        expanded: &ExpandedLogLine,
        byte_index: usize,
    ) -> usize {
        expanded
            .display_to_original_bytes
            .get(byte_index)
            .copied()
            .unwrap_or_else(|| expanded.original_to_display_bytes.len().saturating_sub(1))
    }

    /// 将基于原始日志文本的高亮范围映射到展开后的显示文本。
    ///
    /// 业务意图：
    /// - tab 展开后显示文本长度变长，如果继续使用原始字节范围，搜索命中、语法高亮和选区背景都会向左错位。
    /// - 映射时保留原来的 `HighlightStyle`，只调整字节范围。
    ///
    /// 边界条件：
    /// - 空范围或越界范围会被压缩到安全边界后丢弃，避免传给 GPUI 非法高亮区间。
    pub(in crate::app) fn map_log_highlights_to_display(
        highlights: Vec<(Range<usize>, gpui::HighlightStyle)>,
        expanded: &ExpandedLogLine,
    ) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
        let original_len = expanded.original_to_display_bytes.len().saturating_sub(1);
        highlights
            .into_iter()
            .filter_map(|(range, style)| {
                let start = range.start.min(original_len);
                let end = range.end.min(original_len);
                let display_start = Self::display_byte_index_for_original_byte(expanded, start);
                let display_end = Self::display_byte_index_for_original_byte(expanded, end);
                (display_start < display_end).then_some((display_start..display_end, style))
            })
            .collect()
    }
}
