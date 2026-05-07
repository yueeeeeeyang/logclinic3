//! 日志正文高亮识别与高亮范围生成模块。
//!
//! 业务意图：
//! - 右侧日志查看器使用 GPUI `StyledText` 按行渲染，因此高亮模块需要输出每一行内部的字节范围和样式。
//! - `.log`、Java 线程 dump 和 Java 异常堆栈采用轻量按行规则，避免 200MB 日志打开时为全部行预计算高亮。
//! - `.xml` 和 `.properties` 在小文件内使用 Tree-sitter 官方语法和高亮查询，保证配置文件阅读体验更接近代码编辑器。
//!
//! 关键约束：
//! - Tree-sitter 整文件解析只允许在明确阈值内执行，避免大文件打开或编码切换阻塞后台任务过久。
//! - 所有高亮范围都必须落在 UTF-8 字符边界上；本模块只使用 Tree-sitter 节点边界和 ASCII 关键字匹配来保证安全。
//! - 高亮只影响视觉展示，不改变原始字节、编码选择、tab 去重或滚动状态。

use std::ops::Range;

use gpui::{FontWeight, HighlightStyle, rgb};
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

/// Tree-sitter 语法高亮允许处理的最大原始字节数。
///
/// 业务意图：
/// - XML/properties 配置文件通常远小于日志文件；超过 10MB 时优先保证打开速度和滚动稳定。
/// - 超过阈值时仍可以用轻量日志/线程规则展示，不让用户因为高亮功能打不开文件。
pub const TREE_SITTER_HIGHLIGHT_MAX_BYTES: usize = 10 * 1024 * 1024;

/// Tree-sitter 语法高亮允许处理的最大行数。
///
/// 边界条件：
/// - 行数很多但文件字节数不大时，预计算每行高亮 Vec 也会产生额外内存和初始化成本，因此需要单独限制。
pub const TREE_SITTER_HIGHLIGHT_MAX_LINES: usize = 100_000;

/// 语法高亮实际使用的主题。
///
/// 业务意图：
/// - 应用主题支持明亮和暗色背景，语法高亮颜色必须跟随实际背景，否则暗色优化会反过来破坏明亮主题可读性。
/// - 类型放在高亮模块内，避免 UI 调色板细节泄漏到日志解码和搜索模块。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxTheme {
    /// 适用于白色或浅色日志正文背景。
    Light,
    /// 适用于深色日志正文背景。
    Dark,
}

/// 单套语法高亮调色板。
///
/// 业务意图：
/// - 高亮规则负责“哪些片段需要强调”，调色板负责“在当前背景上如何显示”，两者分开后可避免主题切换时复制规则。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SyntaxPalette {
    /// 普通语法文本颜色，例如 XML 文本节点。
    text: u32,
    /// 弱化但仍可读的文本颜色，例如 Java 包名前缀、括号和分隔符。
    muted: u32,
    /// 注释颜色。
    comment: u32,
    /// 蓝色强调，用于属性名、方法名和线程名等可定位标识。
    blue: u32,
    /// 绿色强调，用于字符串、配置值和源码文件名。
    green: u32,
    /// 紫色强调，用于 XML 标签、类型名和类名。
    purple: u32,
    /// 橙色强调，用于时间戳、行号和数字。
    orange: u32,
    /// 黄色强调，用于等待状态和警告类信号。
    yellow: u32,
    /// 红色强调，用于错误、异常和阻塞状态。
    red: u32,
}

impl SyntaxPalette {
    /// 返回指定主题的语法高亮调色板。
    fn for_theme(theme: SyntaxTheme) -> Self {
        match theme {
            SyntaxTheme::Light => Self {
                text: 0x24292f,
                muted: 0x57606a,
                comment: 0x6a737d,
                blue: 0x0969da,
                green: 0x116329,
                purple: 0x8250df,
                orange: 0x953800,
                yellow: 0x9a6700,
                red: 0xcf222e,
            },
            SyntaxTheme::Dark => Self {
                text: 0xc9d1d9,
                muted: 0xb8c2cc,
                comment: 0x8b949e,
                blue: 0x58a6ff,
                green: 0x56d364,
                purple: 0xd2a8ff,
                orange: 0xffab70,
                yellow: 0xe3b341,
                red: 0xff7b72,
            },
        }
    }
}

/// 当前右侧查看器支持的高亮模式。
///
/// 业务意图：
/// - 模式识别结果随解码文档缓存，渲染可见行时不用重复根据文件名或内容推断。
/// - `Plain` 保留给后续无法识别或需要关闭高亮的场景，当前默认未知文本仍走 `Log`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightMode {
    /// 普通日志模式，按时间戳、等级、线程名和 logger/class 名做轻量高亮。
    Log,
    /// Java 线程 dump 或 Java 异常堆栈模式。
    JavaThread,
    /// Java properties 配置文件模式。
    Properties,
    /// XML 配置文件模式。
    Xml,
    /// 纯文本模式，不做高亮。
    Plain,
}

/// 单行内可直接交给 GPUI `StyledText` 的高亮列表。
///
/// 边界条件：
/// - range 使用当前行字符串的 UTF-8 字节偏移，不是字符下标。
/// - 同一行内高亮会在输出前排序并去除重叠，避免 GPUI 样式范围冲突。
pub type LineHighlights = Vec<(Range<usize>, HighlightStyle)>;

/// Tree-sitter 预计算后的每行高亮。
///
/// 业务意图：
/// - XML/properties 的高亮范围来自整文件语法树，不能只看单行文本独立判断。
/// - 预计算结果按行号索引，虚拟列表渲染可见行时只做 O(1) 克隆。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrecomputedHighlights {
    /// 与 `DecodedLogDocument::lines` 对齐的每行高亮范围。
    pub lines: Vec<LineHighlights>,
}

/// 解码完成后生成的高亮计划。
///
/// 业务意图：
/// - 文档需要同时知道最终使用的模式，以及该模式是否有 Tree-sitter 预计算结果。
/// - 大文件 XML/properties 会降级为轻量模式，因此最终 `mode` 可能不同于扩展名初步识别结果。
#[derive(Clone, Debug, PartialEq)]
pub struct HighlightPlan {
    /// 最终用于渲染的高亮模式。
    pub mode: HighlightMode,
    /// 仅 XML/properties 小文件会填充的预计算高亮。
    pub precomputed: Option<PrecomputedHighlights>,
}

/// 根据来源名、内容和大小准备最终高亮方案。
///
/// 业务意图：
/// - 解码阶段已经拿到完整文本和行列表，此时一次性完成模式识别和必要的 Tree-sitter 解析最省成本。
/// - 渲染阶段只消费结果，不再触发文件级扫描或解析。
///
/// 边界条件：
/// - Tree-sitter 解析失败不会让日志打开失败，而是降级为轻量日志/线程高亮。
/// - 大文件 XML/properties 也会降级，避免用户因配置文件过大而感知卡顿。
pub fn prepare_highlighting(
    source_name: &str,
    text: &str,
    lines: &[String],
    raw_size: usize,
    theme: SyntaxTheme,
) -> HighlightPlan {
    let detected_mode = detect_highlight_mode(source_name, lines);

    match detected_mode {
        HighlightMode::Xml | HighlightMode::Properties
            if should_precompute_tree_sitter_highlights(raw_size, lines.len()) =>
        {
            if let Some(precomputed) =
                build_precomputed_highlights(detected_mode, text, lines, theme)
            {
                return HighlightPlan {
                    mode: detected_mode,
                    precomputed: Some(precomputed),
                };
            }

            HighlightPlan {
                mode: fallback_mode_for_config_text(lines),
                precomputed: None,
            }
        }
        HighlightMode::Xml | HighlightMode::Properties => HighlightPlan {
            mode: fallback_mode_for_config_text(lines),
            precomputed: None,
        },
        HighlightMode::Log | HighlightMode::JavaThread | HighlightMode::Plain => HighlightPlan {
            mode: detected_mode,
            precomputed: None,
        },
    }
}

/// 自动识别高亮模式。
///
/// 业务意图：
/// - 文件名扩展名是最稳定的业务信号，优先识别 `.xml`、`.properties` 和 `.log`。
/// - 对压缩包内无扩展名或名称不可靠的成员，再根据内容做启发式识别。
pub fn detect_highlight_mode(source_name: &str, lines: &[String]) -> HighlightMode {
    // 空文件没有可识别的语法结构，使用纯文本模式可以避免后续规则在空行上产生无意义高亮。
    if lines.iter().all(|line| line.trim().is_empty()) {
        return HighlightMode::Plain;
    }

    let normalized_name = source_name.to_ascii_lowercase();
    if normalized_name.ends_with(".xml") {
        return HighlightMode::Xml;
    }
    if normalized_name.ends_with(".properties") {
        return HighlightMode::Properties;
    }
    if looks_like_java_thread_or_exception(lines) {
        return HighlightMode::JavaThread;
    }
    if normalized_name.ends_with(".log") {
        return HighlightMode::Log;
    }
    if looks_like_xml(lines) {
        return HighlightMode::Xml;
    }
    if looks_like_properties(lines) {
        return HighlightMode::Properties;
    }

    HighlightMode::Log
}

/// 生成单行高亮范围。
///
/// 业务意图：
/// - GPUI 虚拟列表只渲染可见行，因此 `.log` 和 Java 线程日志按当前行即时计算即可。
/// - XML/properties 如果已有预计算结果，直接复用语法树结果；否则按降级后的轻量模式处理。
pub fn highlight_line(
    mode: HighlightMode,
    line: &str,
    precomputed: Option<&LineHighlights>,
    theme: SyntaxTheme,
) -> LineHighlights {
    if let Some(highlights) = precomputed {
        return highlights.clone();
    }

    let mut highlights = match mode {
        HighlightMode::Log => log_line_highlights(line, theme),
        HighlightMode::JavaThread => java_thread_line_highlights(line, theme),
        HighlightMode::Properties => properties_line_fallback_highlights(line, theme),
        HighlightMode::Xml => xml_line_fallback_highlights(line, theme),
        HighlightMode::Plain => Vec::new(),
    };
    normalize_line_highlights(&mut highlights);
    highlights
}

/// 判断是否允许对当前文档执行 Tree-sitter 整文件高亮。
fn should_precompute_tree_sitter_highlights(raw_size: usize, line_count: usize) -> bool {
    raw_size <= TREE_SITTER_HIGHLIGHT_MAX_BYTES && line_count <= TREE_SITTER_HIGHLIGHT_MAX_LINES
}

/// 为 XML/properties 构建 Tree-sitter 预计算高亮。
///
/// 边界条件：
/// - Tree-sitter 返回的是整篇文本的字节范围，本函数会拆分到对应 UI 行。
/// - 解析或查询失败返回 `None`，调用方负责降级，不把高亮失败升级为打开失败。
pub fn build_precomputed_highlights(
    mode: HighlightMode,
    text: &str,
    lines: &[String],
    theme: SyntaxTheme,
) -> Option<PrecomputedHighlights> {
    let (language, query_source) = match mode {
        HighlightMode::Properties => (
            tree_sitter_properties::LANGUAGE.into(),
            tree_sitter_properties::HIGHLIGHTS_QUERY,
        ),
        HighlightMode::Xml => (
            tree_sitter_xml::LANGUAGE_XML.into(),
            tree_sitter_xml::XML_HIGHLIGHT_QUERY,
        ),
        HighlightMode::Log | HighlightMode::JavaThread | HighlightMode::Plain => return None,
    };

    tree_sitter_highlights_for_text(language, query_source, text, lines, theme)
}

/// 使用指定 Tree-sitter 语言和查询生成每行高亮。
fn tree_sitter_highlights_for_text(
    language: Language,
    query_source: &str,
    text: &str,
    lines: &[String],
    theme: SyntaxTheme,
) -> Option<PrecomputedHighlights> {
    let mut parser = Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(text, None)?;
    let query = Query::new(&language, query_source).ok()?;
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), text.as_bytes());
    let capture_names = query.capture_names();
    let line_ranges = line_byte_ranges_for_text(text);
    let mut line_highlights = vec![Vec::new(); lines.len()];

    while {
        captures.advance();
        captures.get().is_some()
    } {
        let Some((query_match, capture_index)) = captures.get() else {
            break;
        };
        let capture = query_match.captures[*capture_index];
        let capture_name = capture_names
            .get(capture.index as usize)
            .copied()
            .unwrap_or_default();
        let Some(style) = style_for_tree_sitter_capture(capture_name, theme) else {
            continue;
        };
        push_global_highlight(
            &mut line_highlights,
            &line_ranges,
            capture.node.start_byte()..capture.node.end_byte(),
            style,
        );
    }

    for highlights in &mut line_highlights {
        normalize_line_highlights(highlights);
    }

    Some(PrecomputedHighlights {
        lines: line_highlights,
    })
}

/// 将整篇文本字节范围拆成与 UI 行列表对齐的内容范围。
///
/// 边界条件：
/// - `split_decoded_lines` 会去掉 `\n` 和行尾 `\r`，这里也必须排除这些字节，保证局部 range 能直接用于行字符串。
/// - 文本以换行结尾时会产生最后一行空行，范围为文本末尾的空范围。
fn line_byte_ranges_for_text(text: &str) -> Vec<Range<usize>> {
    if text.is_empty() {
        return std::iter::once(0..0).collect();
    }

    let mut ranges = Vec::new();
    let mut line_start = 0usize;
    for line in text.split('\n') {
        let raw_line_end = line_start + line.len();
        let content_end = if line.ends_with('\r') {
            raw_line_end.saturating_sub(1)
        } else {
            raw_line_end
        };
        ranges.push(line_start..content_end);
        line_start = raw_line_end + 1;
    }
    ranges
}

/// 把 Tree-sitter 的整篇范围拆分并写入对应行。
fn push_global_highlight(
    line_highlights: &mut [LineHighlights],
    line_ranges: &[Range<usize>],
    global_range: Range<usize>,
    style: HighlightStyle,
) {
    if global_range.start >= global_range.end {
        return;
    }

    let mut line_index = first_line_overlapping(line_ranges, global_range.start);
    while let Some(line_range) = line_ranges.get(line_index) {
        if line_range.start >= global_range.end {
            break;
        }

        let overlap_start = global_range.start.max(line_range.start);
        let overlap_end = global_range.end.min(line_range.end);
        if overlap_start < overlap_end {
            line_highlights[line_index].push((
                overlap_start - line_range.start..overlap_end - line_range.start,
                style,
            ));
        }

        line_index += 1;
    }
}

/// 找到第一个可能和指定字节起点重叠的行。
fn first_line_overlapping(line_ranges: &[Range<usize>], start: usize) -> usize {
    let mut low = 0usize;
    let mut high = line_ranges.len();

    while low < high {
        let middle = (low + high) / 2;
        if line_ranges[middle].end <= start {
            low = middle + 1;
        } else {
            high = middle;
        }
    }

    low
}

/// 将 Tree-sitter capture 名称映射到暗色主题也可读的通用样式。
fn style_for_tree_sitter_capture(capture_name: &str, theme: SyntaxTheme) -> Option<HighlightStyle> {
    let palette = SyntaxPalette::for_theme(theme);
    let style = if capture_name.starts_with("comment") {
        syntax_style(palette.comment, None, None)
    } else if capture_name.starts_with("tag") || capture_name.starts_with("type") {
        syntax_style(palette.purple, None, Some(FontWeight::BOLD))
    } else if capture_name.starts_with("property") || capture_name.starts_with("attribute") {
        syntax_style(palette.blue, None, None)
    } else if capture_name.starts_with("string") {
        syntax_style(palette.green, None, None)
    } else if capture_name.starts_with("number") || capture_name.starts_with("boolean") {
        syntax_style(palette.orange, None, None)
    } else if capture_name.starts_with("keyword") {
        syntax_style(palette.purple, None, Some(FontWeight::BOLD))
    } else if capture_name.starts_with("operator")
        || capture_name.starts_with("punctuation")
        || capture_name.starts_with("constant")
    {
        syntax_style(palette.muted, None, None)
    } else if capture_name.starts_with("markup") {
        syntax_style(palette.text, None, None)
    } else if capture_name.starts_with("error") {
        syntax_style(palette.red, None, Some(FontWeight::BOLD))
    } else {
        return None;
    };

    Some(style)
}

/// 判断内容是否像 XML。
fn looks_like_xml(lines: &[String]) -> bool {
    let Some(first) = first_non_empty_line(lines) else {
        return false;
    };
    let trimmed = first.trim_start();
    if trimmed.starts_with("<?xml") || trimmed.starts_with("<!") {
        return true;
    }

    trimmed.starts_with('<') && trimmed.contains('>') && !trimmed.starts_with("<<")
}

/// 判断内容是否像 properties 配置。
fn looks_like_properties(lines: &[String]) -> bool {
    let mut meaningful = 0usize;
    let mut property_like = 0usize;

    for line in lines.iter().take(120) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        meaningful += 1;
        if trimmed.starts_with('#') || trimmed.starts_with('!') {
            property_like += 1;
            continue;
        }
        if find_unescaped_property_separator(trimmed).is_some() {
            property_like += 1;
        }
    }

    meaningful >= 3 && property_like >= 3 && property_like * 2 >= meaningful
}

/// 判断内容是否像 Java 线程 dump 或异常堆栈。
fn looks_like_java_thread_or_exception(lines: &[String]) -> bool {
    let mut score = 0usize;

    for line in lines.iter().take(200) {
        let trimmed = line.trim_start();
        if trimmed.contains("java.lang.Thread.State:") {
            score += 3;
        }
        if trimmed.starts_with("at ") && trimmed.contains('(') && trimmed.contains(')') {
            score += 1;
        }
        if trimmed.starts_with("Caused by:") || trimmed.starts_with("Suppressed:") {
            score += 2;
        }
        if looks_like_java_thread_header(trimmed) {
            score += 3;
        }
        if contains_java_exception_token(trimmed) {
            score += 1;
        }
        if score >= 3 {
            return true;
        }
    }

    false
}

/// Tree-sitter 降级后选择轻量高亮模式。
fn fallback_mode_for_config_text(lines: &[String]) -> HighlightMode {
    if looks_like_java_thread_or_exception(lines) {
        HighlightMode::JavaThread
    } else {
        HighlightMode::Log
    }
}

/// 返回第一行非空文本。
fn first_non_empty_line(lines: &[String]) -> Option<&str> {
    lines
        .iter()
        .map(String::as_str)
        .find(|line| !line.trim().is_empty())
}

/// 生成普通日志单行高亮。
fn log_line_highlights(line: &str, theme: SyntaxTheme) -> LineHighlights {
    let mut highlights = java_thread_line_highlights(line, theme);

    push_timestamp_highlight(line, &mut highlights, theme);
    push_log_level_highlight(line, &mut highlights, theme);
    push_bracket_thread_highlight(line, &mut highlights, theme);
    push_logger_like_highlight(line, &mut highlights, theme);

    highlights
}

/// 生成 Java 线程 dump 或异常堆栈单行高亮。
fn java_thread_line_highlights(line: &str, theme: SyntaxTheme) -> LineHighlights {
    let mut highlights = Vec::new();

    push_java_thread_header_highlight(line, &mut highlights, theme);
    push_java_state_highlight(line, &mut highlights, theme);
    push_java_stack_frame_highlight(line, &mut highlights, theme);
    push_java_exception_highlight(line, &mut highlights, theme);
    push_java_lock_highlight(line, &mut highlights, theme);

    highlights
}

/// properties 的轻量降级高亮。
fn properties_line_fallback_highlights(line: &str, theme: SyntaxTheme) -> LineHighlights {
    let mut highlights = Vec::new();
    let palette = SyntaxPalette::for_theme(theme);
    let trimmed_start = line.len() - line.trim_start().len();
    let trimmed = &line[trimmed_start..];

    if trimmed.starts_with('#') || trimmed.starts_with('!') {
        highlights.push((
            trimmed_start..line.len(),
            syntax_style(palette.comment, None, None),
        ));
        return highlights;
    }

    if let Some(separator) = find_unescaped_property_separator(line) {
        highlights.push((
            0..separator,
            syntax_style(palette.blue, None, Some(FontWeight::BOLD)),
        ));
        highlights.push((
            separator..separator + 1,
            syntax_style(palette.muted, None, None),
        ));
        if separator + 1 < line.len() {
            highlights.push((
                separator + 1..line.len(),
                syntax_style(palette.green, None, None),
            ));
        }
    }

    push_escape_highlights(line, &mut highlights, theme);
    highlights
}

/// XML 的轻量降级高亮。
fn xml_line_fallback_highlights(line: &str, theme: SyntaxTheme) -> LineHighlights {
    let mut highlights = Vec::new();
    let palette = SyntaxPalette::for_theme(theme);
    let trimmed_start = line.len() - line.trim_start().len();
    let trimmed = &line[trimmed_start..];

    if trimmed.starts_with("<!--") {
        highlights.push((
            trimmed_start..line.len(),
            syntax_style(palette.comment, None, None),
        ));
        return highlights;
    }

    let mut offset = 0usize;
    while let Some(relative_start) = line[offset..].find('<') {
        let tag_start = offset + relative_start;
        let Some(relative_end) = line[tag_start..].find('>') else {
            break;
        };
        let tag_end = tag_start + relative_end + 1;
        highlights.push((
            tag_start..tag_start + 1,
            syntax_style(palette.muted, None, None),
        ));
        highlights.push((
            tag_end - 1..tag_end,
            syntax_style(palette.muted, None, None),
        ));

        if let Some(name_range) = xml_tag_name_range(line, tag_start, tag_end) {
            highlights.push((
                name_range,
                syntax_style(palette.purple, None, Some(FontWeight::BOLD)),
            ));
        }
        push_xml_attribute_highlights(&line[tag_start..tag_end], tag_start, &mut highlights, theme);
        offset = tag_end;
    }

    highlights
}

/// 高亮日志时间戳。
fn push_timestamp_highlight(line: &str, highlights: &mut LineHighlights, theme: SyntaxTheme) {
    let timestamp_ranges = log_timestamp_ranges(line);
    if !timestamp_ranges.is_empty() {
        for range in timestamp_ranges {
            highlights.push((range, timestamp_style(theme)));
        }
        return;
    }

    let trimmed_start = line.len() - line.trim_start().len();
    let trimmed = &line[trimmed_start..];
    let raw_timestamp_end = trimmed
        .find(|character: char| character.is_whitespace())
        .unwrap_or(trimmed.len());
    // 兜底规则保留旧行为：部分业务日志可能把日期、批次号或自定义时间戳写成非标准格式。
    // 兜底只检查行首短前缀，并把 32 字节上限回退到 UTF-8 边界，避免中文日志被截断到字符中间。
    let timestamp_end = floor_char_boundary(trimmed, raw_timestamp_end.min(32));
    let candidate = &trimmed[..timestamp_end];
    if candidate
        .chars()
        .any(|character| character.is_ascii_digit())
        && (candidate.contains('-') || candidate.contains('/') || candidate.contains(':'))
    {
        highlights.push((
            trimmed_start..trimmed_start + timestamp_end,
            timestamp_style(theme),
        ));
    }
}

/// 识别一行内所有常见日志时间戳范围。
///
/// 业务意图：
/// - 真实日志经常在服务名、等级或线程名前后出现时间，不一定从行首开始。
/// - 一行可能同时包含事件时间和请求/业务时间，只高亮第一个会让后续时间字段仍然不明显。
///
/// 边界条件：
/// - 扫描仍然是 O(行长)，只做 ASCII 分隔符和数字判断，适合虚拟列表逐行渲染。
/// - 通过左右边界过滤，避免把 `2115652402026-04-23` 这类拼接数字中间误判为时间。
fn log_timestamp_ranges(line: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut cursor = 0usize;

    while cursor < line.len() {
        let current = line[cursor..].chars().next().unwrap_or_default();
        if !is_timestamp_candidate_start(current) {
            cursor += current.len_utf8().max(1);
            continue;
        }

        if timestamp_left_boundary(line, cursor)
            && let Some(end) = parse_log_timestamp_at(line, cursor)
            && timestamp_right_boundary(line, end)
        {
            ranges.push(cursor..end);
            cursor = end;
            continue;
        }

        cursor += current.len_utf8().max(1);
    }

    ranges
}

/// 判断当前位置是否可能是时间戳起点。
fn is_timestamp_candidate_start(character: char) -> bool {
    character.is_ascii_digit() || matches!(character, '[' | '(')
}

/// 从指定位置解析日志时间戳，返回绝对结束位置。
///
/// 支持格式：
/// - `YYYY-MM-DD HH:mm:ss`、`YYYY/MM/DD HH:mm:ss,SSS`、`YYYY-MM-DDTHH:mm:ssZ`
/// - `YY-MM-DD HH:mm:ss`、`YYYYMMDD HHmmss`
/// - `2026年05月06日 12:00:00`
/// - `[25-12-20 00:20:27.014]`、`[06/May/2026:12:00:00 +0800]`
/// - 独立时间 `HH:mm:ss` 或 `HH:mm:ss.SSS`
fn parse_log_timestamp_at(line: &str, start: usize) -> Option<usize> {
    let opening = line[start..]
        .chars()
        .next()
        .filter(|character| matches!(character, '[' | '('));
    let inner_start = opening
        .map(|character| start + character.len_utf8())
        .unwrap_or(start);
    let text = &line[inner_start..];

    if let Some(common_log_len) = parse_common_log_timestamp_len(text) {
        let cursor = inner_start + common_log_len;
        return Some(include_matching_timestamp_close(line, cursor, opening));
    }

    if let Some(date_len) = parse_log_date_len(text) {
        let mut cursor = inner_start + date_len;
        let date_end = cursor;
        let mut time_start = cursor;

        if let Some(separator_len) = timestamp_date_time_separator_len(&line[time_start..]) {
            time_start += separator_len;
        } else {
            while time_start < line.len() && is_timestamp_inner_space(line.as_bytes()[time_start]) {
                time_start += 1;
            }
        }

        if time_start > date_end
            && let Some(time_len) = parse_log_time_len(&line[time_start..])
        {
            cursor = time_start + time_len;
            cursor = extend_timestamp_timezone(line, cursor);
        }

        return Some(include_matching_timestamp_close(line, cursor, opening));
    }

    if let Some(time_len) = parse_log_time_len(text) {
        let mut cursor = inner_start + time_len;
        cursor = extend_timestamp_timezone(line, cursor);
        return Some(include_matching_timestamp_close(line, cursor, opening));
    }

    None
}

/// 判断时间戳内部可接受的日期和时间分隔空白。
fn is_timestamp_inner_space(byte: u8) -> bool {
    byte.is_ascii_whitespace()
}

/// 返回日期和时间之间的非空白分隔符长度。
///
/// 业务意图：
/// - 常见日志除了空格，还会使用 `T`、`_` 连接日期和时间，例如 ISO-8601 或文件名式日志。
/// - 只接受单字节明确分隔符，避免把普通正文误拼进时间戳。
fn timestamp_date_time_separator_len(text: &str) -> Option<usize> {
    text.as_bytes()
        .first()
        .filter(|byte| matches!(byte, b'T' | b'_'))
        .map(|_byte| 1)
}

/// 判断时间戳左侧是否是安全边界。
///
/// 边界条件：
/// - 允许服务名前的空格、括号和分隔符。
/// - 不允许数字、字母、下划线等 token 内部开始，避免长数字串中误判日期。
fn timestamp_left_boundary(line: &str, start: usize) -> bool {
    start == 0 || !is_timestamp_adjacent_byte(line.as_bytes()[start - 1])
}

/// 判断时间戳右侧是否是安全边界。
fn timestamp_right_boundary(line: &str, end: usize) -> bool {
    end >= line.len() || !is_timestamp_adjacent_byte(line.as_bytes()[end])
}

/// 解析 `YYYY-MM-DD`、`YYYY/MM/DD`、`YY-MM-DD` 等日志日期前缀长度。
fn parse_log_date_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.len() >= 10
        && all_ascii_digits(bytes, 0..4)
        && is_date_separator(bytes[4])
        && all_ascii_digits(bytes, 5..7)
        && bytes[7] == bytes[4]
        && all_ascii_digits(bytes, 8..10)
        && plausible_month_day(bytes, 5, 8)
    {
        return Some(10);
    }

    if bytes.len() >= 8
        && all_ascii_digits(bytes, 0..2)
        && is_date_separator(bytes[2])
        && all_ascii_digits(bytes, 3..5)
        && bytes[5] == bytes[2]
        && all_ascii_digits(bytes, 6..8)
        && plausible_month_day(bytes, 3, 6)
    {
        return Some(8);
    }

    if let Some(chinese_date_len) = parse_chinese_log_date_len(text) {
        return Some(chinese_date_len);
    }

    if bytes.len() >= 8
        && all_ascii_digits(bytes, 0..8)
        && plausible_month_day(bytes, 4, 6)
        && timestamp_right_boundary_for_compact_date(text, 8)
    {
        return Some(8);
    }

    None
}

/// 解析中文日期 `YYYY年MM月DD日`。
fn parse_chinese_log_date_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let year_separator = "年".as_bytes();
    let month_separator = "月".as_bytes();
    let day_separator = "日".as_bytes();

    if bytes.len() < 17
        || !all_ascii_digits(bytes, 0..4)
        || !bytes[4..].starts_with(year_separator)
        || !all_ascii_digits(bytes, 7..9)
        || !bytes[9..].starts_with(month_separator)
        || !all_ascii_digits(bytes, 12..14)
        || !bytes[14..].starts_with(day_separator)
        || !plausible_month_day(bytes, 7, 12)
    {
        return None;
    }

    Some(17)
}

/// 判断紧跟在紧凑日期后的字符是否允许结束日期。
fn timestamp_right_boundary_for_compact_date(text: &str, end: usize) -> bool {
    end >= text.len()
        || text.as_bytes()[end].is_ascii_whitespace()
        || matches!(text.as_bytes()[end], b'T' | b'_' | b'-')
}

/// 解析 `HH:mm:ss`、`HH:mm:ss.SSS`、`HH:mm:ss,SSS` 或 `HHmmss` 时间前缀长度。
fn parse_log_time_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.len() >= 8
        && all_ascii_digits(bytes, 0..2)
        && bytes[2] == b':'
        && all_ascii_digits(bytes, 3..5)
        && bytes[5] == b':'
        && all_ascii_digits(bytes, 6..8)
    {
        return parse_colon_log_time_len(bytes);
    }

    if bytes.len() >= 6 && all_ascii_digits(bytes, 0..6) {
        return parse_compact_log_time_len(bytes);
    }

    None
}

/// 解析带冒号的时间长度。
fn parse_colon_log_time_len(bytes: &[u8]) -> Option<usize> {
    let hour = two_digit_number(bytes, 0)?;
    let minute = two_digit_number(bytes, 3)?;
    let second = two_digit_number(bytes, 6)?;
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let mut cursor = 8usize;
    if cursor < bytes.len() && matches!(bytes[cursor], b'.' | b',') {
        let fraction_start = cursor + 1;
        cursor = fraction_start;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor == fraction_start {
            return Some(8);
        }
    }

    Some(cursor)
}

/// 解析紧凑时间 `HHmmss`。
fn parse_compact_log_time_len(bytes: &[u8]) -> Option<usize> {
    let hour = two_digit_number(bytes, 0)?;
    let minute = two_digit_number(bytes, 2)?;
    let second = two_digit_number(bytes, 4)?;
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    Some(6)
}

/// 解析 Common Log Format 时间：`06/May/2026:12:00:00 +0800`。
fn parse_common_log_timestamp_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.len() < 20
        || !all_ascii_digits(bytes, 0..2)
        || bytes[2] != b'/'
        || !is_english_month_bytes(bytes, 3)
        || bytes[6] != b'/'
        || !all_ascii_digits(bytes, 7..11)
        || bytes[11] != b':'
    {
        return None;
    }

    let day = two_digit_number(bytes, 0)?;
    if !(1..=31).contains(&day) {
        return None;
    }

    let time_len = parse_log_time_len(&text[12..])?;
    let mut cursor = 12 + time_len;
    cursor = extend_timestamp_timezone(text, cursor);
    Some(cursor)
}

/// 判断英文月份缩写。
///
/// 边界条件：
/// - Common Log Format 的月份缩写是 3 个 ASCII 字节；日志里可能出现本地化月份或损坏文本。
/// - 这里先按字节确认 ASCII 范围，再转成 UTF-8，避免用固定字节范围切进中文或重音字符内部。
fn is_english_month_bytes(bytes: &[u8], start: usize) -> bool {
    let Some(month_bytes) = bytes.get(start..start + 3) else {
        return false;
    };
    if !month_bytes.iter().all(u8::is_ascii_alphabetic) {
        return false;
    }

    let Ok(text) = std::str::from_utf8(month_bytes) else {
        return false;
    };

    matches!(
        text,
        "Jan"
            | "Feb"
            | "Mar"
            | "Apr"
            | "May"
            | "Jun"
            | "Jul"
            | "Aug"
            | "Sep"
            | "Oct"
            | "Nov"
            | "Dec"
    )
}

/// 扩展时间戳中的时区后缀。
///
/// 边界条件：
/// - 只接受 `Z`、`+0800`、`+08:00` 这类紧邻或空白后的明确时区。
/// - 不把后续 `ERROR`、线程名或普通正文算进时间戳，避免过度高亮。
fn extend_timestamp_timezone(line: &str, cursor: usize) -> usize {
    if cursor < line.len() && line.as_bytes()[cursor] == b'Z' {
        return cursor + 1;
    }

    let mut timezone_start = cursor;
    while timezone_start < line.len() && line.as_bytes()[timezone_start].is_ascii_whitespace() {
        timezone_start += 1;
    }

    if let Some(timezone_len) = parse_numeric_timezone_len(&line[timezone_start..]) {
        timezone_start + timezone_len
    } else {
        cursor
    }
}

/// 解析数字时区长度。
fn parse_numeric_timezone_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.len() >= 5 && matches!(bytes[0], b'+' | b'-') && all_ascii_digits(bytes, 1..5) {
        return Some(5);
    }
    if bytes.len() >= 6
        && matches!(bytes[0], b'+' | b'-')
        && all_ascii_digits(bytes, 1..3)
        && bytes[3] == b':'
        && all_ascii_digits(bytes, 4..6)
    {
        return Some(6);
    }

    None
}

/// 如果时间戳以括号开头，则把匹配的闭括号纳入高亮范围。
fn include_matching_timestamp_close(line: &str, cursor: usize, opening: Option<char>) -> usize {
    let Some(opening) = opening else {
        return cursor;
    };
    let closing = match opening {
        '[' => b']',
        '(' => b')',
        _ => return cursor,
    };

    if cursor < line.len() && line.as_bytes()[cursor] == closing {
        cursor + 1
    } else {
        cursor
    }
}

/// 判断日期分隔符是否属于常见日志格式。
fn is_date_separator(byte: u8) -> bool {
    matches!(byte, b'-' | b'/' | b'.')
}

/// 判断日期中的月日字段是否处于合理范围。
fn plausible_month_day(bytes: &[u8], month_start: usize, day_start: usize) -> bool {
    let Some(month) = two_digit_number(bytes, month_start) else {
        return false;
    };
    let Some(day) = two_digit_number(bytes, day_start) else {
        return false;
    };

    (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// 判断指定字节范围是否全部是 ASCII 数字。
fn all_ascii_digits(bytes: &[u8], range: Range<usize>) -> bool {
    range.end <= bytes.len() && bytes[range].iter().all(u8::is_ascii_digit)
}

/// 读取两位 ASCII 数字。
fn two_digit_number(bytes: &[u8], start: usize) -> Option<u8> {
    if start + 1 >= bytes.len()
        || !bytes[start].is_ascii_digit()
        || !bytes[start + 1].is_ascii_digit()
    {
        return None;
    }

    Some((bytes[start] - b'0') * 10 + (bytes[start + 1] - b'0'))
}

/// 判断字节是否会让时间戳成为更长 token 的一部分。
fn is_timestamp_adjacent_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b',' | b'_')
}

/// 构造更醒目的时间戳文字样式。
///
/// 业务意图：
/// - 时间是日志排障的第一定位维度，使用较深的橙色和粗体，让它在等宽正文中明显区别于普通数字。
/// - 按用户要求不使用背景色，避免大面积时间列出现色块干扰阅读。
fn timestamp_style(theme: SyntaxTheme) -> HighlightStyle {
    let palette = SyntaxPalette::for_theme(theme);
    syntax_style(palette.orange, None, Some(FontWeight::BOLD))
}

/// 高亮日志等级关键字。
fn push_log_level_highlight(line: &str, highlights: &mut LineHighlights, theme: SyntaxTheme) {
    let palette = SyntaxPalette::for_theme(theme);
    let levels = [
        ("FATAL", palette.red, 0xffebe9),
        ("ERROR", palette.red, 0xffebe9),
        ("WARN", palette.yellow, 0xfff8c5),
        ("INFO", palette.blue, 0xddf4ff),
        ("DEBUG", palette.muted, 0xf6f8fa),
        ("TRACE", palette.purple, 0xfbefff),
    ];

    for (keyword, color, background) in levels {
        if let Some(start) = line.find(keyword) {
            highlights.push((
                start..start + keyword.len(),
                syntax_style(color, Some(background), Some(FontWeight::BOLD)),
            ));
            return;
        }
    }
}

/// 高亮常见方括号或花括号线程名。
fn push_bracket_thread_highlight(line: &str, highlights: &mut LineHighlights, theme: SyntaxTheme) {
    let palette = SyntaxPalette::for_theme(theme);
    for (open, close) in [('[', ']'), ('{', '}')] {
        let Some(start) = line.find(open) else {
            continue;
        };
        let Some(relative_end) = line[start + 1..].find(close) else {
            continue;
        };
        let end = start + 1 + relative_end + close.len_utf8();
        let content = &line[start + 1..end - close.len_utf8()];
        if !content.trim().is_empty() && content.len() <= 80 && !is_log_level_text(content.trim()) {
            highlights.push((
                start..end,
                syntax_style(palette.muted, Some(0xf6f8fa), None),
            ));
            return;
        }
    }
}

/// 高亮常见 logger 或 Java class token。
fn push_logger_like_highlight(line: &str, highlights: &mut LineHighlights, theme: SyntaxTheme) {
    let palette = SyntaxPalette::for_theme(theme);
    // Java 栈帧会被专用规则拆成包名、类名、方法名和源码位置；如果这里再把整段
    // `java.util.concurrent.Foo.bar` 当作 logger 覆盖，会把细分样式重新压成一整块。
    if line.trim_start().starts_with("at ") {
        return;
    }

    for (start, token) in ascii_tokens_with_offsets(line) {
        if token.len() > 6 && token.contains('.') && token.chars().any(|ch| ch.is_ascii_uppercase())
        {
            highlights.push((
                start..start + token.len(),
                syntax_style(palette.purple, None, None),
            ));
            return;
        }
    }
}

/// 高亮 Java 线程头部中的线程名和常见属性。
fn push_java_thread_header_highlight(
    line: &str,
    highlights: &mut LineHighlights,
    theme: SyntaxTheme,
) {
    let palette = SyntaxPalette::for_theme(theme);
    let trimmed_start = line.len() - line.trim_start().len();
    let trimmed = &line[trimmed_start..];
    if !looks_like_java_thread_header(trimmed) {
        return;
    }

    if let Some(rest) = trimmed.strip_prefix('"')
        && let Some(end_quote) = rest.find('"')
    {
        let end = trimmed_start + 1 + end_quote + 1;
        highlights.push((
            trimmed_start..end,
            syntax_style(palette.blue, Some(0xddf4ff), Some(FontWeight::BOLD)),
        ));
    }

    push_java_thread_header_attribute_highlights(line, highlights, theme);
}

/// 高亮 Java 线程状态。
fn push_java_state_highlight(line: &str, highlights: &mut LineHighlights, theme: SyntaxTheme) {
    let palette = SyntaxPalette::for_theme(theme);
    if let Some(start) = line.find("java.lang.Thread.State:") {
        let label_end = start + "java.lang.Thread.State:".len();
        highlights.push((
            start..label_end,
            syntax_style(palette.muted, None, Some(FontWeight::BOLD)),
        ));
    }

    for state in [
        "RUNNABLE",
        "BLOCKED",
        "TIMED_WAITING",
        "WAITING",
        "NEW",
        "TERMINATED",
    ] {
        if let Some(start) = line.find(state) {
            highlights.push((
                start..start + state.len(),
                java_thread_state_style(state, theme),
            ));
            return;
        }
    }
}

/// 高亮 Java 栈帧。
fn push_java_stack_frame_highlight(
    line: &str,
    highlights: &mut LineHighlights,
    theme: SyntaxTheme,
) {
    let palette = SyntaxPalette::for_theme(theme);
    let trimmed_start = line.len() - line.trim_start().len();
    let trimmed = &line[trimmed_start..];
    if !trimmed.starts_with("at ") {
        return;
    }

    highlights.push((
        trimmed_start..trimmed_start + 2,
        syntax_style(palette.muted, None, Some(FontWeight::BOLD)),
    ));
    if let Some(paren_start) = trimmed.find('(') {
        let frame_start = trimmed_start + 3;
        let frame_end = trimmed_start + paren_start;
        if frame_start < frame_end {
            push_java_call_target_highlights(line, frame_start..frame_end, highlights, theme);
        }
        push_java_frame_source_highlights(
            line,
            trimmed_start,
            trimmed,
            paren_start,
            highlights,
            theme,
        );
    }
}

/// 高亮 Java 异常类和异常链标记。
fn push_java_exception_highlight(line: &str, highlights: &mut LineHighlights, theme: SyntaxTheme) {
    let palette = SyntaxPalette::for_theme(theme);
    for marker in ["Caused by:", "Suppressed:"] {
        if let Some(start) = line.find(marker) {
            highlights.push((
                start..start + marker.len(),
                syntax_style(palette.red, Some(0xffebe9), Some(FontWeight::BOLD)),
            ));
        }
    }

    for (start, token) in ascii_tokens_with_offsets(line) {
        if token.ends_with("Exception") || token.ends_with("Error") || token.ends_with("Throwable")
        {
            push_java_type_token_highlight(
                start,
                token,
                highlights,
                syntax_style(palette.red, Some(0xffebe9), Some(FontWeight::BOLD)),
                theme,
            );
            return;
        }
    }
}

/// 高亮 Java 锁等待和持有信息。
fn push_java_lock_highlight(line: &str, highlights: &mut LineHighlights, theme: SyntaxTheme) {
    let palette = SyntaxPalette::for_theme(theme);
    let markers = [
        ("deadlock", palette.red, 0xffebe9),
        ("waiting to lock", palette.red, 0xffebe9),
        ("- waiting on", palette.yellow, 0xfff8c5),
        ("- parking to wait for", palette.yellow, 0xfff8c5),
        ("- locked", palette.green, 0xdfffe0),
        ("locked", palette.green, 0xdfffe0),
    ];
    for (marker, color, background) in markers {
        if let Some(start) = line.find(marker) {
            highlights.push((
                start..start + marker.len(),
                syntax_style(color, Some(background), Some(FontWeight::BOLD)),
            ));
            push_java_monitor_object_highlight(line, start + marker.len(), highlights, theme);
            return;
        }
    }
}

/// 高亮 Java 线程头中的属性键值对。
///
/// 业务意图：
/// - 线程 dump 头部通常包含线程名、`id`、`tid`、`nid`、优先级和 CPU 时间；这些字段对定位
///   线程身份和资源占用很关键，不能只把线程名染色后让其它信息淹没在普通文本里。
/// - 这里按 ASCII 标记扫描，不解析整行语法，保证在大型线程 dump 的虚拟列表滚动中保持 O(行长)。
fn push_java_thread_header_attribute_highlights(
    line: &str,
    highlights: &mut LineHighlights,
    theme: SyntaxTheme,
) {
    let palette = SyntaxPalette::for_theme(theme);
    for marker in [
        "id=",
        "tid=",
        "nid=",
        "prio=",
        "os_prio=",
        "cpu=",
        "elapsed=",
        "CPU Time=",
    ] {
        push_java_header_key_value_highlights(line, marker, highlights, theme);
    }

    // HotSpot 线程头常见 `#12` 序号；它不是键值对，但对人工对照线程也有价值。
    // 只高亮井号后的连续数字，避免把日志正文中的普通 `#` 注释误判为线程序号。
    let mut cursor = 0usize;
    while let Some(relative_start) = line[cursor..].find('#') {
        let start = cursor + relative_start;
        let mut end = start + 1;
        while end < line.len() && line.as_bytes()[end].is_ascii_digit() {
            end += 1;
        }
        if end > start + 1 {
            highlights.push((
                start..end,
                syntax_style(palette.orange, Some(0xfff8c5), None),
            ));
            break;
        }
        cursor = start + 1;
    }
}

/// 高亮线程头属性中的单个 `key=value` 标记。
///
/// 边界条件：
/// - `CPU Time=` 这类键名包含空格，不能依赖通用 token 迭代器。
/// - 属性值可能是带引号的数字，也可能是 `0x...` 或普通数字；扫描到下一个空白即可。
fn push_java_header_key_value_highlights(
    line: &str,
    marker: &str,
    highlights: &mut LineHighlights,
    theme: SyntaxTheme,
) {
    let palette = SyntaxPalette::for_theme(theme);
    let Some(relative_equal) = marker.rfind('=') else {
        return;
    };

    let mut cursor = 0usize;
    while let Some(relative_start) = line[cursor..].find(marker) {
        let key_start = cursor + relative_start;
        if key_start > 0 && is_java_header_key_byte(line.as_bytes()[key_start - 1]) {
            cursor = key_start + marker.len();
            continue;
        }

        let equal = key_start + relative_equal;
        let value_start = equal + 1;
        let value_end = java_header_value_end(line, value_start);

        if key_start < equal {
            highlights.push((
                key_start..equal,
                syntax_style(palette.muted, None, Some(FontWeight::BOLD)),
            ));
        }
        highlights.push((equal..equal + 1, syntax_style(palette.muted, None, None)));
        if value_start < value_end {
            highlights.push((
                value_start..value_end,
                syntax_style(palette.green, None, Some(FontWeight::BOLD)),
            ));
        }

        cursor = value_end.max(value_start);
    }
}

/// 返回 Java 线程头属性值的结束位置。
fn java_header_value_end(line: &str, value_start: usize) -> usize {
    let mut cursor = value_start;
    while cursor < line.len() && !line.as_bytes()[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    cursor
}

/// 判断字节是否属于 Java 线程头属性键名。
///
/// 边界条件：
/// - `id=` 会出现在 `tid=` / `nid=` 内部，`prio=` 也会出现在 `os_prio=` 内部。
/// - 如果不检查左侧边界，会重复生成重叠高亮，最终导致属性字段显示混乱。
fn is_java_header_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// 根据线程状态返回更有语义区分度的样式。
///
/// 业务意图：
/// - `RUNNABLE`、`BLOCKED`、`WAITING` 等状态本身就是诊断信号，使用状态色可以让问题线程
///   在大量重复栈帧中更快被扫到。
fn java_thread_state_style(state: &str, theme: SyntaxTheme) -> HighlightStyle {
    let palette = SyntaxPalette::for_theme(theme);
    match state {
        "RUNNABLE" => syntax_style(palette.green, Some(0xdfffe0), Some(FontWeight::BOLD)),
        "BLOCKED" => syntax_style(palette.red, Some(0xffebe9), Some(FontWeight::BOLD)),
        "WAITING" | "TIMED_WAITING" => {
            syntax_style(palette.yellow, Some(0xfff8c5), Some(FontWeight::BOLD))
        }
        "NEW" => syntax_style(palette.blue, Some(0xddf4ff), Some(FontWeight::BOLD)),
        "TERMINATED" => syntax_style(palette.muted, Some(0xf6f8fa), Some(FontWeight::BOLD)),
        _ => syntax_style(palette.muted, Some(0xf6f8fa), Some(FontWeight::BOLD)),
    }
}

/// 高亮 Java 栈帧调用目标。
///
/// 业务意图：
/// - Java 栈帧最有诊断价值的是“在哪个类的哪个方法”；如果把完整限定名整段染成同色，
///   方法名和类名很难从重复包名前缀中跳出来。
/// - 本函数按最后两个 `.` 拆分 package / class / method，只处理 ASCII Java 标识符片段，
///   因此不会制造非法 UTF-8 range。
fn push_java_call_target_highlights(
    line: &str,
    frame_range: Range<usize>,
    highlights: &mut LineHighlights,
    theme: SyntaxTheme,
) {
    let palette = SyntaxPalette::for_theme(theme);
    let frame = &line[frame_range.clone()];
    let Some(method_dot_relative) = frame.rfind('.') else {
        highlights.push((
            frame_range,
            syntax_style(palette.purple, None, Some(FontWeight::BOLD)),
        ));
        return;
    };

    let method_dot = frame_range.start + method_dot_relative;
    let method_start = method_dot + 1;
    let owner = &frame[..method_dot_relative];
    let class_start = owner
        .rfind('.')
        .map(|relative_dot| frame_range.start + relative_dot + 1)
        .unwrap_or(frame_range.start);

    if frame_range.start < class_start.saturating_sub(1) {
        highlights.push((
            frame_range.start..class_start - 1,
            syntax_style(palette.muted, None, None),
        ));
    }
    if class_start < method_dot {
        highlights.push((
            class_start..method_dot,
            syntax_style(palette.purple, None, Some(FontWeight::BOLD)),
        ));
    }
    if method_start < frame_range.end {
        highlights.push((
            method_start..frame_range.end,
            syntax_style(palette.blue, None, Some(FontWeight::BOLD)),
        ));
    }
}

/// 高亮 Java 栈帧括号中的源码位置。
///
/// 业务意图：
/// - `Foo.java:123` 的文件名和行号是跳转定位线索，应该区别于调用目标。
/// - `Native Method`、`Unknown Source` 没有可跳转行号，使用低饱和背景提示其特殊性。
fn push_java_frame_source_highlights(
    line: &str,
    trimmed_start: usize,
    trimmed: &str,
    paren_start: usize,
    highlights: &mut LineHighlights,
    theme: SyntaxTheme,
) {
    let palette = SyntaxPalette::for_theme(theme);
    let open = trimmed_start + paren_start;
    let close = trimmed[paren_start..]
        .find(')')
        .map(|relative| open + relative)
        .unwrap_or(trimmed_start + trimmed.len());

    highlights.push((open..open + 1, syntax_style(palette.muted, None, None)));
    if close > open + 1 {
        let inner_start = open + 1;
        let inner_end = close;
        let inner = &line[inner_start..inner_end];
        if matches!(inner, "Native Method" | "Unknown Source") {
            highlights.push((
                inner_start..inner_end,
                syntax_style(palette.muted, Some(0xf6f8fa), None),
            ));
        } else if let Some(relative_colon) = inner.rfind(':') {
            let colon = inner_start + relative_colon;
            if inner_start < colon {
                highlights.push((inner_start..colon, syntax_style(palette.green, None, None)));
            }
            highlights.push((colon..colon + 1, syntax_style(palette.muted, None, None)));
            if colon + 1 < inner_end {
                highlights.push((
                    colon + 1..inner_end,
                    syntax_style(palette.orange, None, Some(FontWeight::BOLD)),
                ));
            }
        } else {
            highlights.push((
                inner_start..inner_end,
                syntax_style(palette.green, None, None),
            ));
        }
    }
    if close < trimmed_start + trimmed.len() {
        highlights.push((close..close + 1, syntax_style(palette.muted, None, None)));
    }
}

/// 高亮 Java 类型 token，优先突出类型短名。
///
/// 业务意图：
/// - 异常行中的 `java.lang.IllegalStateException` 如果整段红色，包名前缀会分散注意力。
/// - 拆出短类名后，真正的异常类型更醒目，同时保留包名前缀供需要时辨认。
fn push_java_type_token_highlight(
    start: usize,
    token: &str,
    highlights: &mut LineHighlights,
    class_style: HighlightStyle,
    theme: SyntaxTheme,
) {
    let palette = SyntaxPalette::for_theme(theme);
    if let Some(relative_dot) = token.rfind('.') {
        let class_start = start + relative_dot + 1;
        if start < class_start - 1 {
            highlights.push((
                start..class_start - 1,
                syntax_style(palette.muted, None, None),
            ));
        }
        highlights.push((class_start..start + token.len(), class_style));
    } else {
        highlights.push((start..start + token.len(), class_style));
    }
}

/// 高亮锁对象或监视器地址。
///
/// 业务意图：
/// - `- waiting on <0x...>` / `- locked <0x...>` 中尖括号对象是定位锁竞争的关键标识。
/// - 只高亮尖括号内的对象，避免整行背景色过重，线程 dump 阅读时能同时保留栈帧层次。
fn push_java_monitor_object_highlight(
    line: &str,
    search_start: usize,
    highlights: &mut LineHighlights,
    theme: SyntaxTheme,
) {
    let palette = SyntaxPalette::for_theme(theme);
    let Some(relative_start) = line[search_start..].find('<') else {
        return;
    };
    let start = search_start + relative_start;
    let Some(relative_end) = line[start..].find('>') else {
        return;
    };
    let end = start + relative_end + 1;
    highlights.push((
        start..end,
        syntax_style(palette.purple, Some(0xfbefff), None),
    ));
}

/// 判断单行是否像 Java 线程 dump 的线程头。
fn looks_like_java_thread_header(trimmed: &str) -> bool {
    trimmed.starts_with('"')
        && trimmed[1..].contains('"')
        && (trimmed.contains("id=")
            || trimmed.contains("prio=")
            || trimmed.contains("tid=")
            || trimmed.contains("nid=")
            || trimmed.contains("CPU Time="))
}

/// 判断单行是否包含 Java 异常 token。
fn contains_java_exception_token(line: &str) -> bool {
    ascii_tokens_with_offsets(line).any(|(_start, token)| {
        token.ends_with("Exception") || token.ends_with("Error") || token.ends_with("Throwable")
    })
}

/// 查找 properties 行中第一个未转义的 `=` 或 `:`。
fn find_unescaped_property_separator(line: &str) -> Option<usize> {
    let mut previous_backslash_count = 0usize;

    for (index, byte) in line.bytes().enumerate() {
        if byte == b'\\' {
            previous_backslash_count += 1;
            continue;
        }
        let escaped = previous_backslash_count % 2 == 1;
        previous_backslash_count = 0;
        if !escaped && (byte == b'=' || byte == b':') {
            return Some(index);
        }
    }

    None
}

/// 高亮字符串中的 properties 转义序列。
fn push_escape_highlights(line: &str, highlights: &mut LineHighlights, theme: SyntaxTheme) {
    let palette = SyntaxPalette::for_theme(theme);
    let bytes = line.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            // properties 转义序列以反斜杠加“下一个字符”为单位；下一个字符可能是中文等多字节字符。
            // 使用 `chars().next()` 取得完整字符长度，保证输出给 StyledText 的 range 始终位于 UTF-8 边界。
            let next_character_start = index + 1;
            let end = line[next_character_start..]
                .chars()
                .next()
                .map(|character| next_character_start + character.len_utf8())
                .unwrap_or(next_character_start);
            if index < end {
                highlights.push((
                    index..end,
                    syntax_style(palette.orange, None, Some(FontWeight::BOLD)),
                ));
            }
            index = end;
        } else {
            index += 1;
        }
    }
}

/// 将任意字节下标回退到当前字符串内最近的 UTF-8 字符边界。
///
/// 业务意图：
/// - 轻量高亮规则为了性能会按字节处理 ASCII 片段，但日志内容可以包含中文。
/// - 在需要按固定字节上限截断时，必须先修正边界，避免渲染阶段因非法 range 崩溃。
fn floor_char_boundary(text: &str, limit: usize) -> usize {
    let mut boundary = limit.min(text.len());
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

/// 判断括号内容是否是日志等级。
///
/// 业务意图：
/// - `[ERROR]`、`[WARN]` 是常见日志等级写法，不应被当作线程名整段覆盖高亮。
/// - 这里保持大小写敏感，和日志等级高亮规则一致，避免误伤普通线程名。
fn is_log_level_text(text: &str) -> bool {
    matches!(
        text,
        "FATAL" | "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE"
    )
}

/// 返回 XML 标签名范围。
fn xml_tag_name_range(line: &str, tag_start: usize, tag_end: usize) -> Option<Range<usize>> {
    let mut cursor = tag_start + 1;
    if line[cursor..tag_end].starts_with('/') || line[cursor..tag_end].starts_with('?') {
        cursor += 1;
    }
    while cursor < tag_end && line.as_bytes()[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    let name_start = cursor;
    while cursor < tag_end {
        let byte = line.as_bytes()[cursor];
        if byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>' | b'?') {
            break;
        }
        cursor += 1;
    }

    (name_start < cursor).then_some(name_start..cursor)
}

/// 高亮 XML 标签内部属性名和值。
fn push_xml_attribute_highlights(
    tag_text: &str,
    tag_offset: usize,
    highlights: &mut LineHighlights,
    theme: SyntaxTheme,
) {
    let palette = SyntaxPalette::for_theme(theme);
    let bytes = tag_text.as_bytes();
    let mut index = 0usize;

    while index < bytes.len() {
        if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            let name_start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric()
                    || matches!(bytes[index], b'_' | b'-' | b':' | b'.'))
            {
                index += 1;
            }
            let name_end = index;
            let mut cursor = index;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b'=' {
                highlights.push((
                    tag_offset + name_start..tag_offset + name_end,
                    syntax_style(palette.blue, None, None),
                ));
                highlights.push((
                    tag_offset + cursor..tag_offset + cursor + 1,
                    syntax_style(palette.muted, None, None),
                ));
                if cursor + 1 < bytes.len() {
                    push_xml_attribute_value_highlight(
                        tag_text,
                        tag_offset,
                        cursor + 1,
                        highlights,
                        theme,
                    );
                }
            }
        } else {
            index += 1;
        }
    }
}

/// 高亮 XML 属性值。
fn push_xml_attribute_value_highlight(
    tag_text: &str,
    tag_offset: usize,
    value_start: usize,
    highlights: &mut LineHighlights,
    theme: SyntaxTheme,
) {
    let palette = SyntaxPalette::for_theme(theme);
    let bytes = tag_text.as_bytes();
    let mut cursor = value_start;
    while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
        cursor += 1;
    }
    if cursor >= bytes.len() || !matches!(bytes[cursor], b'"' | b'\'') {
        return;
    }
    let quote = bytes[cursor];
    let start = cursor;
    cursor += 1;
    while cursor < bytes.len() && bytes[cursor] != quote {
        cursor += 1;
    }
    if cursor < bytes.len() {
        cursor += 1;
    }
    highlights.push((
        tag_offset + start..tag_offset + cursor,
        syntax_style(palette.green, None, None),
    ));
}

/// 枚举 ASCII token 及其字节偏移。
fn ascii_tokens_with_offsets(line: &str) -> impl Iterator<Item = (usize, &str)> {
    AsciiTokenIterator { line, offset: 0 }
}

/// ASCII token 迭代器。
struct AsciiTokenIterator<'a> {
    /// 原始行文本。
    line: &'a str,
    /// 下一次扫描的字节偏移。
    offset: usize,
}

impl<'a> Iterator for AsciiTokenIterator<'a> {
    type Item = (usize, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.line.as_bytes();
        while self.offset < bytes.len() && !is_ascii_token_byte(bytes[self.offset]) {
            self.offset += 1;
        }
        if self.offset >= bytes.len() {
            return None;
        }

        let start = self.offset;
        while self.offset < bytes.len() && is_ascii_token_byte(bytes[self.offset]) {
            self.offset += 1;
        }

        Some((start, &self.line[start..self.offset]))
    }
}

/// 判断字节是否属于日志中常见 ASCII token。
fn is_ascii_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'$')
}

/// 规范化单行高亮，去掉重叠和无效范围。
fn normalize_line_highlights(highlights: &mut LineHighlights) {
    highlights.retain(|(range, _style)| range.start < range.end);
    highlights.sort_by(|left, right| {
        left.0
            .start
            .cmp(&right.0.start)
            .then_with(|| left.0.end.cmp(&right.0.end))
    });

    let mut normalized = Vec::with_capacity(highlights.len());
    let mut cursor = 0usize;
    for (range, style) in highlights.drain(..) {
        if range.end <= cursor {
            continue;
        }
        let start = range.start.max(cursor);
        if start < range.end {
            cursor = range.end;
            normalized.push((start..range.end, style));
        }
    }

    *highlights = normalized;
}

/// 构造统一的高亮样式。
fn syntax_style(
    color: u32,
    _background: Option<u32>,
    font_weight: Option<FontWeight>,
) -> HighlightStyle {
    // 用户要求高亮只通过文字颜色表达，不再使用背景色块。
    // 保留 `_background` 参数是为了调用点仍能表达 token 的语义强弱，后续如增加主题开关可恢复使用。
    HighlightStyle {
        color: Some(rgb(color).into()),
        background_color: None,
        font_weight,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    //! 高亮模块单元测试。
    //!
    //! 业务意图：
    //! - 锁定模式识别、关键 token 高亮和大文件降级规则，避免后续 UI 调整破坏日志阅读能力。

    use super::*;

    /// 返回当前行被高亮覆盖到的文本片段。
    ///
    /// 业务意图：
    /// - 高亮样式属于 GPUI 类型，单元测试只关心“关键 token 是否被拆分出来”。
    /// - 通过片段断言可以避免测试和具体颜色强绑定，后续微调主题色不会导致无关失败。
    fn highlighted_fragments(mode: HighlightMode, line: &str) -> Vec<&str> {
        highlight_line(mode, line, None, SyntaxTheme::Light)
            .iter()
            .map(|(range, _style)| &line[range.clone()])
            .collect()
    }

    /// 返回指定片段对应的高亮样式。
    ///
    /// 业务意图：
    /// - 暗色主题可读性依赖具体颜色，部分测试需要锁定关键 token 不再回退到浅色主题的深灰或深绿。
    fn style_for_fragment(
        mode: HighlightMode,
        line: &str,
        fragment: &str,
        theme: SyntaxTheme,
    ) -> Option<HighlightStyle> {
        highlight_line(mode, line, None, theme)
            .into_iter()
            .find(|(range, _style)| &line[range.clone()] == fragment)
            .map(|(_range, style)| style)
    }

    /// 断言高亮样式使用指定文字颜色。
    fn assert_style_color(style: HighlightStyle, color: u32) {
        assert_eq!(style.color, Some(rgb(color).into()));
    }

    /// 验证 `.log` 文件按普通日志模式处理，并能高亮日志等级。
    #[test]
    fn log_文件识别并高亮等级() {
        let lines = vec!["2026-05-06 12:00:00 ERROR failed".to_string()];

        assert_eq!(detect_highlight_mode("app.log", &lines), HighlightMode::Log);
        assert!(
            highlight_line(HighlightMode::Log, &lines[0], None, SyntaxTheme::Light)
                .iter()
                .any(|(range, _style)| &lines[0][range.clone()] == "ERROR")
        );
    }

    /// 验证普通日志行首的完整日期时间会作为一个时间戳整体高亮。
    #[test]
    fn log_完整日期时间整体高亮() {
        let line = "2026-05-06 12:00:00.123 ERROR failed";
        let fragments = highlighted_fragments(HighlightMode::Log, line);

        assert!(fragments.contains(&"2026-05-06 12:00:00.123"));
    }

    /// 验证非行首日期时间也能被识别，覆盖 `service 2026-04-28 ...` 这类表格化日志。
    #[test]
    fn log_非行首日期时间高亮() {
        let line = "ecology 2026-04-28 00:09:34 0 50 300";
        let fragments = highlighted_fragments(HighlightMode::Log, line);

        assert!(fragments.contains(&"2026-04-28 00:09:34"));
    }

    /// 验证线程日志和应用日志常见的方括号日期时间可以被高亮。
    #[test]
    fn log_方括号日期时间整体高亮() {
        let line = "[25-12-20 00:20:27.014] {resin-main-link} Shutdown Resin reason: OK";
        let fragments = highlighted_fragments(HighlightMode::Log, line);

        assert!(fragments.contains(&"[25-12-20 00:20:27.014]"));
    }

    /// 验证日志等级之后的独立时间也能被识别，避免只支持行首时间戳。
    #[test]
    fn log_等级后的独立时间高亮() {
        let line = "INFO 12:34:56.789 worker started";
        let fragments = highlighted_fragments(HighlightMode::Log, line);

        assert!(fragments.contains(&"12:34:56.789"));
    }

    /// 验证多种常见时间格式可以同时高亮，且拼接在长数字中的伪日期不会误判。
    #[test]
    fn log_多格式时间高亮并跳过拼接伪日期() {
        let line = "20260506_123456 [06/May/2026:12:34:56 +0800] 中文 2026年05月06日 12:34:56 2115652402026-04-23";
        let fragments = highlighted_fragments(HighlightMode::Log, line);

        assert!(fragments.contains(&"20260506_123456"));
        assert!(fragments.contains(&"[06/May/2026:12:34:56 +0800]"));
        assert!(fragments.contains(&"2026年05月06日 12:34:56"));
        assert!(!fragments.contains(&"2026-04-23"));
    }

    /// 验证 Common Log 月份字段包含非 ASCII 字符时不会因为固定字节切片而崩溃。
    #[test]
    fn common_log_非_ascii_月份不会截断_utf8() {
        let line = "06/féé/2026:12:00:00 +0800 GET /index.html";
        let highlights = highlight_line(HighlightMode::Log, line, None, SyntaxTheme::Light);

        for (range, _style) in highlights {
            assert!(line.is_char_boundary(range.start));
            assert!(line.is_char_boundary(range.end));
        }
    }

    /// 验证高亮统一使用文字颜色，不再输出背景色。
    #[test]
    fn 高亮样式不使用背景色() {
        for line in [
            "2026-05-06 12:00:00 ERROR failed",
            "\"worker-1\" #12 prio=5 os_prio=31 tid=0x1 nid=0x2 RUNNABLE",
            "   java.lang.Thread.State: BLOCKED",
            "server.port=8080",
        ] {
            for (_range, style) in
                highlight_line(HighlightMode::Log, line, None, SyntaxTheme::Light)
            {
                assert!(style.background_color.is_none());
            }
        }
    }

    /// 验证语法高亮基础色在暗色主题下保持可读。
    ///
    /// 业务意图：
    /// - XML/properties 的 Tree-sitter 高亮曾复用浅色主题深灰、深绿和近黑色，暗色背景下几乎不可见。
    /// - 这里直接锁定 capture 到颜色的映射，避免后续调色时重新引入低对比颜色。
    #[test]
    fn tree_sitter_高亮基础色适配暗色背景() {
        assert_style_color(
            style_for_tree_sitter_capture("markup.raw", SyntaxTheme::Dark)
                .expect("markup 应有样式"),
            SyntaxPalette::for_theme(SyntaxTheme::Dark).text,
        );
        assert_style_color(
            style_for_tree_sitter_capture("string", SyntaxTheme::Dark).expect("string 应有样式"),
            SyntaxPalette::for_theme(SyntaxTheme::Dark).green,
        );
        assert_style_color(
            style_for_tree_sitter_capture("punctuation.delimiter", SyntaxTheme::Dark)
                .expect("标点应有样式"),
            SyntaxPalette::for_theme(SyntaxTheme::Dark).muted,
        );
    }

    /// 验证明亮主题保留原有深色语法颜色。
    ///
    /// 业务意图：
    /// - 暗色主题需要亮色高亮，但明亮主题必须继续使用深色高亮，避免浅灰文字落在白色背景上不可读。
    #[test]
    fn tree_sitter_高亮基础色保留明亮主题对比度() {
        assert_style_color(
            style_for_tree_sitter_capture("punctuation.delimiter", SyntaxTheme::Light)
                .expect("标点应有样式"),
            SyntaxPalette::for_theme(SyntaxTheme::Light).muted,
        );
        assert_style_color(
            style_for_tree_sitter_capture("string", SyntaxTheme::Light).expect("string 应有样式"),
            SyntaxPalette::for_theme(SyntaxTheme::Light).green,
        );
    }

    /// 验证时间戳候选前缀包含中文时，高亮范围仍保持合法 UTF-8 边界。
    #[test]
    fn 中文时间戳候选截断保持_utf8_边界() {
        let line = "2026-05-06中文中文中文中文中文中文中文中文 ERROR failed";
        let highlights = highlight_line(HighlightMode::Log, line, None, SyntaxTheme::Light);

        for (range, _style) in highlights {
            assert!(line.is_char_boundary(range.start));
            assert!(line.is_char_boundary(range.end));
        }
    }

    /// 验证 properties 降级高亮中的反斜杠转义不会截断中文字符。
    #[test]
    fn properties_中文转义范围保持_utf8_边界() {
        let line = "path=\\中文";
        let highlights = highlight_line(HighlightMode::Properties, line, None, SyntaxTheme::Light);

        for (range, _style) in highlights {
            assert!(line.is_char_boundary(range.start));
            assert!(line.is_char_boundary(range.end));
        }
    }

    /// 验证 `[ERROR]` 这类括号日志等级不会被线程名规则覆盖。
    #[test]
    fn 方括号日志等级优先按等级高亮() {
        let line = "[ERROR] failed";
        let highlights = highlight_line(HighlightMode::Log, line, None, SyntaxTheme::Light);

        assert!(
            highlights
                .iter()
                .any(|(range, _style)| &line[range.clone()] == "ERROR")
        );
        assert!(
            !highlights
                .iter()
                .any(|(range, _style)| &line[range.clone()] == "[ERROR]")
        );
    }

    /// 验证 Java 线程 dump 和异常堆栈可以自动识别并高亮核心标记。
    #[test]
    fn java线程日志识别并高亮状态和异常链() {
        let lines = vec![
            "\"worker-1\" #12 prio=5 os_prio=31 tid=0x1 nid=0x2 RUNNABLE".to_string(),
            "   java.lang.Thread.State: RUNNABLE".to_string(),
            "Caused by: java.lang.IllegalStateException: failed".to_string(),
        ];

        assert_eq!(
            detect_highlight_mode("thread_dump.txt", &lines),
            HighlightMode::JavaThread
        );
        assert!(
            highlight_line(
                HighlightMode::JavaThread,
                &lines[1],
                None,
                SyntaxTheme::Light
            )
            .iter()
            .any(|(range, _style)| &lines[1][range.clone()] == "RUNNABLE")
        );
        assert!(
            highlight_line(
                HighlightMode::JavaThread,
                &lines[2],
                None,
                SyntaxTheme::Light
            )
            .iter()
            .any(|(range, _style)| &lines[2][range.clone()] == "Caused by:")
        );
    }

    /// 验证 Java 栈帧不再把完整调用目标整段染色，而是拆出包名、类名、方法名和源码行号。
    #[test]
    fn java栈帧高亮拆分类名方法和源码位置() {
        let line =
            "    at java.util.concurrent.LinkedBlockingQueue.take(LinkedBlockingQueue.java:442)";
        let fragments = highlighted_fragments(HighlightMode::JavaThread, line);

        assert!(fragments.contains(&"java.util.concurrent"));
        assert!(fragments.contains(&"LinkedBlockingQueue"));
        assert!(fragments.contains(&"take"));
        assert!(fragments.contains(&"LinkedBlockingQueue.java"));
        assert!(fragments.contains(&"442"));
    }

    /// 验证线程日志中大量重复的包名前缀不再使用暗色下过低对比的灰色。
    #[test]
    fn java线程栈帧弱化文本在暗色主题下仍可读() {
        let line =
            "    at java.util.concurrent.LinkedBlockingQueue.take(LinkedBlockingQueue.java:442)";
        let style = style_for_fragment(
            HighlightMode::JavaThread,
            line,
            "java.util.concurrent",
            SyntaxTheme::Dark,
        )
        .expect("包名前缀应被高亮");

        assert_style_color(style, SyntaxPalette::for_theme(SyntaxTheme::Dark).muted);
    }

    /// 验证线程日志弱化文本在明亮主题下不会使用暗色主题浅灰。
    #[test]
    fn java线程栈帧弱化文本保留明亮主题对比度() {
        let line =
            "    at java.util.concurrent.LinkedBlockingQueue.take(LinkedBlockingQueue.java:442)";
        let style = style_for_fragment(
            HighlightMode::JavaThread,
            line,
            "java.util.concurrent",
            SyntaxTheme::Light,
        )
        .expect("包名前缀应被高亮");

        assert_style_color(style, SyntaxPalette::for_theme(SyntaxTheme::Light).muted);
    }

    /// 验证截图中的轻量线程头格式也能高亮线程名、线程 id 和 CPU 时间。
    #[test]
    fn java线程头高亮线程名和关键属性() {
        let line = "\"HRM-4-thread-18\" id=\"5663\" CPU Time=62500000";
        let fragments = highlighted_fragments(HighlightMode::JavaThread, line);

        assert!(fragments.contains(&"\"HRM-4-thread-18\""));
        assert!(fragments.contains(&"id"));
        assert!(fragments.contains(&"\"5663\""));
        assert!(fragments.contains(&"CPU Time"));
        assert!(fragments.contains(&"62500000"));
    }

    /// 验证锁等待行只突出等待动作和监视器对象，避免整行背景色压住栈帧层次。
    #[test]
    fn java锁等待高亮动作和监视器对象() {
        let line = "    - waiting to lock <0x000000076abf4d10>";
        let fragments = highlighted_fragments(HighlightMode::JavaThread, line);

        assert!(fragments.contains(&"waiting to lock"));
        assert!(fragments.contains(&"<0x000000076abf4d10>"));
    }

    /// 验证 properties 文件可以按扩展名识别，并通过 Tree-sitter 高亮 key/value。
    #[test]
    fn properties_文件识别并生成预计算高亮() {
        let text = "# 注释\nserver.port=8080\napp.name=LogClinic";
        let lines = text.lines().map(ToString::to_string).collect::<Vec<_>>();
        let plan = prepare_highlighting(
            "app.properties",
            text,
            &lines,
            text.len(),
            SyntaxTheme::Light,
        );

        assert_eq!(plan.mode, HighlightMode::Properties);
        assert!(plan.precomputed.is_some());
        assert!(plan.precomputed.unwrap().lines[1].len() >= 2);
    }

    /// 验证 XML 文件可以按扩展名识别，并通过 Tree-sitter 高亮标签和属性。
    #[test]
    fn xml_文件识别并生成预计算高亮() {
        let text = r#"<configuration><property name="log.level">INFO</property></configuration>"#;
        let lines = vec![text.to_string()];
        let plan = prepare_highlighting("log.xml", text, &lines, text.len(), SyntaxTheme::Light);

        assert_eq!(plan.mode, HighlightMode::Xml);
        assert!(plan.precomputed.is_some());
        assert!(!plan.precomputed.unwrap().lines[0].is_empty());
    }

    /// 验证无扩展名内容可以通过启发式识别 XML。
    #[test]
    fn 无扩展名_xml_内容启发式识别() {
        let lines = vec!["<?xml version=\"1.0\"?>".to_string(), "<root/>".to_string()];

        assert_eq!(detect_highlight_mode("payload", &lines), HighlightMode::Xml);
    }

    /// 验证无扩展名内容可以通过启发式识别 properties。
    #[test]
    fn 无扩展名_properties_内容启发式识别() {
        let lines = vec![
            "# comment".to_string(),
            "server.port=8080".to_string(),
            "app.name=LogClinic".to_string(),
        ];

        assert_eq!(
            detect_highlight_mode("config", &lines),
            HighlightMode::Properties
        );
    }

    /// 验证超过 Tree-sitter 阈值的配置文件会降级，不返回预计算高亮。
    #[test]
    fn 超过阈值的配置文件降级为轻量高亮() {
        let lines = vec!["<root/>".to_string()];
        let plan = prepare_highlighting(
            "large.xml",
            "<root/>",
            &lines,
            TREE_SITTER_HIGHLIGHT_MAX_BYTES + 1,
            SyntaxTheme::Light,
        );

        assert_eq!(plan.mode, HighlightMode::Log);
        assert!(plan.precomputed.is_none());
    }

    /// 验证中文和 ASCII 高亮范围共存时仍保持合法 UTF-8 边界。
    #[test]
    fn 中文内容中的高亮范围保持_utf8_边界() {
        let line = "INFO 中文日志";
        let highlights = highlight_line(HighlightMode::Log, line, None, SyntaxTheme::Light);

        for (range, _style) in highlights {
            assert!(line.is_char_boundary(range.start));
            assert!(line.is_char_boundary(range.end));
        }
    }
}
