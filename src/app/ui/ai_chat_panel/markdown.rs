// AI 对话助手 Markdown 解析与渲染。
//
// 业务意图：
// - 模型回复通常包含标题、列表、引用、表格和代码块；这些内容如果按纯文本显示，会降低排查日志和复制代码时的可读性。
// - 该模块只服务 AI 对话助手消息展示，消息原文仍原样保存到 SQLite 并原样发送给模型，避免展示层影响对话语义。
//
// 边界条件：
// - Markdown 解析失败、不完整流式片段或未知语法必须降级为可见文本，不能导致 UI 崩溃或丢失助手内容。
// - 原始 HTML 不执行、不作为富文本注入，只按普通文本显示，避免引入本地客户端不需要的 HTML 安全边界。
// - 代码块语法高亮使用 syntect 的纯 Rust regex-fancy 路径，避免默认 onig C 依赖影响 macOS/Windows 打包。

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    ops::Range,
    sync::OnceLock,
};

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use syntect::{
    easy::HighlightLines,
    highlighting::{Color as SyntectColor, Style as SyntectStyle, Theme, ThemeSet},
    parsing::SyntaxSet,
};

use super::*;

/// AI 助手 Markdown 渲染缓存条目。
///
/// 业务意图：
/// - 同一条历史消息在虚拟列表滚动、窗口重绘和主题未变时不需要重复解析 Markdown 和高亮代码块。
#[derive(Clone)]
pub(in crate::app) struct AiChatMarkdownCacheEntry {
    /// 消息正文哈希；内容变化时替换缓存。
    pub(in crate::app) content_hash: u64,
    /// 生成该缓存时的实际主题；主题变化时需要重新生成代码高亮颜色。
    pub(in crate::app) theme: EffectiveTheme,
    /// 生成该缓存时的消息状态；状态变化时刷新缓存，保证终态切换不会沿用流式阶段的展示结构。
    pub(in crate::app) status: AiChatMessageStatus,
    /// 已解析好的 Markdown 展示文档。
    pub(in crate::app) document: AiChatMarkdownDocument,
}

/// AI 助手 Markdown 文档。
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct AiChatMarkdownDocument {
    /// 顶层块级元素。
    pub(in crate::app) blocks: Vec<AiChatMarkdownBlock>,
}

/// AI 助手 Markdown 块级元素。
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) enum AiChatMarkdownBlock {
    /// 普通段落。
    Paragraph(Vec<AiChatMarkdownInline>),
    /// 标题，等级范围为 1-6。
    Heading {
        /// Markdown 标题等级。
        level: u8,
        /// 标题内联内容。
        inlines: Vec<AiChatMarkdownInline>,
    },
    /// 有序或无序列表。
    List {
        /// 有序列表起始编号；无序列表为空。
        start: Option<u64>,
        /// 列表项。
        items: Vec<Vec<AiChatMarkdownBlock>>,
    },
    /// 引用块。
    BlockQuote(Vec<AiChatMarkdownBlock>),
    /// 代码块。
    CodeBlock {
        /// 代码块 info string 中的首个语言标识。
        language: Option<String>,
        /// 高亮后的逐行文本。
        lines: Vec<AiChatMarkdownCodeLine>,
    },
    /// 表格。
    Table {
        /// 表头单元格。
        headers: Vec<Vec<AiChatMarkdownInline>>,
        /// 表体行。
        rows: Vec<Vec<Vec<AiChatMarkdownInline>>>,
    },
    /// 分隔线。
    ThematicBreak,
}

/// AI 助手 Markdown 内联元素。
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) enum AiChatMarkdownInline {
    /// 普通文本。
    Text(String),
    /// 加粗文本。
    Strong(Vec<AiChatMarkdownInline>),
    /// 斜体文本。
    Emphasis(Vec<AiChatMarkdownInline>),
    /// 删除线文本。
    Strikethrough(Vec<AiChatMarkdownInline>),
    /// 行内代码。
    Code(String),
    /// 链接；首版只做视觉样式，不打开外部 URL。
    Link {
        /// 链接文本。
        label: Vec<AiChatMarkdownInline>,
        /// 链接目标，保留用于后续交互扩展。
        destination: String,
    },
}

/// 代码块中的一行高亮文本。
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct AiChatMarkdownCodeLine {
    /// 当前行文本，不包含换行符。
    pub(in crate::app) text: String,
    /// 当前行内的语法高亮范围。
    pub(in crate::app) highlights: Vec<(Range<usize>, HighlightStyle)>,
}

/// 展平内联元素时携带的样式标记。
#[derive(Clone, Copy, Default)]
struct AiChatMarkdownInlineStyle {
    /// 是否加粗。
    strong: bool,
    /// 是否斜体。
    emphasis: bool,
    /// 是否删除线。
    strikethrough: bool,
    /// 是否行内代码。
    code: bool,
    /// 是否链接文本。
    link: bool,
}

impl MainView {
    /// 渲染 AI 助手 Markdown 正文。
    pub(in crate::app) fn render_ai_chat_markdown_message(
        &self,
        message: &AiChatMessage,
        palette: AppThemePalette,
    ) -> gpui::AnyElement {
        let theme = self.effective_theme();
        let document = self.ai_chat_markdown_document_for_message(message, theme);
        render_ai_chat_markdown_document(&document, &message.id, palette).into_any_element()
    }

    /// 返回指定助手消息的 Markdown 解析结果。
    ///
    /// 业务意图：
    /// - 按消息 ID 缓存最近一次解析结果，SSE 流式更新时内容哈希变化会自然替换缓存。
    fn ai_chat_markdown_document_for_message(
        &self,
        message: &AiChatMessage,
        theme: EffectiveTheme,
    ) -> AiChatMarkdownDocument {
        let content_hash = ai_chat_markdown_content_hash(&message.content);
        if let Some(entry) = self.ai_chat.markdown_cache.borrow().get(&message.id)
            && entry.content_hash == content_hash
            && entry.theme == theme
            && entry.status == message.status
        {
            return entry.document.clone();
        }

        let document = parse_ai_chat_markdown(&message.content, theme);
        self.ai_chat.markdown_cache.borrow_mut().insert(
            message.id.clone(),
            AiChatMarkdownCacheEntry {
                content_hash,
                theme,
                status: message.status.clone(),
                document: document.clone(),
            },
        );
        document
    }
}

/// 计算 AI Markdown 缓存使用的正文哈希。
///
/// 边界条件：
/// - 哈希只用于当前进程内缓存命中，不持久化，因此使用标准库默认哈希器即可。
pub(in crate::app) fn ai_chat_markdown_content_hash(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

/// 解析 AI 助手 Markdown 正文。
pub(in crate::app) fn parse_ai_chat_markdown(
    content: &str,
    theme: EffectiveTheme,
) -> AiChatMarkdownDocument {
    let mut parser = Parser::new_ext(content, Options::all()).peekable();
    let blocks = parse_markdown_blocks_until(&mut parser, markdown_never_end, theme);
    if blocks.is_empty() && !content.is_empty() {
        AiChatMarkdownDocument {
            blocks: vec![AiChatMarkdownBlock::Paragraph(vec![
                AiChatMarkdownInline::Text(content.to_string()),
            ])],
        }
    } else {
        AiChatMarkdownDocument { blocks }
    }
}

/// 递归解析块级元素，直到遇到指定结束标签。
fn parse_markdown_blocks_until<'a, I>(
    parser: &mut std::iter::Peekable<I>,
    is_end: fn(&TagEnd) -> bool,
    theme: EffectiveTheme,
) -> Vec<AiChatMarkdownBlock>
where
    I: Iterator<Item = Event<'a>>,
{
    let mut blocks = Vec::new();
    while let Some(event) = parser.next() {
        match event {
            Event::End(end) if is_end(&end) => break,
            Event::Start(Tag::Paragraph) => {
                let inlines = parse_markdown_inlines_until(parser, markdown_is_paragraph_end);
                if !inlines.is_empty() {
                    blocks.push(AiChatMarkdownBlock::Paragraph(inlines));
                }
            }
            Event::Start(Tag::Heading { level, .. }) => {
                let inlines = parse_markdown_inlines_until(parser, markdown_is_heading_end);
                blocks.push(AiChatMarkdownBlock::Heading {
                    level: markdown_heading_level(level),
                    inlines,
                });
            }
            Event::Start(Tag::BlockQuote(_)) => {
                let children =
                    parse_markdown_blocks_until(parser, markdown_is_block_quote_end, theme);
                blocks.push(AiChatMarkdownBlock::BlockQuote(children));
            }
            Event::Start(Tag::List(start)) => {
                blocks.push(parse_markdown_list(parser, start, theme));
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                blocks.push(parse_markdown_code_block(parser, kind, theme));
            }
            Event::Start(Tag::Table(_)) => {
                blocks.push(parse_markdown_table(parser));
            }
            Event::Rule => blocks.push(AiChatMarkdownBlock::ThematicBreak),
            Event::Text(text)
            | Event::Code(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => {
                blocks.push(AiChatMarkdownBlock::Paragraph(vec![
                    AiChatMarkdownInline::Text(text.to_string()),
                ]));
            }
            Event::SoftBreak | Event::HardBreak => {
                blocks.push(AiChatMarkdownBlock::Paragraph(vec![
                    AiChatMarkdownInline::Text("\n".to_string()),
                ]));
            }
            _ => {}
        }
    }
    blocks
}

/// 解析 Markdown 内联元素。
fn parse_markdown_inlines_until<'a, I>(
    parser: &mut std::iter::Peekable<I>,
    is_end: fn(&TagEnd) -> bool,
) -> Vec<AiChatMarkdownInline>
where
    I: Iterator<Item = Event<'a>>,
{
    let mut inlines = Vec::new();
    while let Some(event) = parser.next() {
        match event {
            Event::End(end) if is_end(&end) => break,
            Event::Text(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => {
                inlines.push(AiChatMarkdownInline::Text(text.to_string()));
            }
            Event::Code(code) => inlines.push(AiChatMarkdownInline::Code(code.to_string())),
            Event::SoftBreak => inlines.push(AiChatMarkdownInline::Text("\n".to_string())),
            Event::HardBreak => inlines.push(AiChatMarkdownInline::Text("\n".to_string())),
            Event::TaskListMarker(checked) => {
                inlines.push(AiChatMarkdownInline::Text(if checked {
                    "[x] ".to_string()
                } else {
                    "[ ] ".to_string()
                }));
            }
            Event::Start(Tag::Strong) => {
                let children = parse_markdown_inlines_until(parser, markdown_is_strong_end);
                inlines.push(AiChatMarkdownInline::Strong(children));
            }
            Event::Start(Tag::Emphasis) => {
                let children = parse_markdown_inlines_until(parser, markdown_is_emphasis_end);
                inlines.push(AiChatMarkdownInline::Emphasis(children));
            }
            Event::Start(Tag::Strikethrough) => {
                let children = parse_markdown_inlines_until(parser, markdown_is_strikethrough_end);
                inlines.push(AiChatMarkdownInline::Strikethrough(children));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let label = parse_markdown_inlines_until(parser, markdown_is_link_end);
                inlines.push(AiChatMarkdownInline::Link {
                    label,
                    destination: dest_url.to_string(),
                });
            }
            Event::Start(Tag::Image { .. }) => {
                let label = parse_markdown_inlines_until(parser, markdown_is_image_end);
                inlines.extend(label);
            }
            _ => {}
        }
    }
    inlines
}

/// 解析 Markdown 列表。
fn parse_markdown_list<'a, I>(
    parser: &mut std::iter::Peekable<I>,
    start: Option<u64>,
    theme: EffectiveTheme,
) -> AiChatMarkdownBlock
where
    I: Iterator<Item = Event<'a>>,
{
    let mut items = Vec::new();
    while let Some(event) = parser.next() {
        match event {
            Event::End(end) if matches!(end, TagEnd::List(_)) => break,
            Event::Start(Tag::Item) => {
                items.push(parse_markdown_blocks_until(
                    parser,
                    markdown_is_item_end,
                    theme,
                ));
            }
            _ => {}
        }
    }
    AiChatMarkdownBlock::List { start, items }
}

/// 解析 Markdown 代码块并执行语法高亮。
fn parse_markdown_code_block<'a, I>(
    parser: &mut std::iter::Peekable<I>,
    kind: CodeBlockKind<'a>,
    theme: EffectiveTheme,
) -> AiChatMarkdownBlock
where
    I: Iterator<Item = Event<'a>>,
{
    let language = match kind {
        CodeBlockKind::Fenced(info) => info
            .split_whitespace()
            .next()
            .filter(|part| !part.is_empty())
            .map(ToString::to_string),
        CodeBlockKind::Indented => None,
    };
    let mut code = String::new();
    while let Some(event) = parser.next() {
        match event {
            Event::End(end) if matches!(end, TagEnd::CodeBlock) => break,
            Event::Text(text)
            | Event::Code(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => code.push_str(&text),
            Event::SoftBreak | Event::HardBreak => code.push('\n'),
            _ => {}
        }
    }
    AiChatMarkdownBlock::CodeBlock {
        lines: highlight_ai_chat_code_block_with_language(&code, language.as_deref(), theme),
        language,
    }
}

/// 解析 Markdown 表格。
fn parse_markdown_table<'a, I>(parser: &mut std::iter::Peekable<I>) -> AiChatMarkdownBlock
where
    I: Iterator<Item = Event<'a>>,
{
    let mut headers = Vec::new();
    let mut rows = Vec::new();
    while let Some(event) = parser.next() {
        match event {
            Event::End(end) if matches!(end, TagEnd::Table) => break,
            Event::Start(Tag::TableHead) => {
                headers = parse_markdown_table_cells_until(parser, markdown_is_table_head_end);
            }
            Event::Start(Tag::TableRow) => {
                rows.push(parse_markdown_table_cells_until(
                    parser,
                    markdown_is_table_row_end,
                ));
            }
            _ => {}
        }
    }
    AiChatMarkdownBlock::Table { headers, rows }
}

/// 解析表格行内的单元格。
fn parse_markdown_table_cells_until<'a, I>(
    parser: &mut std::iter::Peekable<I>,
    is_end: fn(&TagEnd) -> bool,
) -> Vec<Vec<AiChatMarkdownInline>>
where
    I: Iterator<Item = Event<'a>>,
{
    let mut cells = Vec::new();
    while let Some(event) = parser.next() {
        match event {
            Event::End(end) if is_end(&end) => break,
            Event::Start(Tag::TableCell) => {
                cells.push(parse_markdown_inlines_until(
                    parser,
                    markdown_is_table_cell_end,
                ));
            }
            _ => {}
        }
    }
    cells
}

/// 将 Markdown 标题等级转换为数字。
fn markdown_heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// 顶层解析使用的永不结束条件。
///
/// 实现原因：
/// - 递归解析函数使用函数指针而不是泛型闭包，避免多层嵌套 Markdown 在测试构建中触发泛型递归上限。
fn markdown_never_end(_: &TagEnd) -> bool {
    false
}

/// 判断是否到达段落结束标签。
fn markdown_is_paragraph_end(end: &TagEnd) -> bool {
    matches!(end, TagEnd::Paragraph)
}

/// 判断是否到达标题结束标签。
fn markdown_is_heading_end(end: &TagEnd) -> bool {
    matches!(end, TagEnd::Heading(_))
}

/// 判断是否到达引用块结束标签。
fn markdown_is_block_quote_end(end: &TagEnd) -> bool {
    matches!(end, TagEnd::BlockQuote(_))
}

/// 判断是否到达列表项结束标签。
fn markdown_is_item_end(end: &TagEnd) -> bool {
    matches!(end, TagEnd::Item)
}

/// 判断是否到达粗体结束标签。
fn markdown_is_strong_end(end: &TagEnd) -> bool {
    matches!(end, TagEnd::Strong)
}

/// 判断是否到达斜体结束标签。
fn markdown_is_emphasis_end(end: &TagEnd) -> bool {
    matches!(end, TagEnd::Emphasis)
}

/// 判断是否到达删除线结束标签。
fn markdown_is_strikethrough_end(end: &TagEnd) -> bool {
    matches!(end, TagEnd::Strikethrough)
}

/// 判断是否到达链接结束标签。
fn markdown_is_link_end(end: &TagEnd) -> bool {
    matches!(end, TagEnd::Link)
}

/// 判断是否到达图片结束标签。
fn markdown_is_image_end(end: &TagEnd) -> bool {
    matches!(end, TagEnd::Image)
}

/// 判断是否到达表头结束标签。
fn markdown_is_table_head_end(end: &TagEnd) -> bool {
    matches!(end, TagEnd::TableHead)
}

/// 判断是否到达表格行结束标签。
fn markdown_is_table_row_end(end: &TagEnd) -> bool {
    matches!(end, TagEnd::TableRow)
}

/// 判断是否到达表格单元格结束标签。
fn markdown_is_table_cell_end(end: &TagEnd) -> bool {
    matches!(end, TagEnd::TableCell)
}

/// 渲染完整 Markdown 文档。
fn render_ai_chat_markdown_document(
    document: &AiChatMarkdownDocument,
    message_id: &str,
    palette: AppThemePalette,
) -> gpui::Div {
    div()
        .mt_1()
        .w_full()
        .min_w_0()
        .text_sm()
        .line_height(px(21.0))
        .text_color(rgb(palette.text))
        .children(document.blocks.iter().enumerate().map(|(index, block)| {
            let block_key = format!("{message_id}-{index}");
            render_ai_chat_markdown_block(block, index, &block_key, palette)
        }))
}

/// 渲染单个 Markdown 块级元素。
fn render_ai_chat_markdown_block(
    block: &AiChatMarkdownBlock,
    index: usize,
    block_key: &str,
    palette: AppThemePalette,
) -> gpui::AnyElement {
    match block {
        AiChatMarkdownBlock::Paragraph(inlines) => div()
            .w_full()
            .min_w_0()
            .when(index > 0, |block| block.mt_2())
            .whitespace_normal()
            .child(render_ai_chat_markdown_inlines(inlines, palette))
            .into_any_element(),
        AiChatMarkdownBlock::Heading { level, inlines } => {
            render_ai_chat_markdown_heading(*level, inlines, index, palette).into_any_element()
        }
        AiChatMarkdownBlock::List { start, items } => {
            render_ai_chat_markdown_list(*start, items, index, block_key, palette)
                .into_any_element()
        }
        AiChatMarkdownBlock::BlockQuote(children) => div()
            .when(index > 0, |block| block.mt_2())
            .pl_3()
            .border_l_1()
            .border_color(rgb(palette.border))
            .text_color(rgb(palette.muted_text))
            .children(children.iter().enumerate().map(|(child_index, child)| {
                let child_key = format!("{block_key}-quote-{child_index}");
                render_ai_chat_markdown_block(child, child_index, &child_key, palette)
            }))
            .into_any_element(),
        AiChatMarkdownBlock::CodeBlock { language, lines } => render_ai_chat_markdown_code_block(
            language.as_deref(),
            lines,
            index,
            block_key,
            palette,
        )
        .into_any_element(),
        AiChatMarkdownBlock::Table { headers, rows } => {
            render_ai_chat_markdown_table(headers, rows, index, block_key, palette)
                .into_any_element()
        }
        AiChatMarkdownBlock::ThematicBreak => div()
            .when(index > 0, |block| block.mt_3())
            .mb_2()
            .h(px(1.0))
            .w_full()
            .bg(rgb(palette.border))
            .into_any_element(),
    }
}

/// 渲染 Markdown 标题。
fn render_ai_chat_markdown_heading(
    level: u8,
    inlines: &[AiChatMarkdownInline],
    index: usize,
    palette: AppThemePalette,
) -> gpui::Div {
    let font_size = match level {
        1 => 18.0,
        2 => 16.0,
        3 => 15.0,
        _ => 14.0,
    };
    div()
        .when(index > 0, |block| block.mt_3())
        .text_size(px(font_size))
        .line_height(px(font_size + 6.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(palette.text))
        .child(render_ai_chat_markdown_inlines(inlines, palette))
}

/// 渲染 Markdown 列表。
fn render_ai_chat_markdown_list(
    start: Option<u64>,
    items: &[Vec<AiChatMarkdownBlock>],
    index: usize,
    block_key: &str,
    palette: AppThemePalette,
) -> gpui::Div {
    div()
        .when(index > 0, |block| block.mt_2())
        .flex()
        .flex_col()
        .gap_1()
        .children(items.iter().enumerate().map(|(item_index, item)| {
            let marker = start
                .map(|start| format!("{}.", start + item_index as u64))
                .unwrap_or_else(|| "-".to_string());
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .w(px(24.0))
                        .text_right()
                        .text_color(rgb(palette.muted_text))
                        .child(marker),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .children(item.iter().enumerate().map(|(child_index, child)| {
                            let child_key = format!("{block_key}-item-{item_index}-{child_index}");
                            render_ai_chat_markdown_block(child, child_index, &child_key, palette)
                        })),
                )
        }))
}

/// 渲染 Markdown 代码块。
fn render_ai_chat_markdown_code_block(
    language: Option<&str>,
    lines: &[AiChatMarkdownCodeLine],
    index: usize,
    block_key: &str,
    palette: AppThemePalette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(format!(
            "ai-chat-markdown-code-{block_key}"
        )))
        .when(index > 0, |block| block.mt_2())
        .w_full()
        .min_w_0()
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.input))
        .overflow_hidden()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(rgb(palette.border))
                .text_xs()
                .text_color(rgb(palette.muted_text))
                .child(language.unwrap_or("text").to_string()),
        )
        .child(
            div()
                .id(SharedString::from(format!(
                    "ai-chat-markdown-code-scroll-{block_key}"
                )))
                .w_full()
                .min_w_0()
                .overflow_x_scroll()
                .px_3()
                .py_2()
                .font_family(LOG_VIEWER_FONT_FAMILY)
                .text_size(px(13.0))
                .line_height(px(19.0))
                .children(lines.iter().map(|line| {
                    div().whitespace_nowrap().child(
                        StyledText::new(if line.text.is_empty() {
                            " ".to_string()
                        } else {
                            line.text.clone()
                        })
                        .with_highlights(line.highlights.clone()),
                    )
                })),
        )
}

/// 渲染 Markdown 表格。
fn render_ai_chat_markdown_table(
    headers: &[Vec<AiChatMarkdownInline>],
    rows: &[Vec<Vec<AiChatMarkdownInline>>],
    index: usize,
    block_key: &str,
    palette: AppThemePalette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(format!(
            "ai-chat-markdown-table-{block_key}"
        )))
        .when(index > 0, |block| block.mt_2())
        .w_full()
        .min_w_0()
        .overflow_x_scroll()
        .border_1()
        .border_color(rgb(palette.border))
        .rounded(px(6.0))
        .children(
            headers
                .iter()
                .next()
                .map(|_| render_ai_chat_markdown_table_row(headers, true, palette)),
        )
        .children(
            rows.iter()
                .map(|row| render_ai_chat_markdown_table_row(row, false, palette)),
        )
}

/// 渲染 Markdown 表格行。
fn render_ai_chat_markdown_table_row(
    cells: &[Vec<AiChatMarkdownInline>],
    header: bool,
    palette: AppThemePalette,
) -> gpui::Div {
    div()
        .flex()
        .min_w(px(360.0))
        .when(!header, |row| {
            row.border_t_1().border_color(rgb(palette.border))
        })
        .when(header, |row| row.bg(rgb(palette.panel)))
        .children(cells.iter().map(|cell| {
            div()
                .flex_1()
                .min_w(px(120.0))
                .px_2()
                .py_1()
                .text_color(rgb(palette.text))
                .when(header, |cell| cell.font_weight(FontWeight::SEMIBOLD))
                .child(render_ai_chat_markdown_inlines(cell, palette))
        }))
}

/// 渲染内联 Markdown 内容。
fn render_ai_chat_markdown_inlines(
    inlines: &[AiChatMarkdownInline],
    palette: AppThemePalette,
) -> StyledText {
    let (text, highlights) = flatten_ai_chat_markdown_inlines(inlines, palette);
    StyledText::new(text).with_highlights(highlights)
}

/// 将嵌套内联元素展平为 GPUI `StyledText` 可消费的文本和非重叠高亮范围。
pub(in crate::app) fn flatten_ai_chat_markdown_inlines(
    inlines: &[AiChatMarkdownInline],
    palette: AppThemePalette,
) -> (String, Vec<(Range<usize>, HighlightStyle)>) {
    let mut text = String::new();
    let mut highlights = Vec::new();
    push_ai_chat_markdown_inlines(
        inlines,
        AiChatMarkdownInlineStyle::default(),
        palette,
        &mut text,
        &mut highlights,
    );
    (text, highlights)
}

/// 递归展平内联元素。
fn push_ai_chat_markdown_inlines(
    inlines: &[AiChatMarkdownInline],
    style: AiChatMarkdownInlineStyle,
    palette: AppThemePalette,
    text: &mut String,
    highlights: &mut Vec<(Range<usize>, HighlightStyle)>,
) {
    for inline in inlines {
        match inline {
            AiChatMarkdownInline::Text(value) => {
                push_ai_chat_markdown_text(value, style, palette, text, highlights);
            }
            AiChatMarkdownInline::Code(value) => {
                push_ai_chat_markdown_text(
                    value,
                    AiChatMarkdownInlineStyle {
                        code: true,
                        ..style
                    },
                    palette,
                    text,
                    highlights,
                );
            }
            AiChatMarkdownInline::Strong(children) => {
                push_ai_chat_markdown_inlines(
                    children,
                    AiChatMarkdownInlineStyle {
                        strong: true,
                        ..style
                    },
                    palette,
                    text,
                    highlights,
                );
            }
            AiChatMarkdownInline::Emphasis(children) => {
                push_ai_chat_markdown_inlines(
                    children,
                    AiChatMarkdownInlineStyle {
                        emphasis: true,
                        ..style
                    },
                    palette,
                    text,
                    highlights,
                );
            }
            AiChatMarkdownInline::Strikethrough(children) => {
                push_ai_chat_markdown_inlines(
                    children,
                    AiChatMarkdownInlineStyle {
                        strikethrough: true,
                        ..style
                    },
                    palette,
                    text,
                    highlights,
                );
            }
            AiChatMarkdownInline::Link { label, .. } => {
                push_ai_chat_markdown_inlines(
                    label,
                    AiChatMarkdownInlineStyle {
                        link: true,
                        ..style
                    },
                    palette,
                    text,
                    highlights,
                );
            }
        }
    }
}

/// 追加叶子文本并生成对应样式范围。
fn push_ai_chat_markdown_text(
    value: &str,
    style: AiChatMarkdownInlineStyle,
    palette: AppThemePalette,
    text: &mut String,
    highlights: &mut Vec<(Range<usize>, HighlightStyle)>,
) {
    if value.is_empty() {
        return;
    }
    let start = text.len();
    text.push_str(value);
    let end = text.len();
    if let Some(highlight) = ai_chat_markdown_highlight_style(style, palette) {
        highlights.push((start..end, highlight));
    }
}

/// 将内联样式标记转换为 GPUI 高亮样式。
fn ai_chat_markdown_highlight_style(
    style: AiChatMarkdownInlineStyle,
    palette: AppThemePalette,
) -> Option<HighlightStyle> {
    if !style.strong && !style.emphasis && !style.strikethrough && !style.code && !style.link {
        return None;
    }

    let mut highlight = HighlightStyle::default();
    if style.strong {
        highlight.font_weight = Some(FontWeight::BOLD);
    }
    if style.emphasis {
        highlight.font_style = Some(FontStyle::Italic);
    }
    if style.strikethrough {
        highlight.strikethrough = Some(StrikethroughStyle {
            thickness: px(1.0),
            color: Some(rgb(palette.muted_text).into()),
        });
    }
    if style.code {
        highlight.color = Some(rgb(palette.accent).into());
        highlight.background_color = Some(rgb(palette.input).into());
    }
    if style.link {
        highlight.color = Some(rgb(palette.accent).into());
        highlight.underline = Some(UnderlineStyle {
            color: Some(rgb(palette.accent).into()),
            thickness: px(1.0),
            wavy: false,
        });
    }
    Some(highlight)
}

/// 根据语言名称高亮代码块。
fn highlight_ai_chat_code_block_with_language(
    code: &str,
    language: Option<&str>,
    theme: EffectiveTheme,
) -> Vec<AiChatMarkdownCodeLine> {
    let syntax_set = ai_chat_syntax_set();
    let syntax = language
        .and_then(|language| syntax_set.find_syntax_by_token(language))
        .or_else(|| language.and_then(|language| syntax_set.find_syntax_by_extension(language)))
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
    let syntect_theme = ai_chat_syntect_theme(theme);
    let mut highlighter = HighlightLines::new(syntax, syntect_theme);
    if code.is_empty() {
        return vec![AiChatMarkdownCodeLine {
            text: String::new(),
            highlights: Vec::new(),
        }];
    }

    code.split('\n')
        .map(|raw_line| {
            let line_for_highlight = format!("{raw_line}\n");
            match highlighter.highlight_line(&line_for_highlight, syntax_set) {
                Ok(ranges) => ai_chat_code_line_from_syntect_ranges(raw_line, &ranges),
                Err(_) => AiChatMarkdownCodeLine {
                    text: raw_line.to_string(),
                    highlights: Vec::new(),
                },
            }
        })
        .collect()
}

/// 把 syntect 高亮结果转换为 GPUI 单行高亮。
fn ai_chat_code_line_from_syntect_ranges(
    raw_line: &str,
    ranges: &[(SyntectStyle, &str)],
) -> AiChatMarkdownCodeLine {
    let mut text = String::new();
    let mut highlights = Vec::new();
    for (style, segment) in ranges {
        let display_segment = segment.trim_end_matches(['\r', '\n']);
        if display_segment.is_empty() {
            continue;
        }
        let start = text.len();
        text.push_str(display_segment);
        let end = text.len();
        highlights.push((
            start..end,
            HighlightStyle {
                color: Some(ai_chat_syntect_color(style.foreground)),
                ..HighlightStyle::default()
            },
        ));
    }
    if text.is_empty() && !raw_line.is_empty() {
        text.push_str(raw_line);
    }
    AiChatMarkdownCodeLine { text, highlights }
}

/// 返回 syntect 语法集合。
fn ai_chat_syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// 返回 syntect 主题集合。
fn ai_chat_theme_set() -> &'static ThemeSet {
    static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();
    THEME_SET.get_or_init(ThemeSet::load_defaults)
}

/// 根据应用主题选择 syntect 主题。
fn ai_chat_syntect_theme(theme: EffectiveTheme) -> &'static Theme {
    let theme_set = ai_chat_theme_set();
    let preferred = match theme {
        EffectiveTheme::Light => "base16-ocean.light",
        EffectiveTheme::Dark => "base16-ocean.dark",
    };
    theme_set
        .themes
        .get(preferred)
        .or_else(|| theme_set.themes.values().next())
        .expect("syntect 默认主题集合不能为空")
}

/// 转换 syntect RGBA 颜色为 GPUI HSLA。
fn ai_chat_syntect_color(color: SyntectColor) -> gpui::Hsla {
    rgba(
        ((color.r as u32) << 24)
            | ((color.g as u32) << 16)
            | ((color.b as u32) << 8)
            | color.a as u32,
    )
    .into()
}
