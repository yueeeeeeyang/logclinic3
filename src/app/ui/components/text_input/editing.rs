// 通用单行输入框编辑核心。
//
// 业务意图：
// - 各功能自绘输入框过去分别实现字符边界、复制粘贴、方向键、删除和鼠标选择，导致行为不一致。
// - 本文件只放纯状态规则，便于搜索、设置、连接和后续更多输入框在不依赖 GPUI 渲染的情况下复用和测试。

use super::*;

/// 输入编辑动作执行后的结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::app) struct TextInputEditOutcome {
    /// 事件是否属于输入框并应被消费。
    pub(in crate::app) consumed: bool,
    /// 真实文本是否发生变化。
    pub(in crate::app) changed: bool,
}

impl TextInputEditOutcome {
    /// 返回仅消费事件但未修改文本的结果。
    pub(in crate::app) fn consumed() -> Self {
        Self {
            consumed: true,
            changed: false,
        }
    }

    /// 返回已修改文本的结果。
    pub(in crate::app) fn changed() -> Self {
        Self {
            consumed: true,
            changed: true,
        }
    }
}

/// 将平台输入文本清理为单行内容。
pub(in crate::app) fn sanitize_text_input_single_line_text(text: &str) -> String {
    text.replace(['\n', '\r'], "")
}

/// 将任意字节下标夹到 UTF-8 字符边界。
pub(in crate::app) fn text_input_clamp_byte_index(text: &str, index: usize) -> usize {
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

/// 将输入范围夹到合法 UTF-8 字符边界。
pub(in crate::app) fn text_input_clamp_range(text: &str, range: Range<usize>) -> Range<usize> {
    let start = text_input_clamp_byte_index(text, range.start);
    let end = text_input_clamp_byte_index(text, range.end);
    start.min(end)..start.max(end)
}

/// 返回当前光标左侧的前一个字符边界。
pub(in crate::app) fn text_input_previous_boundary(text: &str, offset: usize) -> usize {
    let offset = text_input_clamp_byte_index(text, offset);
    text[..offset]
        .char_indices()
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

/// 返回当前光标右侧的下一个字符边界。
pub(in crate::app) fn text_input_next_boundary(text: &str, offset: usize) -> usize {
    let offset = text_input_clamp_byte_index(text, offset);
    text[offset..]
        .char_indices()
        .nth(1)
        .map(|(index, _)| offset + index)
        .unwrap_or(text.len())
}

/// 将平台 UTF-16 范围转换为 UTF-8 字节范围。
pub(in crate::app) fn text_input_range_from_utf16(
    text: &str,
    range_utf16: Range<usize>,
) -> Range<usize> {
    let start = text_input_byte_index_from_utf16(text, range_utf16.start);
    let end = text_input_byte_index_from_utf16(text, range_utf16.end);
    start.min(end)..start.max(end)
}

/// 将 UTF-8 字节范围转换为平台 UTF-16 范围。
pub(in crate::app) fn text_input_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    let range = text_input_clamp_range(text, range);
    let start = text_input_utf16_offset_from_byte(text, range.start);
    let end = text_input_utf16_offset_from_byte(text, range.end);
    start..end
}

/// 将平台 UTF-16 偏移映射到 UTF-8 字节边界。
pub(in crate::app) fn text_input_byte_index_from_utf16(text: &str, target_utf16: usize) -> usize {
    let mut utf16_cursor = 0usize;
    for (byte_index, character) in text.char_indices() {
        let next_utf16_cursor = utf16_cursor + character.len_utf16();
        // 平台选择范围偶尔会落在 UTF-16 代理对内部；Rust 字符串只能按 UTF-8
        // 字符边界切片，因此内部偏移必须回退到当前字符起点，而不是越过整个字符。
        if target_utf16 == utf16_cursor || target_utf16 < next_utf16_cursor {
            return byte_index;
        }
        utf16_cursor = next_utf16_cursor;
    }
    text.len()
}

/// 将 UTF-8 字节边界映射到平台 UTF-16 偏移。
pub(in crate::app) fn text_input_utf16_offset_from_byte(text: &str, byte_index: usize) -> usize {
    let byte_index = text_input_clamp_byte_index(text, byte_index);
    text.char_indices()
        .take_while(|(index, _)| *index < byte_index)
        .map(|(_, character)| character.len_utf16())
        .sum()
}

/// 返回双击选择的词范围。
///
/// 业务意图：
/// - 日志搜索、路径和配置输入中标点都有业务含义；沿用现有规则，双击选择连续非空白片段。
pub(in crate::app) fn text_input_word_range_for_index(text: &str, index: usize) -> Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let index = text_input_clamp_range(text, index..index).start;
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

/// 返回当前选中文本。
pub(in crate::app) fn text_input_selected_text(input: &SingleLineTextInputState) -> Option<String> {
    let range = text_input_clamp_range(&input.text, input.selection_range.clone());
    (range.start < range.end).then(|| input.text[range].to_string())
}

/// 用给定文本替换当前选区或组合文本。
pub(in crate::app) fn replace_text_input_selection(
    input: &mut SingleLineTextInputState,
    replacement: &str,
) {
    let range = input
        .marked_range
        .take()
        .unwrap_or_else(|| text_input_clamp_range(&input.text, input.selection_range.clone()));
    input.text.replace_range(range.clone(), replacement);
    let cursor = range.start + replacement.len();
    input.selection_range = cursor..cursor;
    input.horizontal_scroll_px = 0.0;
    // 文本替换会改变 UTF-8 字节下标，旧拖拽锚点必须失效，避免释放事件之后继续按旧位置扩展选区。
    input.selection_drag = None;
}

/// 选择全部文本。
pub(in crate::app) fn select_all_text_input(input: &mut SingleLineTextInputState) {
    input.marked_range = None;
    input.selection_range = 0..input.text.len();
    input.selection_drag = None;
}

/// 光标向左移动。
pub(in crate::app) fn move_text_input_left(
    input: &mut SingleLineTextInputState,
    extend_selection: bool,
) -> TextInputEditOutcome {
    input.marked_range = None;
    input.selection_drag = None;
    if extend_selection {
        input.selection_range.end =
            text_input_previous_boundary(&input.text, input.selection_range.end);
    } else if input.selection_range.start != input.selection_range.end {
        let collapsed = text_input_clamp_range(&input.text, input.selection_range.clone()).start;
        input.selection_range = collapsed..collapsed;
    } else {
        let cursor = text_input_previous_boundary(&input.text, input.selection_range.end);
        input.selection_range = cursor..cursor;
    }
    TextInputEditOutcome::consumed()
}

/// 光标向右移动。
pub(in crate::app) fn move_text_input_right(
    input: &mut SingleLineTextInputState,
    extend_selection: bool,
) -> TextInputEditOutcome {
    input.marked_range = None;
    input.selection_drag = None;
    if extend_selection {
        input.selection_range.end =
            text_input_next_boundary(&input.text, input.selection_range.end);
    } else if input.selection_range.start != input.selection_range.end {
        let collapsed = text_input_clamp_range(&input.text, input.selection_range.clone()).end;
        input.selection_range = collapsed..collapsed;
    } else {
        let cursor = text_input_next_boundary(&input.text, input.selection_range.end);
        input.selection_range = cursor..cursor;
    }
    TextInputEditOutcome::consumed()
}

/// 光标移动到文本开头。
pub(in crate::app) fn move_text_input_home(
    input: &mut SingleLineTextInputState,
    extend_selection: bool,
) -> TextInputEditOutcome {
    input.marked_range = None;
    input.selection_drag = None;
    if extend_selection {
        input.selection_range.end = 0;
    } else {
        input.selection_range = 0..0;
    }
    TextInputEditOutcome::consumed()
}

/// 光标移动到文本末尾。
pub(in crate::app) fn move_text_input_end(
    input: &mut SingleLineTextInputState,
    extend_selection: bool,
) -> TextInputEditOutcome {
    input.marked_range = None;
    input.selection_drag = None;
    let end = input.text.len();
    if extend_selection {
        input.selection_range.end = end;
    } else {
        input.selection_range = end..end;
    }
    TextInputEditOutcome::consumed()
}

/// 删除光标左侧字符或当前选区。
pub(in crate::app) fn backspace_text_input(
    input: &mut SingleLineTextInputState,
) -> TextInputEditOutcome {
    input.selection_drag = None;
    if let Some(range) = input.marked_range.take().or_else(|| {
        (input.selection_range.start != input.selection_range.end)
            .then(|| input.selection_range.clone())
    }) {
        let range = text_input_clamp_range(&input.text, range);
        input.text.replace_range(range.clone(), "");
        input.selection_range = range.start..range.start;
        input.horizontal_scroll_px = 0.0;
        return TextInputEditOutcome::changed();
    }
    if let Some((previous_index, _)) = input.text[..input.selection_range.end]
        .char_indices()
        .next_back()
    {
        input
            .text
            .replace_range(previous_index..input.selection_range.end, "");
        input.selection_range = previous_index..previous_index;
        input.horizontal_scroll_px = 0.0;
        return TextInputEditOutcome::changed();
    }
    TextInputEditOutcome::consumed()
}

/// 删除光标右侧字符或当前选区。
pub(in crate::app) fn delete_text_input(
    input: &mut SingleLineTextInputState,
) -> TextInputEditOutcome {
    input.selection_drag = None;
    if let Some(range) = input.marked_range.take().or_else(|| {
        (input.selection_range.start != input.selection_range.end)
            .then(|| input.selection_range.clone())
    }) {
        let range = text_input_clamp_range(&input.text, range);
        input.text.replace_range(range.clone(), "");
        input.selection_range = range.start..range.start;
        input.horizontal_scroll_px = 0.0;
        return TextInputEditOutcome::changed();
    }
    if let Some((next_index, next_character)) = input.text[input.selection_range.end..]
        .char_indices()
        .next()
    {
        let start = input.selection_range.end + next_index;
        let end = start + next_character.len_utf8();
        input.text.replace_range(start..end, "");
        input.selection_range = start..start;
        input.horizontal_scroll_px = 0.0;
        return TextInputEditOutcome::changed();
    }
    TextInputEditOutcome::consumed()
}

/// 处理鼠标按下时的单行输入选区规则。
pub(in crate::app) fn begin_text_input_mouse_selection(
    input: &mut SingleLineTextInputState,
    index: usize,
    click_count: usize,
    shift: bool,
) -> Option<usize> {
    let index = text_input_clamp_byte_index(&input.text, index);
    input.marked_range = None;
    match click_count {
        0 | 1 => {
            if shift {
                input.selection_range.end = index;
            } else {
                input.selection_range = index..index;
            }
            let anchor = input.selection_range.start;
            input.selection_drag = Some(anchor);
            Some(anchor)
        }
        2 => {
            input.selection_range = text_input_word_range_for_index(&input.text, index);
            input.selection_drag = None;
            None
        }
        _ => {
            input.selection_range = 0..input.text.len();
            input.selection_drag = None;
            None
        }
    }
}

/// 鼠标拖拽时更新选区终点。
pub(in crate::app) fn update_text_input_mouse_selection(
    input: &mut SingleLineTextInputState,
    anchor: usize,
    index: usize,
) {
    input.marked_range = None;
    input.selection_range = text_input_clamp_range(&input.text, anchor..index);
}

/// 计算单行输入框水平滚动偏移。
pub(in crate::app) fn text_input_horizontal_scroll_offset(
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造测试用输入状态。
    fn make_input(text: &str, selection_range: Range<usize>) -> SingleLineTextInputState {
        SingleLineTextInputState {
            text: text.to_string(),
            selection_range,
            marked_range: None,
            horizontal_scroll_px: 0.0,
            selection_drag: None,
        }
    }

    /// 验证 UTF-8 字符边界夹紧不会切坏中文。
    #[test]
    fn 输入范围会夹到_utf8_字符边界() {
        assert_eq!(text_input_clamp_range("a中b", 2..99), 1..5);
        assert_eq!(text_input_clamp_range("a中b", 4..0), 0..4);
    }

    /// 验证 UTF-16 和 UTF-8 范围转换覆盖 emoji 代理对。
    #[test]
    fn 输入范围支持_utf16_和_utf8_转换() {
        let text = "a🙂b";
        let range = text_input_range_from_utf16(text, 1..3);
        assert_eq!(&text[range.clone()], "🙂");
        assert_eq!(text_input_range_to_utf16(text, range), 1..3);
        assert_eq!(text_input_byte_index_from_utf16(text, 2), 1);
    }

    /// 验证替换优先使用组合文本范围。
    #[test]
    fn 替换输入优先覆盖组合文本() {
        let mut input = make_input("abc", 3..3);
        input.marked_range = Some(1..2);
        replace_text_input_selection(&mut input, "中");
        assert_eq!(input.text, "a中c");
        assert_eq!(input.selection_range, 4..4);
        assert!(input.marked_range.is_none());
    }

    /// 验证方向键和 Home/End 支持 Shift 扩展选区。
    #[test]
    fn 输入方向键和_home_end_会移动或扩展选区() {
        let mut input = make_input("a中b", 5..5);
        move_text_input_left(&mut input, true);
        assert_eq!(input.selection_range, 5..4);
        move_text_input_left(&mut input, true);
        assert_eq!(input.selection_range, 5..1);
        move_text_input_home(&mut input, false);
        assert_eq!(input.selection_range, 0..0);
        move_text_input_end(&mut input, true);
        assert_eq!(input.selection_range, 0..5);
    }

    /// 验证 Backspace 和 Delete 删除当前选区或邻近字符。
    #[test]
    fn 输入删除键会删除选区或邻近字符() {
        let mut input = make_input("a中b", 4..4);
        assert!(backspace_text_input(&mut input).changed);
        assert_eq!(input.text, "ab");

        let mut input = make_input("a中b", 1..1);
        assert!(delete_text_input(&mut input).changed);
        assert_eq!(input.text, "ab");
    }

    /// 验证鼠标单击、双击和三击选择规则。
    #[test]
    fn 输入鼠标选择支持单击双击和三击() {
        let mut input = make_input("foo bar", 0..0);
        let drag = begin_text_input_mouse_selection(&mut input, 4, 1, false);
        assert_eq!(input.selection_range, 4..4);
        assert_eq!(drag, Some(4));
        assert_eq!(input.selection_drag, Some(4));
        update_text_input_mouse_selection(&mut input, drag.unwrap(), 1);
        assert_eq!(input.selection_range, 1..4);
        assert_eq!(input.selection_drag, Some(4));

        begin_text_input_mouse_selection(&mut input, 1, 2, false);
        assert_eq!(input.selection_range, 0..3);
        assert!(input.selection_drag.is_none());

        begin_text_input_mouse_selection(&mut input, 1, 3, false);
        assert_eq!(input.selection_range, 0..7);
        assert!(input.selection_drag.is_none());
    }

    /// 验证长文本水平滚动会让光标保持可见。
    #[test]
    fn 输入水平滚动会跟随光标() {
        assert_eq!(
            text_input_horizontal_scroll_offset(0.0, px(180.0), px(240.0), px(100.0), true),
            89.5
        );
    }
}
