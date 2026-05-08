//! 分页日志流式搜索模块。
//!
//! 业务意图：
//! - 超大日志不能为了搜索把所有行解码进内存，因此分页文档按行读取、即时匹配、即时丢弃未命中文本。
//! - 搜索结果仍复用现有 `SearchResultItem`，保证搜索面板、点击跳转和高亮逻辑不需要区分内存/分页来源。
//! - 单次搜索最多收集 50,000 行结果，避免过宽关键字在大日志中生成百万级 UI 行。
//!
//! 关键约束：
//! - 当前搜索仍是单行普通文本搜索，不做正则、跨行或多关键字组合。
//! - 不区分大小写时沿用现有 Unicode 小写折叠规则，保证内存模式和分页模式匹配语义一致。

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    ops::Range,
};

use crate::{
    line_index::LineIndexEntry,
    log_content::decode_lossy,
    paged_document::PagedLogDocument,
    search::{SearchOptions, SearchResultItem, source_location_label},
};

/// 单次搜索最多保留的结果行数。
pub const STREAM_SEARCH_RESULT_LIMIT: usize = 50_000;

/// 分页搜索结果。
#[derive(Clone, Debug)]
pub struct StreamSearchOutcome {
    /// 命中行结果。
    pub results: Vec<SearchResultItem>,
    /// 是否因为结果过多被截断。
    pub truncated: bool,
}

/// 在分页文档中搜索。
pub fn search_paged_document(
    document: &PagedLogDocument,
    options: &SearchOptions,
) -> StreamSearchOutcome {
    if options.is_empty_query() {
        return StreamSearchOutcome {
            results: Vec::new(),
            truncated: false,
        };
    }

    let source = document.source.clone();
    let file_name = source.display_name();
    let source_key = source.stable_key();
    let location = source_location_label(&source);
    let mut results = Vec::new();

    let Ok(mut file) = File::open(&document.path) else {
        return StreamSearchOutcome {
            results,
            truncated: false,
        };
    };
    let _ = document.line_index.for_each(|line_index, entry| {
        let Some(line_text) = read_line_text(&mut file, document, entry) else {
            return Ok(true);
        };
        if let Some(match_range) =
            find_query_range(&line_text, &options.query, options.case_sensitive)
        {
            results.push(SearchResultItem {
                source: source.clone(),
                source_key: source_key.clone(),
                file_name: file_name.clone(),
                location: location.clone(),
                line_index,
                line_text,
                match_range,
            });
            if results.len() >= STREAM_SEARCH_RESULT_LIMIT {
                return Ok(false);
            }
        }
        Ok(true)
    });

    let truncated = results.len() >= STREAM_SEARCH_RESULT_LIMIT;
    StreamSearchOutcome { results, truncated }
}

/// 统计分页文档中查询词出现次数。
pub fn count_query_occurrences_paged(
    document: &PagedLogDocument,
    options: &SearchOptions,
) -> usize {
    if options.is_empty_query() {
        return 0;
    }

    let mut count = 0_usize;
    let Ok(mut file) = File::open(&document.path) else {
        return 0;
    };
    let _ = document.line_index.for_each(|_, entry| {
        let Some(line_text) = read_line_text(&mut file, document, entry) else {
            return Ok(true);
        };
        count = count.saturating_add(count_query_occurrences_in_line(
            &line_text,
            &options.query,
            options.case_sensitive,
        ));
        Ok(true)
    });
    count
}

/// 使用单个文件句柄按索引读取并解码一行。
///
/// 业务意图：
/// - 搜索和计数会顺序访问大量行，必须复用文件句柄，避免每行 `File::open` 带来的系统调用风暴。
fn read_line_text(
    file: &mut File,
    document: &PagedLogDocument,
    entry: LineIndexEntry,
) -> Option<String> {
    file.seek(SeekFrom::Start(entry.offset)).ok()?;
    let mut bytes = vec![0_u8; entry.byte_len as usize];
    file.read_exact(&mut bytes).ok()?;
    decode_lossy(&bytes, document.encoding)
        .ok()
        .map(|decoded| decoded.text)
}

/// 在单行文本中查找查询词并返回原始行内的字节范围。
fn find_query_range(line_text: &str, query: &str, case_sensitive: bool) -> Option<Range<usize>> {
    if query.is_empty() {
        return None;
    }

    if case_sensitive {
        return line_text
            .find(query)
            .map(|start| start..start + query.len());
    }

    let folded_line = line_text.to_lowercase();
    let folded_query = query.to_lowercase();
    let folded_start = folded_line.find(&folded_query)?;
    let folded_end = folded_start + folded_query.len();
    folded_range_to_original_range(line_text, &folded_line, folded_start..folded_end)
}

/// 将小写折叠后的字节范围映射回原始字符串字节范围。
fn folded_range_to_original_range(
    original: &str,
    folded: &str,
    folded_range: Range<usize>,
) -> Option<Range<usize>> {
    let mut folded_char_starts = Vec::new();
    for (byte_index, _) in folded.char_indices() {
        folded_char_starts.push(byte_index);
    }
    folded_char_starts.push(folded.len());

    let start_char = folded_char_starts
        .iter()
        .position(|byte_index| *byte_index == folded_range.start)?;
    let end_char = folded_char_starts
        .iter()
        .position(|byte_index| *byte_index == folded_range.end)?;

    let mut original_char_starts = Vec::new();
    for (byte_index, _) in original.char_indices() {
        original_char_starts.push(byte_index);
    }
    original_char_starts.push(original.len());

    let start = *original_char_starts.get(start_char)?;
    let end = *original_char_starts.get(end_char)?;
    Some(start..end)
}

/// 统计单行中的普通文本出现次数。
fn count_query_occurrences_in_line(line_text: &str, query: &str, case_sensitive: bool) -> usize {
    if query.is_empty() {
        return 0;
    }

    if case_sensitive {
        return line_text.matches(query).count();
    }

    let folded_line = line_text.to_lowercase();
    let folded_query = query.to_lowercase();
    folded_line.matches(&folded_query).count()
}
