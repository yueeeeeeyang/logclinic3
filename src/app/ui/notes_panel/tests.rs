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

/// 验证 Markdown 笔记编辑态按导出的 Markdown 判断未保存修改。
///
/// 业务意图：
/// - 物理文件存储下 Markdown 是真实正文格式；如果富文本导出的 Markdown 与文件原文一致，就不应提示保存。
#[test]
fn markdown_笔记编辑后按_markdown_内容判断未保存() {
    let note = Note {
        id: "note-1".to_string(),
        directory_id: None,
        title: "旧笔记".to_string(),
        content: "# 标题".to_string(),
        content_format: NoteContentFormat::Markdown,
        created_at_ms: 1,
        updated_at_ms: 1,
    };

    assert!(!NotesWorkspaceState::note_editor_has_unsaved_changes(
        true,
        "旧笔记",
        "# 标题",
        &note
    ));

    assert!(NotesWorkspaceState::note_editor_has_unsaved_changes(
        true,
        "旧笔记",
        "## 标题",
        &note
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

/// 验证笔记纸张保持接近 A4 的视觉比例。
///
/// 业务意图：
/// - 笔记编辑器和预览页需要给用户稳定的纸张版心，而不是随窗口宽度不断拉伸。
/// - 常量一旦被误改，会直接影响正文换行、截图中纸面比例和代码块菜单定位。
#[test]
fn 笔记_a4_纸张比例稳定() {
    let ratio = NOTES_A4_PAGE_HEIGHT / NOTES_A4_PAGE_WIDTH;
    assert!((ratio - (297.0 / 210.0)).abs() < 0.02);
    assert!(NOTES_A4_PAGE_WIDTH > 700.0);
    assert!(NOTES_A4_PAGE_HEIGHT > NOTES_A4_PAGE_WIDTH);
    assert!(NOTES_A4_PAGE_GUTTER >= 24.0);
}

/// 验证代码块视觉高度包含卡片头部、正文和留白。
///
/// 业务意图：
/// - 代码块改为带头部的卡片后，布局预估高度必须覆盖语言标签、代码行和横向滚动条预留空间。
/// - 如果高度不足，滚动容器会裁掉底部圆角或滚动条，预览和编辑态都会出现视觉压缩。
#[test]
fn 笔记代码块视觉高度包含卡片结构() {
    let height = code_block_visual_height(1);
    assert!(
        height
            > NOTES_CODE_BLOCK_VERTICAL_GAP
                + NOTES_CODE_BLOCK_HEADER_HEIGHT
                + NOTES_CODE_BLOCK_VERTICAL_PADDING
                + NOTES_CODE_BLOCK_LINE_HEIGHT
    );
}

/// 验证笔记 AI 请求上下文使用当前编辑器草稿。
///
/// 业务意图：
/// - 笔记 AI 发送前必须携带当前未保存标题和 Markdown 正文；该测试锁定请求构造，不允许未来改成重新读取磁盘旧内容。
#[test]
fn 笔记_ai_请求包含当前草稿标题和_markdown() {
    let document = NotesAiDocumentContext {
        title: "草稿标题".to_string(),
        markdown: "# 未保存正文".to_string(),
    };
    let messages = build_notes_ai_request_messages(&document, &[], "帮我扩写");

    assert_eq!(messages[0].role, "system");
    assert!(messages[1].content.contains("草稿标题"));
    assert!(messages[1].content.contains("# 未保存正文"));
    assert_eq!(messages.last().unwrap().role, "user");
    assert_eq!(messages.last().unwrap().content, "帮我扩写");
}

/// 验证笔记 AI 临时历史只发送可用消息。
///
/// 边界条件：
/// - 失败、正在生成和空正文消息不能再次进入请求上下文，否则模型会看到错误或半截回复。
#[test]
fn 笔记_ai_请求过滤失败和流式历史() {
    let document = NotesAiDocumentContext {
        title: "标题".to_string(),
        markdown: "正文".to_string(),
    };
    let history = vec![
        NotesAiMessage {
            id: "user-ok".to_string(),
            role: NotesAiMessageRole::User,
            content: "上一轮要求".to_string(),
            reasoning_content: String::new(),
            status: AiChatMessageStatus::Complete,
            error_message: None,
        },
        NotesAiMessage {
            id: "assistant-streaming".to_string(),
            role: NotesAiMessageRole::Assistant,
            content: "半截回复".to_string(),
            reasoning_content: String::new(),
            status: AiChatMessageStatus::Streaming,
            error_message: None,
        },
        NotesAiMessage {
            id: "assistant-failed".to_string(),
            role: NotesAiMessageRole::Assistant,
            content: "失败回复".to_string(),
            reasoning_content: String::new(),
            status: AiChatMessageStatus::Failed,
            error_message: Some("失败".to_string()),
        },
        NotesAiMessage {
            id: "assistant-ok".to_string(),
            role: NotesAiMessageRole::Assistant,
            content: "可用回复".to_string(),
            reasoning_content: String::new(),
            status: AiChatMessageStatus::Stopped,
            error_message: None,
        },
    ];

    let messages = build_notes_ai_request_messages(&document, &history, "继续");
    let joined = messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(joined.contains("上一轮要求"));
    assert!(joined.contains("可用回复"));
    assert!(!joined.contains("半截回复"));
    assert!(!joined.contains("失败回复"));
}

/// 验证 AI Markdown 回复可以转换为富文本片段。
///
/// 业务意图：
/// - 用户点击“插入”时会把 AI Markdown 回复转成编辑器富文本片段；基础 Markdown 语义必须能进入文档模型。
#[test]
fn 笔记_ai_markdown_回复可转换为富文本() {
    let document = NoteRichTextDocument::from_markdown("**重点**\n\n- 条目");
    let markdown = document.to_markdown();

    assert!(markdown.contains("**重点**"));
    assert!(markdown.contains("- 条目"));
}
