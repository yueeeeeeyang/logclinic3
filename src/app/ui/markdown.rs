// 应用内 Markdown 解析与渲染。
//
// 业务意图：
// - AI 对话和笔记阅读器都会展示 Markdown；解析、代码块高亮和 GPUI 渲染必须复用同一套规则，避免同一段内容在不同模块显示不一致。
// - 展示层只消费原始 Markdown 字符串并生成本地富文本结构，不改写物理笔记文件或 AI 对话存储，也不执行 HTML。
//
// 边界条件：
// - Markdown 解析失败、不完整流式片段或未知语法必须降级为可见文本，不能导致 UI 崩溃或丢失用户内容。
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

/// 应用 Markdown 渲染缓存条目。
///
/// 业务意图：
/// - 同一条历史消息在虚拟列表滚动、窗口重绘和主题未变时不需要重复解析 Markdown 和高亮代码块。
#[derive(Clone)]
pub(in crate::app) struct AppMarkdownCacheEntry {
    /// 正文哈希；内容变化时替换缓存。
    pub(in crate::app) content_hash: u64,
    /// 生成该缓存时的实际主题；主题变化时需要重新生成代码高亮颜色。
    pub(in crate::app) theme: EffectiveTheme,
    /// 生成该缓存时的状态版本；流式消息或编辑草稿可以用该字段强制刷新展示结构。
    pub(in crate::app) status: AiChatMessageStatus,
    /// 已解析好的 Markdown 展示文档。
    pub(in crate::app) document: AppMarkdownDocument,
}

/// 应用 Markdown 文档。
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct AppMarkdownDocument {
    /// 顶层块级元素。
    pub(in crate::app) blocks: Vec<AppMarkdownBlock>,
}

/// 应用 Markdown 块级元素。
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) enum AppMarkdownBlock {
    /// 普通段落。
    Paragraph(Vec<AppMarkdownInline>),
    /// 标题，等级范围为 1-6。
    Heading {
        /// Markdown 标题等级。
        level: u8,
        /// 标题内联内容。
        inlines: Vec<AppMarkdownInline>,
    },
    /// 有序或无序列表。
    List {
        /// 有序列表起始编号；无序列表为空。
        start: Option<u64>,
        /// 列表项。
        items: Vec<Vec<AppMarkdownBlock>>,
    },
    /// 引用块。
    BlockQuote(Vec<AppMarkdownBlock>),
    /// 代码块。
    CodeBlock {
        /// 代码块 info string 中的首个语言标识。
        language: Option<String>,
        /// 高亮后的逐行文本。
        lines: Vec<AppMarkdownCodeLine>,
    },
    /// 表格。
    Table {
        /// 表头单元格。
        headers: Vec<Vec<AppMarkdownInline>>,
        /// 表体行。
        rows: Vec<Vec<Vec<AppMarkdownInline>>>,
    },
    /// 分隔线。
    ThematicBreak,
}

/// 应用 Markdown 内联元素。
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) enum AppMarkdownInline {
    /// 普通文本。
    Text(String),
    /// 加粗文本。
    Strong(Vec<AppMarkdownInline>),
    /// 斜体文本。
    Emphasis(Vec<AppMarkdownInline>),
    /// 删除线文本。
    Strikethrough(Vec<AppMarkdownInline>),
    /// 行内代码。
    Code(String),
    /// 链接；首版只做视觉样式，不打开外部 URL。
    Link {
        /// 链接文本。
        label: Vec<AppMarkdownInline>,
        /// 链接目标，保留用于后续交互扩展。
        destination: String,
    },
}

/// 代码块中的一行高亮文本。
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct AppMarkdownCodeLine {
    /// 当前行文本，不包含换行符。
    pub(in crate::app) text: String,
    /// 当前行内的语法高亮范围。
    pub(in crate::app) highlights: Vec<(Range<usize>, HighlightStyle)>,
}

/// 展平内联元素时携带的样式标记。
#[derive(Clone, Copy, Default)]
struct AppMarkdownInlineStyle {
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
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = self.effective_theme();
        let document = self.ai_chat_markdown_document_for_message(message, theme);
        self.render_ai_chat_selectable_markdown_document(&document, &message.id, palette, context)
            .into_any_element()
    }

    /// 返回指定助手消息的 Markdown 解析结果。
    ///
    /// 业务意图：
    /// - 按消息 ID 缓存最近一次解析结果，SSE 流式更新时内容哈希变化会自然替换缓存。
    fn ai_chat_markdown_document_for_message(
        &self,
        message: &AiChatMessage,
        theme: EffectiveTheme,
    ) -> AppMarkdownDocument {
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
            AppMarkdownCacheEntry {
                content_hash,
                theme,
                status: message.status.clone(),
                document: document.clone(),
            },
        );
        document
    }

    /// 渲染支持拖选复制的 AI 消息 Markdown 文档。
    ///
    /// 业务意图：
    /// - AI 对话气泡需要保持 Markdown 展示，同时允许用户用鼠标选择其中一段文本再复制。
    /// - 通用 `render_app_markdown_document` 仍用于日志分析等只读展示场景，这里只给 AI 对话页面接入消息选区状态。
    fn render_ai_chat_selectable_markdown_document(
        &self,
        document: &AppMarkdownDocument,
        message_id: &str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        let mut children = Vec::new();
        for (index, block) in document.blocks.iter().enumerate() {
            let block_key = format!("{message_id}-{index}");
            children.push(self.render_ai_chat_selectable_markdown_block(
                block, index, &block_key, message_id, palette, context,
            ));
        }
        div()
            .mt_1()
            .w_full()
            .min_w_0()
            .text_sm()
            .line_height(px(21.0))
            .text_color(rgb(palette.text))
            .children(children)
    }

    /// 渲染可选择的 AI 消息文本段。
    ///
    /// 边界条件：
    /// - 空文本不注册鼠标选区，避免没有内容的 Markdown 块抢占点击事件。
    pub(in crate::app) fn render_ai_chat_selectable_text_segment(
        &self,
        segment: AiChatMessageTextSegmentKey,
        text: String,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        let selectable_text = text.clone();
        let selectable_segment = segment.clone();
        div()
            .w_full()
            .min_w_0()
            .child(AiChatSelectableTextElement {
                view: context.entity(),
                segment,
                text,
                highlights,
                palette,
            })
            .when(!selectable_text.is_empty(), |text_element| {
                text_element.on_mouse_down(
                    MouseButton::Left,
                    context.listener(move |view, event: &MouseDownEvent, window, context| {
                        view.start_ai_chat_message_text_selection(
                            selectable_segment.clone(),
                            selectable_text.clone(),
                            event,
                            window,
                            context,
                        );
                        context.stop_propagation();
                    }),
                )
            })
    }

    /// 渲染单个可选择 Markdown 块级元素。
    fn render_ai_chat_selectable_markdown_block(
        &self,
        block: &AppMarkdownBlock,
        index: usize,
        block_key: &str,
        message_id: &str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match block {
            AppMarkdownBlock::Paragraph(inlines) => {
                let (text, highlights) = flatten_ai_chat_markdown_inlines(inlines, palette);
                div()
                    .w_full()
                    .min_w_0()
                    .when(index > 0, |block| block.mt_2())
                    .whitespace_normal()
                    .child(self.render_ai_chat_selectable_text_segment(
                        AiChatMessageTextSegmentKey::new(message_id.to_string(), block_key),
                        text,
                        highlights,
                        palette,
                        context,
                    ))
                    .into_any_element()
            }
            AppMarkdownBlock::Heading { level, inlines } => self
                .render_ai_chat_selectable_markdown_heading(
                    *level, inlines, index, block_key, message_id, palette, context,
                )
                .into_any_element(),
            AppMarkdownBlock::List { start, items } => self
                .render_ai_chat_selectable_markdown_list(
                    *start, items, index, block_key, message_id, palette, context,
                )
                .into_any_element(),
            AppMarkdownBlock::BlockQuote(children) => div()
                .when(index > 0, |block| block.mt_2())
                .pl_3()
                .border_l_1()
                .border_color(rgb(palette.border))
                .text_color(rgb(palette.muted_text))
                .children(children.iter().enumerate().map(|(child_index, child)| {
                    let child_key = format!("{block_key}-quote-{child_index}");
                    self.render_ai_chat_selectable_markdown_block(
                        child,
                        child_index,
                        &child_key,
                        message_id,
                        palette,
                        context,
                    )
                }))
                .into_any_element(),
            AppMarkdownBlock::CodeBlock { language, lines } => self
                .render_ai_chat_selectable_markdown_code_block(
                    language.as_deref(),
                    lines,
                    index,
                    block_key,
                    message_id,
                    palette,
                    context,
                )
                .into_any_element(),
            AppMarkdownBlock::Table { headers, rows } => self
                .render_ai_chat_selectable_markdown_table(
                    headers, rows, index, block_key, message_id, palette, context,
                )
                .into_any_element(),
            AppMarkdownBlock::ThematicBreak => div()
                .when(index > 0, |block| block.mt_3())
                .mb_2()
                .h(px(1.0))
                .w_full()
                .bg(rgb(palette.border))
                .into_any_element(),
        }
    }
}

impl MainView {
    /// 渲染支持文本选择的 Markdown 标题。
    fn render_ai_chat_selectable_markdown_heading(
        &self,
        level: u8,
        inlines: &[AppMarkdownInline],
        index: usize,
        block_key: &str,
        message_id: &str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        let font_size = match level {
            1 => 18.0,
            2 => 16.0,
            3 => 15.0,
            _ => 14.0,
        };
        let (text, highlights) = flatten_ai_chat_markdown_inlines(inlines, palette);
        div()
            .when(index > 0, |block| block.mt_3())
            .text_size(px(font_size))
            .line_height(px(font_size + 6.0))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(palette.text))
            .child(self.render_ai_chat_selectable_text_segment(
                AiChatMessageTextSegmentKey::new(message_id.to_string(), block_key),
                text,
                highlights,
                palette,
                context,
            ))
    }

    /// 渲染支持文本选择的 Markdown 列表。
    fn render_ai_chat_selectable_markdown_list(
        &self,
        start: Option<u64>,
        items: &[Vec<AppMarkdownBlock>],
        index: usize,
        block_key: &str,
        message_id: &str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
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
                                let child_key =
                                    format!("{block_key}-item-{item_index}-{child_index}");
                                self.render_ai_chat_selectable_markdown_block(
                                    child,
                                    child_index,
                                    &child_key,
                                    message_id,
                                    palette,
                                    context,
                                )
                            })),
                    )
            }))
    }

    /// 渲染支持文本选择的 Markdown 代码块。
    fn render_ai_chat_selectable_markdown_code_block(
        &self,
        language: Option<&str>,
        lines: &[AppMarkdownCodeLine],
        index: usize,
        block_key: &str,
        message_id: &str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
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
                    .children(lines.iter().enumerate().map(|(line_index, line)| {
                        let text = if line.text.is_empty() {
                            " ".to_string()
                        } else {
                            line.text.clone()
                        };
                        div().whitespace_nowrap().child(
                            self.render_ai_chat_selectable_text_segment(
                                AiChatMessageTextSegmentKey::new(
                                    message_id.to_string(),
                                    format!("{block_key}-code-{line_index}"),
                                ),
                                text,
                                line.highlights.clone(),
                                palette,
                                context,
                            ),
                        )
                    })),
            )
    }

    /// 渲染支持文本选择的 Markdown 表格。
    fn render_ai_chat_selectable_markdown_table(
        &self,
        headers: &[Vec<AppMarkdownInline>],
        rows: &[Vec<Vec<AppMarkdownInline>>],
        index: usize,
        block_key: &str,
        message_id: &str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
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
            .children(headers.iter().next().map(|_| {
                self.render_ai_chat_selectable_markdown_table_row(
                    headers, true, block_key, message_id, 0, palette, context,
                )
            }))
            .children(rows.iter().enumerate().map(|(row_index, row)| {
                self.render_ai_chat_selectable_markdown_table_row(
                    row,
                    false,
                    block_key,
                    message_id,
                    row_index + 1,
                    palette,
                    context,
                )
            }))
    }

    /// 渲染支持文本选择的 Markdown 表格行。
    fn render_ai_chat_selectable_markdown_table_row(
        &self,
        cells: &[Vec<AppMarkdownInline>],
        header: bool,
        block_key: &str,
        message_id: &str,
        row_index: usize,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .min_w(px(360.0))
            .when(!header, |row| {
                row.border_t_1().border_color(rgb(palette.border))
            })
            .when(header, |row| row.bg(rgb(palette.panel)))
            .children(cells.iter().enumerate().map(|(cell_index, cell)| {
                let (text, highlights) = flatten_ai_chat_markdown_inlines(cell, palette);
                div()
                    .flex_1()
                    .min_w(px(120.0))
                    .px_2()
                    .py_1()
                    .text_color(rgb(palette.text))
                    .when(header, |cell| cell.font_weight(FontWeight::SEMIBOLD))
                    .child(self.render_ai_chat_selectable_text_segment(
                        AiChatMessageTextSegmentKey::new(
                            message_id.to_string(),
                            format!("{block_key}-table-{row_index}-{cell_index}"),
                        ),
                        text,
                        highlights,
                        palette,
                        context,
                    ))
            }))
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

/// 返回 AI 助手消息使用的 Markdown 扩展集合。
///
/// 业务意图：
/// - 只启用聊天回复常用的 GFM 展示能力，避免 `Options::all()` 打开 smart punctuation 后把 `--`、`---`、`...` 等命令或日志文本改写成不同字符。
/// - 表格、任务列表和删除线是模型回复中常见结构；其它扩展先保持关闭，减少展示层对原始回答文本的语义改写。
fn ai_chat_markdown_options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_GFM);
    options
}

/// 解析 AI 助手 Markdown 正文。
pub(in crate::app) fn parse_ai_chat_markdown(
    content: &str,
    theme: EffectiveTheme,
) -> AppMarkdownDocument {
    let mut parser = Parser::new_ext(content, ai_chat_markdown_options()).peekable();
    let blocks = parse_markdown_blocks_until(&mut parser, markdown_never_end, theme);
    if blocks.is_empty() && !content.is_empty() {
        AppMarkdownDocument {
            blocks: vec![AppMarkdownBlock::Paragraph(vec![AppMarkdownInline::Text(
                content.to_string(),
            )])],
        }
    } else {
        AppMarkdownDocument { blocks }
    }
}

/// 递归解析块级元素，直到遇到指定结束标签。
fn parse_markdown_blocks_until<'a, I>(
    parser: &mut std::iter::Peekable<I>,
    is_end: fn(&TagEnd) -> bool,
    theme: EffectiveTheme,
) -> Vec<AppMarkdownBlock>
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
                    blocks.push(AppMarkdownBlock::Paragraph(inlines));
                }
            }
            Event::Start(Tag::Heading { level, .. }) => {
                let inlines = parse_markdown_inlines_until(parser, markdown_is_heading_end);
                blocks.push(AppMarkdownBlock::Heading {
                    level: markdown_heading_level(level),
                    inlines,
                });
            }
            Event::Start(Tag::BlockQuote(_)) => {
                let children =
                    parse_markdown_blocks_until(parser, markdown_is_block_quote_end, theme);
                blocks.push(AppMarkdownBlock::BlockQuote(children));
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
            Event::Rule => blocks.push(AppMarkdownBlock::ThematicBreak),
            Event::Text(text)
            | Event::Code(text)
            | Event::Html(text)
            | Event::InlineHtml(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => {
                blocks.push(AppMarkdownBlock::Paragraph(vec![AppMarkdownInline::Text(
                    text.to_string(),
                )]));
            }
            Event::SoftBreak => {
                blocks.push(AppMarkdownBlock::Paragraph(vec![AppMarkdownInline::Text(
                    " ".to_string(),
                )]));
            }
            Event::HardBreak => {
                blocks.push(AppMarkdownBlock::Paragraph(vec![AppMarkdownInline::Text(
                    "\n".to_string(),
                )]));
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
) -> Vec<AppMarkdownInline>
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
                inlines.push(AppMarkdownInline::Text(text.to_string()));
            }
            Event::Code(code) => inlines.push(AppMarkdownInline::Code(code.to_string())),
            Event::SoftBreak => inlines.push(AppMarkdownInline::Text(" ".to_string())),
            Event::HardBreak => inlines.push(AppMarkdownInline::Text("\n".to_string())),
            Event::TaskListMarker(checked) => {
                inlines.push(AppMarkdownInline::Text(if checked {
                    "[x] ".to_string()
                } else {
                    "[ ] ".to_string()
                }));
            }
            Event::Start(Tag::Strong) => {
                let children = parse_markdown_inlines_until(parser, markdown_is_strong_end);
                inlines.push(AppMarkdownInline::Strong(children));
            }
            Event::Start(Tag::Emphasis) => {
                let children = parse_markdown_inlines_until(parser, markdown_is_emphasis_end);
                inlines.push(AppMarkdownInline::Emphasis(children));
            }
            Event::Start(Tag::Strikethrough) => {
                let children = parse_markdown_inlines_until(parser, markdown_is_strikethrough_end);
                inlines.push(AppMarkdownInline::Strikethrough(children));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let label = parse_markdown_inlines_until(parser, markdown_is_link_end);
                inlines.push(AppMarkdownInline::Link {
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
) -> AppMarkdownBlock
where
    I: Iterator<Item = Event<'a>>,
{
    let mut items = Vec::new();
    while let Some(event) = parser.next() {
        match event {
            Event::End(end) if matches!(end, TagEnd::List(_)) => break,
            Event::Start(Tag::Item) => {
                let item_blocks = parse_markdown_blocks_until(parser, markdown_is_item_end, theme);
                items.push(normalize_ai_chat_markdown_list_item_blocks(item_blocks));
            }
            _ => {}
        }
    }
    AppMarkdownBlock::List { start, items }
}

/// 规范化列表项内部块结构。
///
/// 业务意图：
/// - 大模型经常在列表项中按视觉宽度插入普通换行，pulldown-cmark 在部分懒延续场景下会把后续文本暴露成相邻段落块。
/// - 聊天窗口如果直接按段落渲染，会在同一个列表项中产生异常大空白；这里把相邻段落按空格合并，符合普通软换行的展示预期。
///
/// 边界条件：
/// - 代码块、表格、引用等非段落块不参与合并，避免破坏有明确结构的 Markdown 内容。
fn normalize_ai_chat_markdown_list_item_blocks(
    blocks: Vec<AppMarkdownBlock>,
) -> Vec<AppMarkdownBlock> {
    let mut normalized: Vec<AppMarkdownBlock> = Vec::new();
    for block in blocks {
        match (normalized.last_mut(), block) {
            (
                Some(AppMarkdownBlock::Paragraph(previous)),
                AppMarkdownBlock::Paragraph(mut current),
            ) => {
                trim_ai_chat_markdown_inlines_start(&mut current);
                if !previous.is_empty()
                    && !current.is_empty()
                    && !ai_chat_markdown_inlines_end_with_whitespace(previous)
                {
                    previous.push(AppMarkdownInline::Text(" ".to_string()));
                }
                previous.append(&mut current);
            }
            (_, block) => normalized.push(block),
        }
    }
    normalized
}

/// 去除段落开头的缩进空白。
///
/// 业务意图：
/// - 列表项延续行通常会带有 Markdown 缩进；合并到上一段时这些缩进只服务源码排版，不应成为聊天气泡里的多余空白。
fn trim_ai_chat_markdown_inlines_start(inlines: &mut Vec<AppMarkdownInline>) {
    while let Some(first) = inlines.first_mut() {
        match first {
            AppMarkdownInline::Text(value) | AppMarkdownInline::Code(value) => {
                let trimmed = value.trim_start().to_string();
                if trimmed.is_empty() {
                    inlines.remove(0);
                } else {
                    *value = trimmed;
                    break;
                }
            }
            AppMarkdownInline::Strong(children)
            | AppMarkdownInline::Emphasis(children)
            | AppMarkdownInline::Strikethrough(children) => {
                trim_ai_chat_markdown_inlines_start(children);
                if children.is_empty() {
                    inlines.remove(0);
                } else {
                    break;
                }
            }
            AppMarkdownInline::Link { label, .. } => {
                trim_ai_chat_markdown_inlines_start(label);
                if label.is_empty() {
                    inlines.remove(0);
                } else {
                    break;
                }
            }
        }
    }
}

/// 判断段落末尾是否已经带有空白。
///
/// 实现原因：
/// - 合并软换行段落时，如果上一段已因 `SoftBreak` 追加空格，再额外插入分隔空格会导致截图中的异常空白变宽。
fn ai_chat_markdown_inlines_end_with_whitespace(inlines: &[AppMarkdownInline]) -> bool {
    inlines
        .iter()
        .rev()
        .find_map(ai_chat_markdown_inline_end_with_whitespace)
        .unwrap_or(false)
}

/// 判断单个内联元素末尾是否为空白。
fn ai_chat_markdown_inline_end_with_whitespace(inline: &AppMarkdownInline) -> Option<bool> {
    match inline {
        AppMarkdownInline::Text(value) | AppMarkdownInline::Code(value) => {
            value.chars().next_back().map(char::is_whitespace)
        }
        AppMarkdownInline::Strong(children)
        | AppMarkdownInline::Emphasis(children)
        | AppMarkdownInline::Strikethrough(children) => children
            .iter()
            .rev()
            .find_map(ai_chat_markdown_inline_end_with_whitespace),
        AppMarkdownInline::Link { label, .. } => label
            .iter()
            .rev()
            .find_map(ai_chat_markdown_inline_end_with_whitespace),
    }
}

/// 解析 Markdown 代码块并执行语法高亮。
fn parse_markdown_code_block<'a, I>(
    parser: &mut std::iter::Peekable<I>,
    kind: CodeBlockKind<'a>,
    theme: EffectiveTheme,
) -> AppMarkdownBlock
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
    AppMarkdownBlock::CodeBlock {
        lines: highlight_app_code_block_with_language(&code, language.as_deref(), theme),
        language,
    }
}

/// 解析 Markdown 表格。
fn parse_markdown_table<'a, I>(parser: &mut std::iter::Peekable<I>) -> AppMarkdownBlock
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
    AppMarkdownBlock::Table { headers, rows }
}

/// 解析表格行内的单元格。
fn parse_markdown_table_cells_until<'a, I>(
    parser: &mut std::iter::Peekable<I>,
    is_end: fn(&TagEnd) -> bool,
) -> Vec<Vec<AppMarkdownInline>>
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
pub(in crate::app) fn render_app_markdown_document(
    document: &AppMarkdownDocument,
    message_id: &str,
    palette: AppThemePalette,
) -> gpui::Div {
    render_app_markdown_document_with_block_inserts(
        document,
        message_id,
        palette,
        |_index, _block| Vec::new(),
    )
}

/// 渲染完整 Markdown 文档，并允许调用方在块级元素后插入自定义元素。
///
/// 业务意图：
/// - 日志智能分析最终结论需要把本地原日志证据片段插入到引用证据 ID 的结论附近；
///   普通 Markdown 渲染不应该知道日志分析业务，因此通过回调扩展块级渲染。
pub(in crate::app) fn render_app_markdown_document_with_block_inserts<F>(
    document: &AppMarkdownDocument,
    message_id: &str,
    palette: AppThemePalette,
    mut insert_after_block: F,
) -> gpui::Div
where
    F: FnMut(usize, &AppMarkdownBlock) -> Vec<gpui::AnyElement>,
{
    let mut children = Vec::new();
    for (index, block) in document.blocks.iter().enumerate() {
        let block_key = format!("{message_id}-{index}");
        children.push(render_ai_chat_markdown_block(
            block, index, &block_key, palette,
        ));
        children.extend(insert_after_block(index, block));
    }

    div()
        .mt_1()
        .w_full()
        .min_w_0()
        .text_sm()
        .line_height(px(21.0))
        .text_color(rgb(palette.text))
        .children(children)
}

/// 渲染单个 Markdown 块级元素。
fn render_ai_chat_markdown_block(
    block: &AppMarkdownBlock,
    index: usize,
    block_key: &str,
    palette: AppThemePalette,
) -> gpui::AnyElement {
    match block {
        AppMarkdownBlock::Paragraph(inlines) => div()
            .w_full()
            .min_w_0()
            .when(index > 0, |block| block.mt_2())
            .whitespace_normal()
            .child(render_ai_chat_markdown_inlines(inlines, palette))
            .into_any_element(),
        AppMarkdownBlock::Heading { level, inlines } => {
            render_ai_chat_markdown_heading(*level, inlines, index, palette).into_any_element()
        }
        AppMarkdownBlock::List { start, items } => {
            render_ai_chat_markdown_list(*start, items, index, block_key, palette)
                .into_any_element()
        }
        AppMarkdownBlock::BlockQuote(children) => div()
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
        AppMarkdownBlock::CodeBlock { language, lines } => render_ai_chat_markdown_code_block(
            language.as_deref(),
            lines,
            index,
            block_key,
            palette,
        )
        .into_any_element(),
        AppMarkdownBlock::Table { headers, rows } => {
            render_ai_chat_markdown_table(headers, rows, index, block_key, palette)
                .into_any_element()
        }
        AppMarkdownBlock::ThematicBreak => div()
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
    inlines: &[AppMarkdownInline],
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
    items: &[Vec<AppMarkdownBlock>],
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
    lines: &[AppMarkdownCodeLine],
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
    headers: &[Vec<AppMarkdownInline>],
    rows: &[Vec<Vec<AppMarkdownInline>>],
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
    cells: &[Vec<AppMarkdownInline>],
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
    inlines: &[AppMarkdownInline],
    palette: AppThemePalette,
) -> StyledText {
    let (text, highlights) = flatten_ai_chat_markdown_inlines(inlines, palette);
    StyledText::new(text).with_highlights(highlights)
}

/// 将嵌套内联元素展平为 GPUI `StyledText` 可消费的文本和非重叠高亮范围。
pub(in crate::app) fn flatten_ai_chat_markdown_inlines(
    inlines: &[AppMarkdownInline],
    palette: AppThemePalette,
) -> (String, Vec<(Range<usize>, HighlightStyle)>) {
    let mut text = String::new();
    let mut highlights = Vec::new();
    push_ai_chat_markdown_inlines(
        inlines,
        AppMarkdownInlineStyle::default(),
        palette,
        &mut text,
        &mut highlights,
    );
    (text, highlights)
}

/// 递归展平内联元素。
fn push_ai_chat_markdown_inlines(
    inlines: &[AppMarkdownInline],
    style: AppMarkdownInlineStyle,
    palette: AppThemePalette,
    text: &mut String,
    highlights: &mut Vec<(Range<usize>, HighlightStyle)>,
) {
    for inline in inlines {
        match inline {
            AppMarkdownInline::Text(value) => {
                push_ai_chat_markdown_text(value, style, palette, text, highlights);
            }
            AppMarkdownInline::Code(value) => {
                push_ai_chat_markdown_text(
                    value,
                    AppMarkdownInlineStyle {
                        code: true,
                        ..style
                    },
                    palette,
                    text,
                    highlights,
                );
            }
            AppMarkdownInline::Strong(children) => {
                push_ai_chat_markdown_inlines(
                    children,
                    AppMarkdownInlineStyle {
                        strong: true,
                        ..style
                    },
                    palette,
                    text,
                    highlights,
                );
            }
            AppMarkdownInline::Emphasis(children) => {
                push_ai_chat_markdown_inlines(
                    children,
                    AppMarkdownInlineStyle {
                        emphasis: true,
                        ..style
                    },
                    palette,
                    text,
                    highlights,
                );
            }
            AppMarkdownInline::Strikethrough(children) => {
                push_ai_chat_markdown_inlines(
                    children,
                    AppMarkdownInlineStyle {
                        strikethrough: true,
                        ..style
                    },
                    palette,
                    text,
                    highlights,
                );
            }
            AppMarkdownInline::Link { label, .. } => {
                push_ai_chat_markdown_inlines(
                    label,
                    AppMarkdownInlineStyle {
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
    style: AppMarkdownInlineStyle,
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
    style: AppMarkdownInlineStyle,
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

/// 根据语言名称高亮应用内代码块。
///
/// 业务意图：
/// - AI 对话 Markdown 和笔记富文本代码块都需要语法高亮，复用同一套 syntect 适配可保证主题颜色和未知语言降级行为一致。
/// - 该函数不执行代码、不解析 HTML，只把纯文本按语言 token 转换为 GPUI 高亮范围。
pub(in crate::app) fn highlight_app_code_block_with_language(
    code: &str,
    language: Option<&str>,
    theme: EffectiveTheme,
) -> Vec<AppMarkdownCodeLine> {
    let syntax_set = ai_chat_syntax_set();
    let syntax = language
        .and_then(|language| syntax_set.find_syntax_by_token(language))
        .or_else(|| language.and_then(|language| syntax_set.find_syntax_by_extension(language)))
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
    let syntect_theme = ai_chat_syntect_theme(theme);
    let mut highlighter = HighlightLines::new(syntax, syntect_theme);
    if code.is_empty() {
        return vec![AppMarkdownCodeLine {
            text: String::new(),
            highlights: Vec::new(),
        }];
    }

    code.split('\n')
        .map(|raw_line| {
            let line_for_highlight = format!("{raw_line}\n");
            match highlighter.highlight_line(&line_for_highlight, syntax_set) {
                Ok(ranges) => ai_chat_code_line_from_syntect_ranges(raw_line, &ranges),
                Err(_) => AppMarkdownCodeLine {
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
) -> AppMarkdownCodeLine {
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
    AppMarkdownCodeLine { text, highlights }
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

/// 预热 Markdown 代码块高亮所需的 syntect 缓存。
///
/// 业务意图：
/// - AI 历史消息第一次渲染代码块时会触发 syntect 默认语法集和主题集加载，这个过程可能占用 1 秒左右。
/// - 该函数允许主窗口启动后在后台线程提前完成初始化，避免用户第一次进入 AI 对话页时把成本压到 UI 线程。
///
/// 边界条件：
/// - `OnceLock` 保证多次调用只会真实初始化一次；后台预热失败风险等同于正常渲染路径，不改变 Markdown 解析结果。
pub(in crate::app) fn prewarm_app_markdown_code_highlighting() {
    let _ = ai_chat_syntax_set();
    let _ = ai_chat_theme_set();
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
