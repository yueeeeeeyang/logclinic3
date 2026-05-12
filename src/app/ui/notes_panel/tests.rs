// 笔记 GPUI 面板纯逻辑测试。
//
// 业务意图：
// - 覆盖不需要真实窗口的笔记树、选区和编辑器辅助规则，避免后续调整 UI 时破坏基础行为。

use super::*;

/// 验证笔记只读选区会按 UTF-8 字符边界截取中文。
#[test]
fn 笔记源码选区按字符边界复制() {
    let selection = NoteTextSelection {
        anchor: NoteTextPosition {
            line_index: 0,
            column: 1,
        },
        focus: NoteTextPosition {
            line_index: 0,
            column: 3,
        },
    };
    let range = MainView::selected_byte_range_for_note_line(&selection, 0, "A中文B").unwrap();
    assert_eq!(&"A中文B"[range], "中文");
}

/// 验证双击选词不包含 Markdown 标点。
#[test]
fn 笔记源码双击选词只包含文字() {
    let selection = MainView::note_word_selection_for_position(
        0,
        "**标题**",
        NoteTextPosition {
            line_index: 0,
            column: 3,
        },
    )
    .unwrap();
    assert_eq!(
        selection,
        NoteTextSelection {
            anchor: NoteTextPosition {
                line_index: 0,
                column: 2,
            },
            focus: NoteTextPosition {
                line_index: 0,
                column: 4,
            },
        }
    );
}

/// 验证笔记编辑器上下方向键按字符列移动且保持 UTF-8 边界。
///
/// 业务意图：
/// - 笔记正文支持中文编辑，上下移动不能按字节列截断中文，也不能直接跳到全文首尾。
#[test]
fn 笔记编辑器上下移动保持字符列() {
    let text = "ABCD\n中文\nXYZ";
    let cursor = "ABCD\n中".len();

    assert_eq!(
        MainView::note_editor_vertical_target_index(text, cursor, -1),
        "A".len()
    );
    assert_eq!(
        MainView::note_editor_vertical_target_index(text, cursor, 1),
        "ABCD\n中文\nX".len()
    );
}

/// 验证笔记编辑器垂直移动到短行时会夹到目标行末尾。
///
/// 边界条件：
/// - 当前列超过目标行长度时，返回目标行末尾，且仍是合法 UTF-8 字节下标。
#[test]
fn 笔记编辑器上下移动会夹到短行末尾() {
    let text = "ABCD\n中\nXYZ";
    let cursor = "ABCD".len();

    assert_eq!(
        MainView::note_editor_vertical_target_index(text, cursor, 1),
        "ABCD\n中".len()
    );
}
