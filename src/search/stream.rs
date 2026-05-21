//! 分页日志流式搜索模块。
//!
//! 业务意图：
//! - 超大日志不能为了搜索把所有行解码进内存，因此分页文档按行读取、即时匹配、即时丢弃未命中文本。
//! - 搜索结果仍复用现有 `SearchResultItem`，保证搜索面板、点击跳转和高亮逻辑不需要区分内存/分页来源。
//! - 单次搜索最多收集 50,000 行结果，避免过宽关键字在大日志中生成百万级 UI 行。
//!
//! 关键约束：
//! - 搜索仍按单行执行，不做跨行组合；多关键字快搜按“任一关键字命中”处理。
//! - 普通文本不区分大小写时沿用现有 Unicode 小写折叠规则，正则模式完全由表达式自身控制。

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    ops::Range,
};

use crate::{
    log_document::{LineIndexEntry, PagedLogDocument, decode_lossy},
    log_source::LogFileSource,
    search::{
        SearchMatchPosition, SearchMatcher, SearchOptions, SearchResultItem, source_location_label,
    },
};

/// 单次搜索最多保留的结果行数。
pub const STREAM_SEARCH_RESULT_LIMIT: usize = 50_000;

/// 分页搜索顺序扫描时的单批读取上限。
///
/// 业务意图：
/// - 搜索和计数通常从头到尾顺序扫描，按 8MiB 左右合并读取可以显著减少 `seek/read` 次数。
/// - 该值和行索引扫描块保持同一量级，既能摊薄系统调用，也不会因为一次搜索批次造成过高内存峰值。
const STREAM_SEARCH_BATCH_BYTES: u64 = 8 * 1024 * 1024;

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
    let Ok(matcher) = SearchMatcher::new(options) else {
        return StreamSearchOutcome {
            results: Vec::new(),
            truncated: false,
        };
    };

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
    let _ = for_each_paged_line_in_range(
        &mut file,
        document,
        0..document.line_index.len(),
        |line_index, line_text| {
            if let Some(match_range) = matcher.best_range(&line_text) {
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
                    return false;
                }
            }
            true
        },
    );

    let truncated = results.len() >= STREAM_SEARCH_RESULT_LIMIT;
    StreamSearchOutcome { results, truncated }
}

/// 从分页日志指定命中位置之后查找下一处命中。
///
/// 业务意图：
/// - 分页日志可能非常大，但搜索窗口“下一个”仍需要和内存日志保持片段级导航语义。
/// - 如果当前行还有更靠后的命中，优先复用当前行；只有当前行没有后续命中时才继续读取后续行。
///
/// 边界条件：
/// - `previous` 为 `None` 时从文件顶部开始。
/// - `wrap` 为 true 时会在文件末尾后绕回文件顶部；只有一处命中时允许回到自身。
pub fn find_paged_search_result_after_position(
    document: &PagedLogDocument,
    options: &SearchOptions,
    previous: Option<&SearchMatchPosition>,
    wrap: bool,
) -> Option<SearchResultItem> {
    if options.is_empty_query() {
        return None;
    }
    let Ok(matcher) = SearchMatcher::new(options) else {
        return None;
    };
    let Ok(mut file) = File::open(&document.path) else {
        return None;
    };

    let source = document.source.clone();
    let file_name = source.display_name();
    let source_key = source.stable_key();
    let location = source_location_label(&source);
    let line_count = document.line_index.len();
    let Some(previous) = previous.filter(|previous| previous.line_index < line_count) else {
        return find_paged_search_result_in_range(
            &mut file,
            document,
            &matcher,
            &source,
            &file_name,
            &source_key,
            &location,
            0..line_count,
        );
    };

    find_paged_search_result_in_line_after(
        &mut file,
        document,
        &matcher,
        &source,
        &file_name,
        &source_key,
        &location,
        previous.line_index,
        &previous.match_range,
    )
    .or_else(|| {
        find_paged_search_result_in_range(
            &mut file,
            document,
            &matcher,
            &source,
            &file_name,
            &source_key,
            &location,
            previous.line_index.saturating_add(1)..line_count,
        )
    })
    .or_else(|| {
        wrap.then(|| {
            find_paged_search_result_in_range(
                &mut file,
                document,
                &matcher,
                &source,
                &file_name,
                &source_key,
                &location,
                0..previous.line_index,
            )
            .or_else(|| {
                find_paged_search_result_in_line_first(
                    &mut file,
                    document,
                    &matcher,
                    &source,
                    &file_name,
                    &source_key,
                    &location,
                    previous.line_index,
                )
            })
        })
        .flatten()
    })
}

/// 从分页日志指定命中位置之前查找上一处命中。
///
/// 业务意图：
/// - “上一个”必须能在同一行的多个命中之间反向移动，避免只按行号跳转导致部分关键字永远无法定位。
pub fn find_paged_search_result_before_position(
    document: &PagedLogDocument,
    options: &SearchOptions,
    previous: Option<&SearchMatchPosition>,
    wrap: bool,
) -> Option<SearchResultItem> {
    if options.is_empty_query() {
        return None;
    }
    let Ok(matcher) = SearchMatcher::new(options) else {
        return None;
    };
    let Ok(mut file) = File::open(&document.path) else {
        return None;
    };

    let source = document.source.clone();
    let file_name = source.display_name();
    let source_key = source.stable_key();
    let location = source_location_label(&source);
    let line_count = document.line_index.len();
    let Some(previous) = previous.filter(|previous| previous.line_index < line_count) else {
        return find_paged_search_result_in_range_rev(
            &mut file,
            document,
            &matcher,
            &source,
            &file_name,
            &source_key,
            &location,
            0..line_count,
        );
    };

    find_paged_search_result_in_line_before(
        &mut file,
        document,
        &matcher,
        &source,
        &file_name,
        &source_key,
        &location,
        previous.line_index,
        &previous.match_range,
    )
    .or_else(|| {
        find_paged_search_result_in_range_rev(
            &mut file,
            document,
            &matcher,
            &source,
            &file_name,
            &source_key,
            &location,
            0..previous.line_index,
        )
    })
    .or_else(|| {
        wrap.then(|| {
            find_paged_search_result_in_range_rev(
                &mut file,
                document,
                &matcher,
                &source,
                &file_name,
                &source_key,
                &location,
                previous.line_index.saturating_add(1)..line_count,
            )
            .or_else(|| {
                find_paged_search_result_in_line_last(
                    &mut file,
                    document,
                    &matcher,
                    &source,
                    &file_name,
                    &source_key,
                    &location,
                    previous.line_index,
                )
            })
        })
        .flatten()
    })
}

/// 在分页日志的指定行范围内查找第一条命中。
///
/// 实现原因：
/// - 分页行索引支持按行号读取单行，范围查找可以避免为了“下一个”从头重复扫描已经经过的行。
fn find_paged_search_result_in_range(
    file: &mut File,
    document: &PagedLogDocument,
    matcher: &SearchMatcher,
    source: &LogFileSource,
    file_name: &str,
    source_key: &str,
    location: &str,
    line_range: Range<usize>,
) -> Option<SearchResultItem> {
    let mut found = None;
    let _ = for_each_paged_line_in_range(file, document, line_range, |line_index, line_text| {
        if let Some(match_range) = matcher.best_range(&line_text) {
            found = Some(SearchResultItem {
                source: source.clone(),
                source_key: source_key.to_string(),
                file_name: file_name.to_string(),
                location: location.to_string(),
                line_index,
                line_text,
                match_range,
            });
            return false;
        }
        true
    });
    found
}

/// 在分页日志指定行中查找锚点之后的第一处命中。
fn find_paged_search_result_in_line_after(
    file: &mut File,
    document: &PagedLogDocument,
    matcher: &SearchMatcher,
    source: &LogFileSource,
    file_name: &str,
    source_key: &str,
    location: &str,
    line_index: usize,
    previous_range: &Range<usize>,
) -> Option<SearchResultItem> {
    let line_text = read_paged_line_text(file, document, line_index)?;
    matcher
        .ranges(&line_text)
        .into_iter()
        .find(|range| (range.start, range.end) > (previous_range.start, previous_range.end))
        .map(|match_range| {
            search_result_item(
                source,
                file_name,
                source_key,
                location,
                line_index,
                &line_text,
                match_range,
            )
        })
}

/// 在分页日志指定行中查找锚点之前的最后一处命中。
fn find_paged_search_result_in_line_before(
    file: &mut File,
    document: &PagedLogDocument,
    matcher: &SearchMatcher,
    source: &LogFileSource,
    file_name: &str,
    source_key: &str,
    location: &str,
    line_index: usize,
    previous_range: &Range<usize>,
) -> Option<SearchResultItem> {
    let line_text = read_paged_line_text(file, document, line_index)?;
    matcher
        .ranges(&line_text)
        .into_iter()
        .rev()
        .find(|range| (range.start, range.end) < (previous_range.start, previous_range.end))
        .map(|match_range| {
            search_result_item(
                source,
                file_name,
                source_key,
                location,
                line_index,
                &line_text,
                match_range,
            )
        })
}

/// 在分页日志指定行中查找第一处命中。
fn find_paged_search_result_in_line_first(
    file: &mut File,
    document: &PagedLogDocument,
    matcher: &SearchMatcher,
    source: &LogFileSource,
    file_name: &str,
    source_key: &str,
    location: &str,
    line_index: usize,
) -> Option<SearchResultItem> {
    let line_text = read_paged_line_text(file, document, line_index)?;
    matcher
        .ranges(&line_text)
        .into_iter()
        .next()
        .map(|match_range| {
            search_result_item(
                source,
                file_name,
                source_key,
                location,
                line_index,
                &line_text,
                match_range,
            )
        })
}

/// 在分页日志指定行中查找最后一处命中。
fn find_paged_search_result_in_line_last(
    file: &mut File,
    document: &PagedLogDocument,
    matcher: &SearchMatcher,
    source: &LogFileSource,
    file_name: &str,
    source_key: &str,
    location: &str,
    line_index: usize,
) -> Option<SearchResultItem> {
    let line_text = read_paged_line_text(file, document, line_index)?;
    matcher
        .ranges(&line_text)
        .into_iter()
        .last()
        .map(|match_range| {
            search_result_item(
                source,
                file_name,
                source_key,
                location,
                line_index,
                &line_text,
                match_range,
            )
        })
}

/// 按行号读取分页日志单行文本。
fn read_paged_line_text(
    file: &mut File,
    document: &PagedLogDocument,
    line_index: usize,
) -> Option<String> {
    let entry = document.line_index.get(line_index)?;
    read_line_text(file, document, entry)
}

/// 构造分页搜索结果，集中维护跨路径一致的展示字段。
fn search_result_item(
    source: &LogFileSource,
    file_name: &str,
    source_key: &str,
    location: &str,
    line_index: usize,
    line_text: &str,
    match_range: Range<usize>,
) -> SearchResultItem {
    SearchResultItem {
        source: source.clone(),
        source_key: source_key.to_string(),
        file_name: file_name.to_string(),
        location: location.to_string(),
        line_index,
        line_text: line_text.to_string(),
        match_range,
    }
}

/// 在分页日志的指定行范围内反向查找第一条命中。
///
/// 实现原因：
/// - 分页索引支持随机读取任意行，反向查找可以让“上一个”从当前位置向文件头定位，同时保持内存占用固定。
fn find_paged_search_result_in_range_rev(
    file: &mut File,
    document: &PagedLogDocument,
    matcher: &SearchMatcher,
    source: &LogFileSource,
    file_name: &str,
    source_key: &str,
    location: &str,
    line_range: Range<usize>,
) -> Option<SearchResultItem> {
    let mut found = None;
    let _ =
        for_each_paged_line_in_range_rev(file, document, line_range, |line_index, line_text| {
            if let Some(match_range) = matcher.best_range(&line_text) {
                found = Some(SearchResultItem {
                    source: source.clone(),
                    source_key: source_key.to_string(),
                    file_name: file_name.to_string(),
                    location: location.to_string(),
                    line_index,
                    line_text,
                    match_range,
                });
                return false;
            }
            true
        });
    found
}

/// 统计分页文档中查询词出现次数。
pub fn count_query_occurrences_paged(
    document: &PagedLogDocument,
    options: &SearchOptions,
) -> usize {
    if options.is_empty_query() {
        return 0;
    }
    let Ok(matcher) = SearchMatcher::new(options) else {
        return 0;
    };

    let mut count = 0_usize;
    let Ok(mut file) = File::open(&document.path) else {
        return 0;
    };
    let _ = for_each_paged_line_in_range(
        &mut file,
        document,
        0..document.line_index.len(),
        |_, line_text| {
            count = count.saturating_add(matcher.count_in_line(&line_text));
            true
        },
    );
    count
}

/// 按连续字节窗口顺序遍历分页日志行。
///
/// 业务意图：
/// - 分页全文搜索和计数会线性访问大量相邻行，如果每行都 `seek + read + Vec`，性能会被系统调用和临时分配主导。
/// - 这里按行索引把相邻行合并成有限大小的字节窗口，一次读取后按每行原始偏移切片解码，调用方仍以单行文本执行匹配。
///
/// 边界条件：
/// - 单行超过批次上限时仍单独读取该行，保持旧实现能处理超长行的行为，只是不把其它行并入同一批次。
/// - 任一批次读取失败时返回 `false`，调用方按“无法继续扫描”处理；这和旧实现遇到单行读取失败后跳过的容错略有不同，但能避免在损坏偏移上反复随机读取。
fn for_each_paged_line_in_range<F>(
    file: &mut File,
    document: &PagedLogDocument,
    line_range: Range<usize>,
    mut callback: F,
) -> bool
where
    F: FnMut(usize, String) -> bool,
{
    let range_end = line_range.end.min(document.line_index.len());
    let mut current_line = line_range.start.min(range_end);
    while current_line < range_end {
        let Some((batch_start_line, batch_end_line, batch_offset, batch_len)) =
            next_forward_batch(document, current_line, range_end)
        else {
            return true;
        };
        let Some(span) = read_paged_batch_bytes(file, batch_offset, batch_len) else {
            return false;
        };
        for line_index in batch_start_line..batch_end_line {
            let Some(line_text) = decode_line_from_batch(document, line_index, batch_offset, &span)
            else {
                continue;
            };
            if !callback(line_index, line_text) {
                return false;
            }
        }
        current_line = batch_end_line;
    }
    true
}

/// 按连续字节窗口反向遍历分页日志行。
///
/// 业务意图：
/// - “上一个”搜索需要从当前位置向文件头扫描；按反向批次读取可以避免每一行都随机 seek。
/// - 批次内部仍按行号倒序回调，保证同旧实现一样优先返回离当前位置最近的上一条命中。
///
/// 边界条件：
/// - 范围为空时直接完成。
/// - 读取失败时停止扫描，避免对同一异常区域重复产生大量 I/O。
fn for_each_paged_line_in_range_rev<F>(
    file: &mut File,
    document: &PagedLogDocument,
    line_range: Range<usize>,
    mut callback: F,
) -> bool
where
    F: FnMut(usize, String) -> bool,
{
    let range_start = line_range.start.min(document.line_index.len());
    let mut current_end = line_range.end.min(document.line_index.len());
    while current_end > range_start {
        let Some((batch_start_line, batch_end_line, batch_offset, batch_len)) =
            next_reverse_batch(document, range_start, current_end)
        else {
            return true;
        };
        let Some(span) = read_paged_batch_bytes(file, batch_offset, batch_len) else {
            return false;
        };
        for line_index in (batch_start_line..batch_end_line).rev() {
            let Some(line_text) = decode_line_from_batch(document, line_index, batch_offset, &span)
            else {
                continue;
            };
            if !callback(line_index, line_text) {
                return false;
            }
        }
        current_end = batch_start_line;
    }
    true
}

/// 计算下一段向前扫描的批次范围。
///
/// 返回值中的行号范围是左闭右开，字节范围从 `batch_offset` 开始、长度为 `batch_len`。
fn next_forward_batch(
    document: &PagedLogDocument,
    start_line: usize,
    range_end: usize,
) -> Option<(usize, usize, u64, u64)> {
    let first_entry = document.line_index.get(start_line)?;
    let batch_offset = first_entry.offset;
    let mut batch_end_line = start_line + 1;
    let mut batch_end_offset = line_entry_end_offset(first_entry)?;
    while batch_end_line < range_end {
        let next_entry = document.line_index.get(batch_end_line)?;
        let next_end_offset = line_entry_end_offset(next_entry)?;
        let next_batch_len = next_end_offset.checked_sub(batch_offset)?;
        if next_batch_len > STREAM_SEARCH_BATCH_BYTES {
            break;
        }
        batch_end_offset = next_end_offset;
        batch_end_line += 1;
        if next_batch_len >= STREAM_SEARCH_BATCH_BYTES {
            break;
        }
    }
    Some((
        start_line,
        batch_end_line,
        batch_offset,
        batch_end_offset.checked_sub(batch_offset)?,
    ))
}

/// 计算下一段反向扫描的批次范围。
///
/// 业务意图：
/// - 反向扫描从 `current_end - 1` 开始，尽量向前合并到 8MiB 附近，返回的行号范围仍保持左闭右开，方便复用同一解码函数。
fn next_reverse_batch(
    document: &PagedLogDocument,
    range_start: usize,
    current_end: usize,
) -> Option<(usize, usize, u64, u64)> {
    let last_line = current_end.checked_sub(1)?;
    let last_entry = document.line_index.get(last_line)?;
    let mut batch_start_line = last_line;
    let mut batch_offset = last_entry.offset;
    let batch_end_offset = line_entry_end_offset(last_entry)?;
    while batch_start_line > range_start {
        let previous_line = batch_start_line - 1;
        let previous_entry = document.line_index.get(previous_line)?;
        let next_batch_len = batch_end_offset.checked_sub(previous_entry.offset)?;
        if next_batch_len > STREAM_SEARCH_BATCH_BYTES {
            break;
        }
        batch_start_line = previous_line;
        batch_offset = previous_entry.offset;
        if next_batch_len >= STREAM_SEARCH_BATCH_BYTES {
            break;
        }
    }
    Some((
        batch_start_line,
        current_end,
        batch_offset,
        batch_end_offset.checked_sub(batch_offset)?,
    ))
}

/// 读取分页搜索批次的原始字节。
///
/// 边界条件：
/// - 空字节范围返回空数组，覆盖空行和空文件规范化后的首行。
/// - 字节长度不能转换为当前平台 `usize` 时直接失败，避免异常索引导致隐式截断。
fn read_paged_batch_bytes(file: &mut File, offset: u64, byte_len: u64) -> Option<Vec<u8>> {
    if byte_len == 0 {
        return Some(Vec::new());
    }
    let byte_len = usize::try_from(byte_len).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = vec![0_u8; byte_len];
    file.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

/// 从已读取批次中解码指定行。
///
/// 业务意图：
/// - 行索引保存的是原始文件偏移，批次解码需要把绝对偏移换算成当前字节窗口内的相对切片。
/// - 只为当前行解码 `String`，避免把整个 8MiB 批次先转成一个大字符串后再按换行拆分，兼容 GBK 等非 UTF-8 编码。
fn decode_line_from_batch(
    document: &PagedLogDocument,
    line_index: usize,
    batch_offset: u64,
    batch: &[u8],
) -> Option<String> {
    let entry = document.line_index.get(line_index)?;
    let relative_offset = entry.offset.checked_sub(batch_offset)?;
    let relative_start = usize::try_from(relative_offset).ok()?;
    let relative_end = relative_start.checked_add(entry.byte_len as usize)?;
    let bytes = batch.get(relative_start..relative_end)?;
    decode_lossy(bytes, document.encoding)
        .ok()
        .map(|decoded| decoded.text)
}

/// 返回行索引条目的末尾字节偏移。
fn line_entry_end_offset(entry: LineIndexEntry) -> Option<u64> {
    entry.offset.checked_add(u64::from(entry.byte_len))
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

#[cfg(test)]
mod tests {
    //! 分页搜索规则测试。
    //!
    //! 业务意图：
    //! - 超大日志分页路径不能和内存日志搜索出现语义差异，尤其是正则匹配范围和计数规则。

    use std::{
        env, fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::{
        archive::MaterializedLogSource,
        log_document::{EncodingChoice, LogTextEncoding},
        log_source::LogFileSource,
        search::{SearchMatchMode, count_query_occurrences, search_lines},
    };

    /// 为测试创建唯一临时目录，避免并行测试互相覆盖分页文件。
    fn unique_test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("测试时钟应可用")
            .as_nanos();
        env::temp_dir().join(format!(
            "logclinic-stream-search-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    /// 构造分页文档并返回需要清理的临时目录。
    ///
    /// 边界条件：
    /// - 测试写入普通 UTF-8 文件，目的是验证分页搜索逻辑，不覆盖编码探测异常。
    fn paged_document_with_content(name: &str, content: &str) -> (PathBuf, PagedLogDocument) {
        let dir = unique_test_dir(name);
        fs::create_dir_all(&dir).expect("测试目录应能创建");
        let path = dir.join("app.log");
        fs::write(&path, content).expect("测试日志应能写入");
        let byte_len = fs::metadata(&path).expect("测试日志元数据应可读").len();
        let source = LogFileSource::LocalFile { path: path.clone() };
        let materialized = MaterializedLogSource {
            original_source: source,
            temp_path: path,
            byte_len,
        };
        let document = PagedLogDocument::open(
            materialized,
            EncodingChoice::Manual(LogTextEncoding::Utf8),
            "app.log",
        )
        .expect("测试分页文档应能打开");
        (dir, document)
    }

    /// 分页正则搜索应与内存日志搜索返回同样的命中行和高亮片段。
    #[test]
    fn 分页正则搜索与内存搜索一致() {
        let content = "INFO id=42\nWARN id=100\nINFO id=7\n";
        let (dir, document) = paged_document_with_content("regex-search", content);
        let source = document.source.clone();
        let lines = content.lines().map(str::to_string).collect::<Vec<_>>();
        let options = SearchOptions::single_with_mode(r"id=\d+", false, SearchMatchMode::Regex);

        let paged = search_paged_document(&document, &options);
        let memory = search_lines(&source, &lines, &options);

        let paged_ranges = paged
            .results
            .iter()
            .map(|item| {
                (
                    item.line_index,
                    item.line_text[item.match_range.clone()].to_string(),
                )
            })
            .collect::<Vec<_>>();
        let memory_ranges = memory
            .iter()
            .map(|item| {
                (
                    item.line_index,
                    item.line_text[item.match_range.clone()].to_string(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(paged_ranges, memory_ranges);

        let _ = fs::remove_dir_all(dir);
    }

    /// 分页正则计数应与内存日志计数一致，避免“计数”和“搜索结果”在大文件上出现不同语义。
    #[test]
    fn 分页正则计数与内存计数一致() {
        let content = "INFO id=42 id=100\nWARN no id\nINFO id=7\n";
        let (dir, document) = paged_document_with_content("regex-count", content);
        let lines = content.lines().map(str::to_string).collect::<Vec<_>>();
        let options = SearchOptions::single_with_mode(r"id=\d+", true, SearchMatchMode::Regex);

        assert_eq!(
            count_query_occurrences_paged(&document, &options),
            count_query_occurrences(&lines, &options)
        );

        let _ = fs::remove_dir_all(dir);
    }
}
