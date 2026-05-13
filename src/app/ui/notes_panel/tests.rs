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

/// 验证旧 Markdown 笔记进入编辑态后即使正文未变也会提示保存。
///
/// 业务意图：
/// - Markdown UI 已隐藏，旧数据按普通文本富文本化展示和编辑；保存后需要把格式字段转为 `RichText`，避免后续再次触发旧格式分支。
#[test]
fn markdown_笔记编辑后会按富文本保存() {
    let note = Note {
        id: "note-1".to_string(),
        directory_id: None,
        title: "旧笔记".to_string(),
        content: "# 标题".to_string(),
        content_format: NoteContentFormat::Markdown,
        created_at_ms: 1,
        updated_at_ms: 1,
    };

    assert!(NotesWorkspaceState::note_editor_has_unsaved_changes(
        true,
        "旧笔记",
        &NoteRichTextDocument::from_plain_text("# 标题")
            .to_json()
            .unwrap(),
        &note
    ));

    let rich_content = NoteRichTextDocument::from_plain_text("# 标题")
        .to_json()
        .unwrap();
    let rich_note = Note {
        content: rich_content.clone(),
        content_format: NoteContentFormat::RichText,
        ..note
    };
    assert!(!NotesWorkspaceState::note_editor_has_unsaved_changes(
        true,
        "旧笔记",
        &rich_content,
        &rich_note
    ));
}

/// 验证普通正文自动换行优先在英文空白处断行。
///
/// 业务意图：
/// - 长英文段落默认换行时应尽量保留单词完整性，避免阅读器把单词中间切开。
#[test]
fn 笔记正文自动换行优先英文空白() {
    let text = "hello world again";
    let end = preferred_soft_wrap_end(text, 0, "hello wor".len());
    assert_eq!(end, "hello ".len());
}

/// 验证中文自动换行只使用合法 UTF-8 字符边界。
///
/// 边界条件：
/// - GPUI 命中和富文本存储都使用 UTF-8 字节下标，换行断点不能落在中文字符内部。
#[test]
fn 笔记正文自动换行保持中文字符边界() {
    let text = "你好世界";
    let inside_character = "你好".len() + 1;
    let end = preferred_soft_wrap_end(text, 0, inside_character);
    assert_eq!(end, "你好".len());
    assert!(text.is_char_boundary(end));
}

/// 验证超长单词没有空白时会按字符边界强制换行。
#[test]
fn 笔记正文自动换行支持超长单词强制断行() {
    let text = "superlongword";
    let end = preferred_soft_wrap_end(text, 0, "super".len());
    assert_eq!(end, "super".len());
}
