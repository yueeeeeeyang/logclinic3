//! 日志智能分析业务域。
//!
//! 业务意图：
//! - 为日志树右键“智能分析”提供独立于 GPUI 的分块、提示词、OpenAI 兼容请求和流式事件模型。
//! - 日志内容可能来自普通文件、压缩包成员或分页物化文件，本模块只消费统一的 `LogFileSource`，
//!   读取细节继续复用 `log_document`，避免在 UI 层重新实现跨平台文件和压缩包处理。
//!
//! 关键约束：
//! - 任何日志内容都可能包含敏感信息，只有用户主动点击智能分析时才会通过本模块发送给已配置模型。
//! - 大日志必须按固定字节预算拆分，避免单次请求超过模型上下文；分页日志按需读取行，不能整文件载入内存。
//! - 模型推理内容只接受服务端 SSE 显式字段，不能通过提示词伪造隐藏思维链。

use std::{
    collections::{BTreeSet, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use crate::{
    ai_chat::{
        AiChatReasoningEffort, AiChatStreamEvent, OpenAiCompatibleChatMessage,
        stream_openai_compatible_chat_messages_with_handler,
    },
    archive::cleanup_materialized_file,
    config::ModelProfile,
    log_document::{EncodingChoice, LargeLogOpenResult, open_log_source_for_tab},
    log_loader::{LoadedLogTree, LogTreeEntryKind, LogTreeRow},
    log_source::LogFileSource,
};

/// 单个日志分析分块的正文字节预算。
///
/// 业务意图：
/// - 用户要求按 500KB 一批发送日志；这里按 UTF-8 文本实际字节数控制，避免中文、多字节符号和英文日志体量估算不一致。
/// - 预算只覆盖发送给模型的日志正文片段，系统提示词和用户指令另有固定开销。
pub(crate) const LOG_AI_ANALYSIS_CHUNK_BYTE_BUDGET: usize = 500 * 1024;

/// 每轮自动发送的最大分块数。
///
/// 业务意图：
/// - 超大目录或超大日志可能拆出数百批请求，必须给用户一个成本和耗时刹车点。
/// - 窗口暂停后允许继续下一轮，既满足完整分析，又避免一次点击产生不可控调用量。
pub(crate) const LOG_AI_ANALYSIS_BATCH_LIMIT: usize = 80;

/// 单个模型请求最多尝试次数。
///
/// 业务意图：
/// - 智能分析可能连续发送多批日志，远程兼容接口偶发网络抖动或读取中断时不应直接终止整次分析。
/// - 这里包含第一次请求，因此实际最多重试 2 次；用户点击停止时不会进入重试。
const LOG_AI_ANALYSIS_MODEL_REQUEST_MAX_ATTEMPTS: usize = 3;

/// 模型请求重试基础等待时间。
///
/// 业务意图：
/// - 简单线性退避可以给本地模型或远程网关短暂恢复时间，同时不让用户长时间看不到进展。
const LOG_AI_ANALYSIS_RETRY_BASE_DELAY_MS: u64 = 800;

/// 单个证据片段最多展示的真实日志行数。
///
/// 业务意图：
/// - 模型可能输出很宽的行号范围，本地只展示最靠近问题的前几行，避免最终汇总窗口被大段日志淹没。
const LOG_AI_ANALYSIS_MAX_EVIDENCE_LINES: usize = 20;

/// 单个证据片段最多保留的 UTF-8 字节数。
///
/// 业务意图：
/// - 超长日志行或连续堆栈会让 UI 和 Markdown 渲染压力变大，因此片段需要在本地做硬上限裁剪。
const LOG_AI_ANALYSIS_MAX_EVIDENCE_BYTES: usize = 12 * 1024;

/// 单个批次最多提取的证据片段数量。
///
/// 业务意图：
/// - 批次回复中可能反复引用同一类问题的许多行，限制数量可以避免最终汇总证据区过载。
const LOG_AI_ANALYSIS_MAX_EVIDENCE_SNIPPETS_PER_CHUNK: usize = 24;

/// 日志智能分析系统提示词。
///
/// 业务意图：
/// - 让模型以专业排障视角输出结论，避免寒暄、泛泛建议和没有证据的猜测。
/// - 提示词固定为中文，保证当前中文界面下的结果可直接阅读。
pub(crate) const LOG_AI_ANALYSIS_SYSTEM_PROMPT: &str = r#"你是 LogClinic 的日志诊断专家，负责判断日志中是否存在性能问题、异常、错误模式或潜在风险。

分析范围包括但不限于：JVM/GC/线程阻塞、CPU/内存/磁盘/网络资源、HTTP/RPC 慢请求、数据库慢查询、连接池耗尽、缓存异常、队列堆积、超时、重试风暴、分布式调用失败、启动失败、配置错误和业务异常。

必须遵守：
1. 只基于用户提供的日志证据分析，不臆测未出现的事实。
2. 结论优先，输出中文，简洁专业，不寒暄，不解释方法论。
3. 每个问题必须给出证据行号或发送范围；没有证据时明确写“证据不足”。
4. 输出结构固定为：结论、严重级别、证据、可能原因、建议、需补充信息。
5. 严重级别只能使用：P0 致命、P1 严重、P2 中等、P3 提示、未发现明确问题。
6. 未发现问题时直接说明“当前片段未发现明确异常或性能问题”，不要凑建议。
7. 不要复述整段日志，不要输出与排障无关的内容。"#;

/// 右键智能分析的单个目标日志。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogAiAnalysisTarget {
    /// 日志读取来源。
    ///
    /// 业务意图：
    /// - 继续复用日志查看器已有来源模型，保证压缩包、嵌套压缩包和物化成员读取语义一致。
    pub(crate) source: LogFileSource,
    /// 用户可见名称。
    ///
    /// 边界条件：
    /// - 名称只用于窗口标题、范围展示和提示词，不参与文件读取。
    pub(crate) display_name: String,
}

impl LogAiAnalysisTarget {
    /// 从日志来源构造默认分析目标。
    pub(crate) fn from_source(source: LogFileSource) -> Self {
        let display_name = source.display_name();
        Self {
            source,
            display_name,
        }
    }
}

/// 单个发送分块的日志范围。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogAiAnalysisChunkRange {
    /// 目标日志在本次任务中的序号。
    pub(crate) target_index: usize,
    /// 用户可见文件名。
    pub(crate) source_name: String,
    /// 1 基起始行号。
    pub(crate) start_line: usize,
    /// 1 基结束行号。
    pub(crate) end_line: usize,
    /// 本批发送正文 UTF-8 字节数。
    pub(crate) byte_count: usize,
}

impl LogAiAnalysisChunkRange {
    /// 返回窗口和提示词中使用的稳定范围文案。
    pub(crate) fn label(&self) -> String {
        format!(
            "{} · 行 {}-{} · 大小 {}",
            self.source_name,
            self.start_line,
            self.end_line,
            format_log_ai_analysis_bytes(self.byte_count)
        )
    }
}

/// 把内部字节数转换成用户可读的日志分块大小。
///
/// 业务意图：
/// - 智能分析按 500KB 分批发送，窗口和提示词展示“大小”比“字符数”更贴近真实上下文预算。
/// - 小于 1KB 的空文件或极小片段保留 B 单位，避免显示为 `0.0 KB` 造成误解。
fn format_log_ai_analysis_bytes(byte_count: usize) -> String {
    if byte_count < 1024 {
        format!("{byte_count} B")
    } else {
        format!("{:.1} KB", byte_count as f64 / 1024.0)
    }
}

/// 一个准备发送给模型的日志分块。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogAiAnalysisChunk {
    /// 分块对应的源日志范围。
    pub(crate) range: LogAiAnalysisChunkRange,
    /// 带行号前缀的日志正文。
    ///
    /// 业务意图：
    /// - 行号前缀让模型输出证据时可以引用真实行号，不需要 UI 再反向映射相对行。
    pub(crate) text: String,
}

/// 批次分析结果对应的真实日志证据片段。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogAiAnalysisEvidenceSnippet {
    /// 在本次智能分析内稳定展示和引用的证据 ID。
    ///
    /// 业务意图：
    /// - 最终汇总提示词只把 ID、文件和行号发给模型，不发送原始日志正文；UI 再用 ID 关联本地原文片段。
    pub(crate) id: String,
    /// 证据来源的 0 基批次序号。
    pub(crate) chunk_index: usize,
    /// 用户可见文件名。
    pub(crate) source_name: String,
    /// 证据起始行号，1 基。
    pub(crate) start_line: usize,
    /// 证据结束行号，1 基。
    pub(crate) end_line: usize,
    /// 本地保留片段正文的 UTF-8 字节数。
    pub(crate) byte_count: usize,
    /// 从已发送批次正文中截取的真实日志文本，保留行号前缀。
    pub(crate) text: String,
}

impl LogAiAnalysisEvidenceSnippet {
    /// 返回窗口中展示的证据范围文案。
    pub(crate) fn label(&self) -> String {
        format!(
            "{} · {} · 行 {}-{} · {}",
            self.id,
            self.source_name,
            self.start_line,
            self.end_line,
            format_log_ai_analysis_bytes(self.byte_count)
        )
    }
}

/// 已完成的分块摘要。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogAiAnalysisCompletedChunk {
    /// 分块范围。
    pub(crate) range: LogAiAnalysisChunkRange,
    /// 模型对该范围的正式分析结果。
    pub(crate) response: String,
    /// 从该批次回复行号中本地截取出的原始日志证据。
    pub(crate) evidence_snippets: Vec<LogAiAnalysisEvidenceSnippet>,
}

/// 智能分析完成后的追问历史。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LogAiAnalysisFollowUpExchange {
    /// 用户追问内容。
    pub(crate) question: String,
    /// 模型围绕当前分析结论给出的回答。
    pub(crate) answer: String,
}

/// 日志智能分析后台流式事件。
///
/// 业务意图：
/// - 后台线程不能直接触碰 GPUI 状态，只能把准备进度、推理增量和回复增量发回窗口实体。
pub(crate) enum LogAiAnalysisRuntimeEvent {
    /// 已完成日志读取和分块。
    Prepared {
        /// 所有分块范围，不包含正文，避免 UI 常驻完整日志内容。
        ranges: Vec<LogAiAnalysisChunkRange>,
        /// 本轮从第几个分块开始发送。
        start_index: usize,
    },
    /// 当前分块开始请求模型。
    ChunkStarted {
        /// 0 基分块序号。
        index: usize,
    },
    /// 当前分块请求失败后准备自动重试。
    ChunkRetrying {
        /// 0 基分块序号。
        index: usize,
        /// 下一次尝试序号，1 基。
        attempt: usize,
        /// 最大尝试次数。
        max_attempts: usize,
        /// 上一次失败原因。
        message: String,
    },
    /// 当前分块收到显式推理增量。
    ChunkReasoningDelta {
        /// 0 基分块序号。
        index: usize,
        /// 推理增量文本。
        delta: String,
    },
    /// 当前分块收到正式回复增量。
    ChunkDelta {
        /// 0 基分块序号。
        index: usize,
        /// 回复增量文本。
        delta: String,
    },
    /// 当前分块完成。
    ChunkFinished {
        /// 0 基分块序号。
        index: usize,
        /// 当前批次完成后本地抽取出的原始日志证据片段。
        evidence_snippets: Vec<LogAiAnalysisEvidenceSnippet>,
    },
    /// 最终汇总请求开始。
    SummaryStarted,
    /// 最终汇总请求失败后准备自动重试。
    SummaryRetrying {
        /// 下一次尝试序号，1 基。
        attempt: usize,
        /// 最大尝试次数。
        max_attempts: usize,
        /// 上一次失败原因。
        message: String,
    },
    /// 最终汇总收到显式推理增量。
    SummaryReasoningDelta(String),
    /// 最终汇总收到正式回复增量。
    SummaryDelta(String),
    /// 最终汇总完成。
    SummaryFinished,
    /// 追问请求开始。
    FollowUpStarted {
        /// 0 基追问序号。
        index: usize,
    },
    /// 追问请求失败后准备自动重试。
    FollowUpRetrying {
        /// 0 基追问序号。
        index: usize,
        /// 下一次尝试序号，1 基。
        attempt: usize,
        /// 最大尝试次数。
        max_attempts: usize,
        /// 上一次失败原因。
        message: String,
    },
    /// 追问收到显式推理增量。
    FollowUpReasoningDelta {
        /// 0 基追问序号。
        index: usize,
        /// 推理增量文本。
        delta: String,
    },
    /// 追问收到正式回复增量。
    FollowUpDelta {
        /// 0 基追问序号。
        index: usize,
        /// 回复增量文本。
        delta: String,
    },
    /// 追问完成。
    FollowUpFinished {
        /// 0 基追问序号。
        index: usize,
    },
    /// 追问失败。
    FollowUpFailed {
        /// 0 基追问序号。
        index: usize,
        /// 用户可见失败原因。
        message: String,
    },
    /// 到达批次数上限，等待用户确认继续。
    Paused {
        /// 下一轮应从该分块继续。
        next_index: usize,
        /// 全部分块数量。
        total: usize,
        /// 面向用户的暂停说明。
        message: String,
    },
    /// 全部分析完成。
    Done,
    /// 用户停止任务。
    Stopped,
    /// 读取、请求或解析失败。
    Error(String),
}

/// 从日志树选择中递归收集可分析文件来源。
///
/// 业务意图：
/// - 右键目录或压缩包时，用户预期分析其下所有日志；右键文件时只分析文件本身。
/// - 多选父子节点时按树顺序去重，避免同一个日志被重复发送给模型。
pub(crate) fn collect_log_ai_analysis_sources_from_tree(
    tree: &LoadedLogTree,
    selected_node_ids: &HashSet<usize>,
) -> Vec<LogFileSource> {
    let mut sources = Vec::new();
    let mut seen_keys = HashSet::new();
    let mut covered_depth: Option<usize> = None;

    for (index, row) in tree.rows.iter().enumerate() {
        if let Some(depth) = covered_depth {
            if row.depth > depth {
                continue;
            }
            covered_depth = None;
        }

        if !selected_node_ids.contains(&row.id) {
            continue;
        }

        push_log_ai_analysis_row_source(row, &mut sources, &mut seen_keys);
        if row.has_children {
            collect_descendant_log_ai_analysis_sources(
                &tree.rows,
                index,
                row.depth,
                &mut sources,
                &mut seen_keys,
            );
            covered_depth = Some(row.depth);
        }
    }

    sources
}

/// 执行一次日志智能分析后台任务。
///
/// 业务意图：
/// - 该函数运行在后台线程，串行完成读取、分块和模型请求，避免并发请求压垮本地模型或远程额度。
/// - `start_chunk_index` 用于用户在批次数上限暂停后继续下一轮。
pub(crate) fn run_log_ai_analysis(
    profile: ModelProfile,
    targets: Vec<LogAiAnalysisTarget>,
    start_chunk_index: usize,
    previous_summaries: Vec<LogAiAnalysisCompletedChunk>,
    cancel: Arc<AtomicBool>,
    sender: mpsc::Sender<LogAiAnalysisRuntimeEvent>,
) {
    let result = (|| -> Result<(), String> {
        let prepared = build_log_ai_analysis_chunks_for_targets(
            &targets,
            start_chunk_index,
            LOG_AI_ANALYSIS_BATCH_LIMIT,
        )?;
        if prepared.ranges.is_empty() {
            return Err("智能分析失败：没有可发送的日志内容".to_string());
        }

        if sender
            .send(LogAiAnalysisRuntimeEvent::Prepared {
                ranges: prepared.ranges.clone(),
                start_index: start_chunk_index,
            })
            .is_err()
        {
            return Ok(());
        }

        let mut completed_summaries = previous_summaries;

        for (index, chunk) in prepared.chunks_to_send {
            if cancel.load(Ordering::Relaxed) {
                let _ = sender.send(LogAiAnalysisRuntimeEvent::Stopped);
                return Ok(());
            }
            if sender
                .send(LogAiAnalysisRuntimeEvent::ChunkStarted { index })
                .is_err()
            {
                return Ok(());
            }

            let messages = log_ai_analysis_chunk_messages(&chunk, index + 1, prepared.ranges.len());
            let Some(response) = stream_log_ai_analysis_chunk_with_retry(
                profile.clone(),
                messages,
                index,
                cancel.clone(),
                &sender,
            ) else {
                return Ok(());
            };
            let evidence_snippets =
                extract_log_ai_analysis_evidence_snippets(index, &chunk, &response);
            if sender
                .send(LogAiAnalysisRuntimeEvent::ChunkFinished {
                    index,
                    evidence_snippets: evidence_snippets.clone(),
                })
                .is_err()
            {
                return Ok(());
            }
            completed_summaries.push(LogAiAnalysisCompletedChunk {
                range: chunk.range.clone(),
                response,
                evidence_snippets,
            });
        }

        let next_index = start_chunk_index.saturating_add(LOG_AI_ANALYSIS_BATCH_LIMIT);
        if next_index < prepared.ranges.len() {
            let message = format!(
                "已完成 {} / {} 批，达到每轮 {} 批上限；点击继续可发送下一轮。",
                next_index,
                prepared.ranges.len(),
                LOG_AI_ANALYSIS_BATCH_LIMIT
            );
            let _ = sender.send(LogAiAnalysisRuntimeEvent::Paused {
                next_index,
                total: prepared.ranges.len(),
                message,
            });
            return Ok(());
        }

        if prepared.ranges.len() > 1
            && !stream_log_ai_analysis_summary(profile, completed_summaries, cancel, &sender)
        {
            return Ok(());
        }
        let _ = sender.send(LogAiAnalysisRuntimeEvent::Done);
        Ok(())
    })();

    if let Err(message) = result {
        let _ = sender.send(LogAiAnalysisRuntimeEvent::Error(message));
    }
}

/// 为多个分析目标构建分块范围，并只保留当前轮需要发送的正文。
fn build_log_ai_analysis_chunks_for_targets(
    targets: &[LogAiAnalysisTarget],
    send_start: usize,
    send_limit: usize,
) -> Result<PreparedLogAiAnalysisChunks, String> {
    let mut collector = LogAiAnalysisChunkCollector::new(send_start, send_limit);
    for (target_index, target) in targets.iter().enumerate() {
        build_log_ai_analysis_chunks_for_target(target_index, target, &mut collector)?;
    }
    Ok(collector.finish())
}

/// 为单个目标读取日志并构建分块。
fn build_log_ai_analysis_chunks_for_target(
    target_index: usize,
    target: &LogAiAnalysisTarget,
    collector: &mut LogAiAnalysisChunkCollector,
) -> Result<(), String> {
    let open_result = open_log_source_for_tab(
        target.source.clone(),
        EncodingChoice::Auto,
        &target.display_name,
    )
    .map_err(|error| format!("智能分析失败：无法读取 {}：{}", target.display_name, error))?;
    match open_result {
        LargeLogOpenResult::InMemoryReady { document, .. } => {
            build_log_ai_analysis_chunks_from_lines_into_collector(
                collector,
                target_index,
                &target.display_name,
                document.lines.iter().cloned(),
                LOG_AI_ANALYSIS_CHUNK_BYTE_BUDGET,
            );
            Ok(())
        }
        LargeLogOpenResult::InMemoryDecodeFailed { message, .. } => Err(format!(
            "智能分析失败：{} 解码失败：{}",
            target.display_name, message
        )),
        LargeLogOpenResult::PagedReady { document } => {
            let mut builder = LogAiAnalysisChunkBuilder::new(
                collector,
                target_index,
                &target.display_name,
                LOG_AI_ANALYSIS_CHUNK_BYTE_BUDGET,
            );
            for line_number in 0..document.line_count() {
                if let Some(line) = document
                    .read_line(line_number)
                    .map_err(|error| format!("智能分析失败：读取分页日志失败：{error}"))?
                {
                    // 分页日志可能达到 GB 级，必须边读取边分块，不能把所有行先收集到内存。
                    builder.push_line(line_number + 1, &line.text);
                }
            }
            if let Some(temp_path) = document.materialized_temp_path.clone() {
                cleanup_materialized_file(&temp_path);
            }
            builder.finish();
            Ok(())
        }
    }
}

/// 将日志行构造成满足字节预算的发送分块。
///
/// 边界条件：
/// - 空日志仍构造一个空分块，让模型能明确返回未发现问题，而不是 UI 静默失败。
/// - 单行超过预算时按 UTF-8 字符边界切段，同一真实行号会出现在多个分块中，保证请求不会越界。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn build_log_ai_analysis_chunks_from_lines<I>(
    target_index: usize,
    source_name: &str,
    lines: I,
    budget: usize,
) -> Vec<LogAiAnalysisChunk>
where
    I: IntoIterator<Item = String>,
{
    let mut collector = LogAiAnalysisChunkCollector::new(0, usize::MAX);
    build_log_ai_analysis_chunks_from_lines_into_collector(
        &mut collector,
        target_index,
        source_name,
        lines,
        budget,
    );
    collector
        .finish()
        .chunks_to_send
        .into_iter()
        .map(|(_, chunk)| chunk)
        .collect()
}

/// 将日志行增量写入分块收集器。
fn build_log_ai_analysis_chunks_from_lines_into_collector<I>(
    collector: &mut LogAiAnalysisChunkCollector,
    target_index: usize,
    source_name: &str,
    lines: I,
    budget: usize,
) where
    I: IntoIterator<Item = String>,
{
    let mut builder = LogAiAnalysisChunkBuilder::new(collector, target_index, source_name, budget);
    for (line_index, line) in lines.into_iter().enumerate() {
        builder.push_line(line_index + 1, &line);
    }
    builder.finish();
}

/// 构造单批日志分析消息。
pub(crate) fn log_ai_analysis_chunk_messages(
    chunk: &LogAiAnalysisChunk,
    chunk_number: usize,
    total_chunks: usize,
) -> Vec<OpenAiCompatibleChatMessage> {
    vec![
        OpenAiCompatibleChatMessage::new("system", LOG_AI_ANALYSIS_SYSTEM_PROMPT),
        OpenAiCompatibleChatMessage::new(
            "user",
            format!(
                "请分析以下日志片段是否存在性能问题或异常。\n\
                 批次：{chunk_number}/{total_chunks}\n\
                 范围：{}\n\n\
                 要求：只分析当前范围；每个问题的证据必须使用“证据：行 x-y”或“证据：Lx-Ly”给出真实行号；不要复制整段日志；输出可被最终汇总合并的精简结论。\n\n\
                 日志内容：\n```text\n{}\n```",
                chunk.range.label(),
                chunk.text
            ),
        ),
    ]
}

/// 构造最终汇总消息。
pub(crate) fn log_ai_analysis_summary_messages(
    completed_chunks: &[LogAiAnalysisCompletedChunk],
) -> Vec<OpenAiCompatibleChatMessage> {
    let mut summaries = String::new();
    for (index, chunk) in completed_chunks.iter().enumerate() {
        summaries.push_str(&format!(
            "## 批次 {}：{}\n{}\n\n",
            index + 1,
            chunk.range.label(),
            chunk.response.trim()
        ));
    }
    let evidence_metadata = log_ai_analysis_evidence_metadata(completed_chunks);

    vec![
        OpenAiCompatibleChatMessage::new("system", LOG_AI_ANALYSIS_SYSTEM_PROMPT),
        OpenAiCompatibleChatMessage::new(
            "user",
            format!(
                "下面是各日志分块的分析结果，请生成最终汇总。\n\n\
                 要求：去重合并相同问题；按严重级别从高到低排序；每个问题必须引用可用的证据 ID；不要回传、复述或改写原始日志；没有明确问题时直接给出未发现明确问题。\n\n\
                 可引用证据 ID：\n{}\n\n\
                 分块分析结果：\n{}",
                evidence_metadata, summaries
            ),
        ),
    ]
}

/// 构造智能分析完成后的追问消息。
///
/// 业务意图：
/// - 追问只围绕已经生成的结论和本地展示的证据片段，不重新发送完整日志批次正文，控制上下文和敏感信息外发范围。
pub(crate) fn log_ai_analysis_follow_up_messages(
    completed_chunks: &[LogAiAnalysisCompletedChunk],
    summary_response: &str,
    visible_snippets: &[LogAiAnalysisEvidenceSnippet],
    history: &[LogAiAnalysisFollowUpExchange],
    question: &str,
) -> Vec<OpenAiCompatibleChatMessage> {
    let mut messages = vec![
        OpenAiCompatibleChatMessage::new("system", LOG_AI_ANALYSIS_SYSTEM_PROMPT),
        OpenAiCompatibleChatMessage::new(
            "user",
            format!(
                "用户正在基于一次已完成的日志智能分析继续追问。\n\n\
                 回答要求：只基于下方最终结论、批次摘要和已展示证据片段回答；不要假设未出现的日志事实；中文简洁专业；如果证据不足，直接说明还需要哪些日志。\n\n\
                 最终结论：\n{}\n\n\
                 批次分析摘要：\n{}\n\n\
                 已展示原日志片段：\n{}",
                empty_log_ai_analysis_text(summary_response, "（无最终汇总结论）"),
                log_ai_analysis_completed_chunk_summary_text(completed_chunks),
                log_ai_analysis_visible_evidence_text(visible_snippets),
            ),
        ),
    ];

    for exchange in history {
        messages.push(OpenAiCompatibleChatMessage::new(
            "user",
            format!("追问：{}", exchange.question),
        ));
        messages.push(OpenAiCompatibleChatMessage::new(
            "assistant",
            exchange.answer.clone(),
        ));
    }
    messages.push(OpenAiCompatibleChatMessage::new(
        "user",
        format!("追问：{question}"),
    ));
    messages
}

/// 汇总所有已完成批次中的本地证据片段。
///
/// 业务意图：
/// - 最终汇总 UI 需要根据模型回复中引用的证据 ID 显示原日志片段；该函数提供稳定顺序的片段列表。
pub(crate) fn all_log_ai_analysis_evidence_snippets(
    completed_chunks: &[LogAiAnalysisCompletedChunk],
) -> Vec<LogAiAnalysisEvidenceSnippet> {
    completed_chunks
        .iter()
        .flat_map(|chunk| chunk.evidence_snippets.iter().cloned())
        .collect()
}

/// 根据最终汇总回复选择应展示的原日志片段。
///
/// 业务意图：
/// - 模型按提示引用证据 ID 时，UI 只展示被引用的片段，避免无关日志噪声。
/// - 如果模型没有引用任何 ID，则回退展示全部已抽取证据，保证用户仍可看到本地可用原文。
pub(crate) fn select_log_ai_analysis_summary_evidence_snippets(
    summary_response: &str,
    snippets: &[LogAiAnalysisEvidenceSnippet],
) -> Vec<LogAiAnalysisEvidenceSnippet> {
    let referenced = snippets
        .iter()
        .filter(|snippet| {
            log_ai_analysis_text_references_evidence_id(summary_response, &snippet.id)
        })
        .cloned()
        .collect::<Vec<_>>();
    if referenced.is_empty() {
        snippets.to_vec()
    } else {
        referenced
    }
}

/// 返回文本中引用到的证据片段。
pub(crate) fn referenced_log_ai_analysis_evidence_snippets(
    text: &str,
    snippets: &[LogAiAnalysisEvidenceSnippet],
) -> Vec<LogAiAnalysisEvidenceSnippet> {
    snippets
        .iter()
        .filter(|snippet| log_ai_analysis_text_references_evidence_id(text, &snippet.id))
        .cloned()
        .collect()
}

/// 返回文本中引用到的证据片段，优先按证据 ID 匹配，缺少 ID 时回退按行号范围匹配。
///
/// 业务意图：
/// - 多批次最终汇总会收到证据 ID，因此应优先使用 ID 做精确定位，避免行号在不同文件或批次中冲突。
/// - 单批次结果不会额外走汇总请求，模型通常只输出 `证据：行 x-y`；此时需要按本地已抽取片段的行号插入原日志。
pub(crate) fn referenced_log_ai_analysis_evidence_snippets_for_text(
    text: &str,
    snippets: &[LogAiAnalysisEvidenceSnippet],
) -> Vec<LogAiAnalysisEvidenceSnippet> {
    let by_id = referenced_log_ai_analysis_evidence_snippets(text, snippets);
    if !by_id.is_empty() {
        return by_id;
    }
    referenced_log_ai_analysis_evidence_snippets_by_line_range(text, snippets)
}

/// 根据文本中的行号引用选择证据片段。
///
/// 边界条件：
/// - 行号只作为证据 ID 缺失时的兜底；多个文件行号可能重复，因此不能覆盖 ID 匹配结果。
/// - 只要引用范围与片段范围有交集即可展示，兼容模型输出 `行 10-20` 而本地片段因上限裁剪为 `行 10-29` 的情况。
fn referenced_log_ai_analysis_evidence_snippets_by_line_range(
    text: &str,
    snippets: &[LogAiAnalysisEvidenceSnippet],
) -> Vec<LogAiAnalysisEvidenceSnippet> {
    let ranges = parse_log_ai_analysis_line_ranges(text);
    if ranges.is_empty() {
        return Vec::new();
    }
    snippets
        .iter()
        .filter(|snippet| {
            ranges.iter().any(|(start_line, end_line)| {
                log_ai_analysis_line_ranges_overlap(
                    *start_line,
                    *end_line,
                    snippet.start_line,
                    snippet.end_line,
                )
            })
        })
        .cloned()
        .collect()
}

/// 判断两个闭区间行号范围是否相交。
fn log_ai_analysis_line_ranges_overlap(
    left_start: usize,
    left_end: usize,
    right_start: usize,
    right_end: usize,
) -> bool {
    left_start <= right_end && right_start <= left_end
}

/// 后台执行一次智能分析追问。
pub(crate) fn run_log_ai_analysis_follow_up(
    profile: ModelProfile,
    completed_chunks: Vec<LogAiAnalysisCompletedChunk>,
    summary_response: String,
    visible_snippets: Vec<LogAiAnalysisEvidenceSnippet>,
    history: Vec<LogAiAnalysisFollowUpExchange>,
    question: String,
    index: usize,
    cancel: Arc<AtomicBool>,
    sender: mpsc::Sender<LogAiAnalysisRuntimeEvent>,
) {
    if sender
        .send(LogAiAnalysisRuntimeEvent::FollowUpStarted { index })
        .is_err()
    {
        return;
    }
    let messages = log_ai_analysis_follow_up_messages(
        &completed_chunks,
        &summary_response,
        &visible_snippets,
        &history,
        &question,
    );

    for attempt in 1..=LOG_AI_ANALYSIS_MODEL_REQUEST_MAX_ATTEMPTS {
        let outcome = stream_log_ai_analysis_messages_once(
            profile.clone(),
            messages.clone(),
            cancel.clone(),
            |event| match event {
                AiChatStreamEvent::Delta(delta) => sender
                    .send(LogAiAnalysisRuntimeEvent::FollowUpDelta { index, delta })
                    .is_ok(),
                AiChatStreamEvent::ReasoningDelta(delta) => sender
                    .send(LogAiAnalysisRuntimeEvent::FollowUpReasoningDelta { index, delta })
                    .is_ok(),
                AiChatStreamEvent::Done
                | AiChatStreamEvent::Stopped
                | AiChatStreamEvent::Error(_) => true,
            },
        );
        match outcome {
            LogAiAnalysisStreamAttemptOutcome::Finished { .. } => {
                let _ = sender.send(LogAiAnalysisRuntimeEvent::FollowUpFinished { index });
                return;
            }
            LogAiAnalysisStreamAttemptOutcome::Stopped => {
                let _ = sender.send(LogAiAnalysisRuntimeEvent::Stopped);
                return;
            }
            LogAiAnalysisStreamAttemptOutcome::ReceiverClosed => return,
            LogAiAnalysisStreamAttemptOutcome::Failed(message) => {
                if !should_retry_log_ai_analysis_request(&message)
                    || attempt >= LOG_AI_ANALYSIS_MODEL_REQUEST_MAX_ATTEMPTS
                {
                    let _ = sender.send(LogAiAnalysisRuntimeEvent::FollowUpFailed {
                        index,
                        message: final_log_ai_analysis_request_error(
                            "追问回答生成",
                            attempt,
                            &message,
                        ),
                    });
                    return;
                }
                let next_attempt = attempt.saturating_add(1);
                if sender
                    .send(LogAiAnalysisRuntimeEvent::FollowUpRetrying {
                        index,
                        attempt: next_attempt,
                        max_attempts: LOG_AI_ANALYSIS_MODEL_REQUEST_MAX_ATTEMPTS,
                        message,
                    })
                    .is_err()
                {
                    return;
                }
                if wait_before_log_ai_analysis_retry(attempt, &cancel, &sender).is_none() {
                    return;
                }
            }
        }
    }
}

/// 带自动重试地流式执行单个日志分块请求。
///
/// 返回值：
/// - `Some(response)` 表示请求最终成功完成，可继续抽取证据并进入下一批。
/// - `None` 表示用户停止、窗口关闭或最终失败事件已发送，调用方应结束后台任务。
fn stream_log_ai_analysis_chunk_with_retry(
    profile: ModelProfile,
    messages: Vec<OpenAiCompatibleChatMessage>,
    index: usize,
    cancel: Arc<AtomicBool>,
    sender: &mpsc::Sender<LogAiAnalysisRuntimeEvent>,
) -> Option<String> {
    for attempt in 1..=LOG_AI_ANALYSIS_MODEL_REQUEST_MAX_ATTEMPTS {
        let outcome = stream_log_ai_analysis_messages_once(
            profile.clone(),
            messages.clone(),
            cancel.clone(),
            |event| match event {
                AiChatStreamEvent::Delta(delta) => sender
                    .send(LogAiAnalysisRuntimeEvent::ChunkDelta { index, delta })
                    .is_ok(),
                AiChatStreamEvent::ReasoningDelta(delta) => sender
                    .send(LogAiAnalysisRuntimeEvent::ChunkReasoningDelta { index, delta })
                    .is_ok(),
                AiChatStreamEvent::Done
                | AiChatStreamEvent::Stopped
                | AiChatStreamEvent::Error(_) => true,
            },
        );
        match outcome {
            LogAiAnalysisStreamAttemptOutcome::Finished { response } => return Some(response),
            LogAiAnalysisStreamAttemptOutcome::Stopped => {
                let _ = sender.send(LogAiAnalysisRuntimeEvent::Stopped);
                return None;
            }
            LogAiAnalysisStreamAttemptOutcome::ReceiverClosed => return None,
            LogAiAnalysisStreamAttemptOutcome::Failed(message) => {
                if !should_retry_log_ai_analysis_request(&message)
                    || attempt >= LOG_AI_ANALYSIS_MODEL_REQUEST_MAX_ATTEMPTS
                {
                    let _ = sender.send(LogAiAnalysisRuntimeEvent::Error(
                        final_log_ai_analysis_request_error("日志分块分析", attempt, &message),
                    ));
                    return None;
                }
                let next_attempt = attempt.saturating_add(1);
                if sender
                    .send(LogAiAnalysisRuntimeEvent::ChunkRetrying {
                        index,
                        attempt: next_attempt,
                        max_attempts: LOG_AI_ANALYSIS_MODEL_REQUEST_MAX_ATTEMPTS,
                        message,
                    })
                    .is_err()
                {
                    return None;
                }
                wait_before_log_ai_analysis_retry(attempt, &cancel, sender)?;
            }
        }
    }
    None
}

/// 单次执行智能分析模型请求，并把输出增量交给调用方转发。
///
/// 关键约束：
/// - 重试时调用方会清空 UI 半截输出，因此这里仅返回正式回复正文用于成功后的证据抽取。
/// - reasoning 只在 UI 中展示，不参与后续提示词或证据解析。
fn stream_log_ai_analysis_messages_once<F>(
    profile: ModelProfile,
    messages: Vec<OpenAiCompatibleChatMessage>,
    cancel: Arc<AtomicBool>,
    mut forward_delta: F,
) -> LogAiAnalysisStreamAttemptOutcome
where
    F: FnMut(AiChatStreamEvent) -> bool,
{
    let mut response = String::new();
    let mut failed = None;
    let mut stopped = false;
    let mut finished = false;
    let mut receiver_closed = false;
    stream_openai_compatible_chat_messages_with_handler(
        profile,
        messages,
        true,
        AiChatReasoningEffort::High,
        cancel,
        |event| match event {
            AiChatStreamEvent::Delta(delta) => {
                response.push_str(&delta);
                let sent = forward_delta(AiChatStreamEvent::Delta(delta));
                receiver_closed = !sent;
                sent
            }
            AiChatStreamEvent::ReasoningDelta(delta) => {
                let sent = forward_delta(AiChatStreamEvent::ReasoningDelta(delta));
                receiver_closed = !sent;
                sent
            }
            AiChatStreamEvent::Done => {
                finished = true;
                true
            }
            AiChatStreamEvent::Stopped => {
                stopped = true;
                false
            }
            AiChatStreamEvent::Error(message) => {
                failed = Some(message);
                false
            }
        },
    );

    if receiver_closed {
        LogAiAnalysisStreamAttemptOutcome::ReceiverClosed
    } else if stopped {
        LogAiAnalysisStreamAttemptOutcome::Stopped
    } else if let Some(message) = failed {
        LogAiAnalysisStreamAttemptOutcome::Failed(message)
    } else if finished {
        LogAiAnalysisStreamAttemptOutcome::Finished { response }
    } else {
        LogAiAnalysisStreamAttemptOutcome::Failed("AI 请求失败：响应未完整结束".to_string())
    }
}

/// 单次模型请求的结果。
enum LogAiAnalysisStreamAttemptOutcome {
    /// 本次请求完整结束。
    Finished {
        /// 本次请求的正式回复正文。
        response: String,
    },
    /// 用户主动停止。
    Stopped,
    /// UI 接收端已关闭。
    ReceiverClosed,
    /// 请求或响应失败。
    Failed(String),
}

/// 判断失败是否值得自动重试。
fn should_retry_log_ai_analysis_request(message: &str) -> bool {
    !matches!(
        message,
        "配置名称不能为空"
            | "Base URL 不能为空"
            | "模型 ID 不能为空"
            | "Base URL 必须以 http:// 或 https:// 开头"
    )
}

/// 构造最终失败文案。
fn final_log_ai_analysis_request_error(scope: &str, attempts: usize, message: &str) -> String {
    if attempts <= 1 {
        format!("{scope}失败：{message}")
    } else {
        format!("{scope}失败：已尝试 {} 次仍失败：{}", attempts, message)
    }
}

/// 等待下一次重试，等待期间响应用户停止。
fn wait_before_log_ai_analysis_retry(
    failed_attempt: usize,
    cancel: &Arc<AtomicBool>,
    sender: &mpsc::Sender<LogAiAnalysisRuntimeEvent>,
) -> Option<()> {
    let total_delay = Duration::from_millis(
        LOG_AI_ANALYSIS_RETRY_BASE_DELAY_MS.saturating_mul(failed_attempt as u64),
    );
    let step = Duration::from_millis(100);
    let mut elapsed = Duration::from_millis(0);
    while elapsed < total_delay {
        if cancel.load(Ordering::Relaxed) {
            let _ = sender.send(LogAiAnalysisRuntimeEvent::Stopped);
            return None;
        }
        let remaining = total_delay.saturating_sub(elapsed);
        let sleep_duration = remaining.min(step);
        thread::sleep(sleep_duration);
        elapsed = elapsed.saturating_add(sleep_duration);
    }
    Some(())
}

/// 构造最终汇总提示词中可发送给模型的证据元数据。
///
/// 关键约束：
/// - 这里绝不包含 `LogAiAnalysisEvidenceSnippet::text`，避免最终汇总请求重复发送原始日志正文。
fn log_ai_analysis_evidence_metadata(completed_chunks: &[LogAiAnalysisCompletedChunk]) -> String {
    let snippets = all_log_ai_analysis_evidence_snippets(completed_chunks);
    if snippets.is_empty() {
        return "（没有可引用证据 ID；请基于分块分析结果中的行号范围输出结论。）".to_string();
    }

    snippets
        .iter()
        .map(|snippet| {
            format!(
                "{}：批次 {}，{}，行 {}-{}，大小 {}",
                snippet.id,
                snippet.chunk_index.saturating_add(1),
                snippet.source_name,
                snippet.start_line,
                snippet.end_line,
                format_log_ai_analysis_bytes(snippet.byte_count)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 构造追问上下文中的批次摘要文本。
fn log_ai_analysis_completed_chunk_summary_text(
    completed_chunks: &[LogAiAnalysisCompletedChunk],
) -> String {
    if completed_chunks.is_empty() {
        return "（无批次摘要）".to_string();
    }
    completed_chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            format!(
                "## 批次 {}：{}\n{}",
                index + 1,
                chunk.range.label(),
                empty_log_ai_analysis_text(&chunk.response, "（无回复）")
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 构造追问上下文中的可见证据片段文本。
fn log_ai_analysis_visible_evidence_text(snippets: &[LogAiAnalysisEvidenceSnippet]) -> String {
    if snippets.is_empty() {
        return "（无可见原日志片段）".to_string();
    }
    snippets
        .iter()
        .map(|snippet| {
            format!(
                "### {}\n```text\n{}\n```",
                snippet.label(),
                snippet.text.trim_end()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 将空文本替换为上下文提示。
fn empty_log_ai_analysis_text<'a>(text: &'a str, fallback: &'a str) -> &'a str {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    }
}

/// 判断最终回复是否精确引用了某个证据 ID。
///
/// 边界条件：
/// - `E1-1` 不能误匹配 `E1-10`，因此 ID 两侧必须是非 ID 字符边界。
fn log_ai_analysis_text_references_evidence_id(text: &str, evidence_id: &str) -> bool {
    text.match_indices(evidence_id).any(|(index, _)| {
        let before = text[..index].chars().next_back();
        let after = text[index + evidence_id.len()..].chars().next();
        is_log_ai_analysis_evidence_id_boundary(before)
            && is_log_ai_analysis_evidence_id_boundary(after)
    })
}

/// 判断字符是否可作为证据 ID 的边界。
fn is_log_ai_analysis_evidence_id_boundary(character: Option<char>) -> bool {
    !matches!(character, Some(value) if value.is_ascii_alphanumeric() || value == '-')
}

/// 从批次回复中的行号引用提取本地原日志证据片段。
///
/// 业务意图：
/// - 模型只负责指出问题和行号，原文由本地从已发送的 `chunk.text` 中截取，避免模型复述时改写日志。
pub(crate) fn extract_log_ai_analysis_evidence_snippets(
    chunk_index: usize,
    chunk: &LogAiAnalysisChunk,
    response: &str,
) -> Vec<LogAiAnalysisEvidenceSnippet> {
    let ranges = parse_log_ai_analysis_evidence_line_ranges(response, &chunk.range);
    let mut snippets = Vec::new();
    for (start_line, end_line) in ranges {
        if snippets.len() >= LOG_AI_ANALYSIS_MAX_EVIDENCE_SNIPPETS_PER_CHUNK {
            break;
        }
        let Some(text) = extract_log_ai_analysis_chunk_text_range(chunk, start_line, end_line)
        else {
            continue;
        };
        let snippet = LogAiAnalysisEvidenceSnippet {
            id: format!("E{}-{}", chunk_index.saturating_add(1), snippets.len() + 1),
            chunk_index,
            source_name: chunk.range.source_name.clone(),
            start_line,
            end_line,
            byte_count: text.len(),
            text,
        };
        snippets.push(snippet);
    }
    snippets
}

/// 从模型回复中解析证据行号范围。
///
/// 支持格式：
/// - `行 12`、`行 12-18`
/// - `第 12 行`、`第 12-18 行`
/// - `L12`、`L12-L18`
fn parse_log_ai_analysis_evidence_line_ranges(
    response: &str,
    chunk_range: &LogAiAnalysisChunkRange,
) -> Vec<(usize, usize)> {
    parse_log_ai_analysis_line_ranges(response)
        .into_iter()
        .filter(|(start_line, end_line)| {
            *start_line >= chunk_range.start_line && *end_line <= chunk_range.end_line
        })
        .collect()
}

/// 从文本中解析所有可识别的行号范围。
///
/// 业务意图：
/// - 批次证据抽取和 UI 内联插入都需要理解模型输出的中文/英文行号写法，集中解析可以避免两套规则漂移。
fn parse_log_ai_analysis_line_ranges(response: &str) -> Vec<(usize, usize)> {
    let Ok(regex) = regex::Regex::new(
        r"(?i)(?:第?\s*(\d{1,9})\s*(?:-|~|至|到)\s*(\d{1,9})\s*行|第?\s*(\d{1,9})\s*行|行\s*(\d{1,9})(?:\s*(?:-|~|至|到)\s*(\d{1,9}))?|L\s*(\d{1,9})(?:\s*(?:-|~)\s*L?\s*(\d{1,9}))?)",
    ) else {
        return Vec::new();
    };

    let mut seen = BTreeSet::new();
    let mut ranges = Vec::new();
    for captures in regex.captures_iter(response) {
        let Some((raw_start_line, raw_end_line)) = captured_log_ai_analysis_line_range(&captures)
        else {
            continue;
        };
        let start_line = raw_start_line.min(raw_end_line);
        let end_line = raw_start_line.max(raw_end_line);
        let clipped_end_line = end_line.min(
            start_line
                .saturating_add(LOG_AI_ANALYSIS_MAX_EVIDENCE_LINES)
                .saturating_sub(1),
        );
        if seen.insert((start_line, clipped_end_line)) {
            ranges.push((start_line, clipped_end_line));
        }
    }
    ranges
}

/// 从正则捕获组中读取一个行号范围。
fn captured_log_ai_analysis_line_range(captures: &regex::Captures<'_>) -> Option<(usize, usize)> {
    if let Some(start_line) = captured_log_ai_analysis_line_number(captures, 1) {
        return Some((
            start_line,
            captured_log_ai_analysis_line_number(captures, 2).unwrap_or(start_line),
        ));
    }
    if let Some(start_line) = captured_log_ai_analysis_line_number(captures, 3) {
        return Some((start_line, start_line));
    }
    if let Some(start_line) = captured_log_ai_analysis_line_number(captures, 4) {
        return Some((
            start_line,
            captured_log_ai_analysis_line_number(captures, 5).unwrap_or(start_line),
        ));
    }
    if let Some(start_line) = captured_log_ai_analysis_line_number(captures, 6) {
        return Some((
            start_line,
            captured_log_ai_analysis_line_number(captures, 7).unwrap_or(start_line),
        ));
    }
    None
}

/// 从正则捕获组中安全解析正整数行号。
fn captured_log_ai_analysis_line_number(
    captures: &regex::Captures<'_>,
    index: usize,
) -> Option<usize> {
    captures
        .get(index)
        .and_then(|capture| capture.as_str().parse::<usize>().ok())
}

/// 从带行号前缀的批次正文中截取原始日志片段。
fn extract_log_ai_analysis_chunk_text_range(
    chunk: &LogAiAnalysisChunk,
    start_line: usize,
    end_line: usize,
) -> Option<String> {
    let mut text = String::new();
    for raw_line in chunk.text.lines() {
        let Some((line_number, _line_text)) = parse_log_ai_analysis_prefixed_line(raw_line) else {
            continue;
        };
        if line_number < start_line || line_number > end_line {
            continue;
        }
        text.push_str(raw_line);
        text.push('\n');
    }
    if text.is_empty() {
        return None;
    }
    Some(truncate_log_ai_analysis_snippet_text(&text))
}

/// 解析智能分析发送正文中的 `行号: 内容` 前缀。
fn parse_log_ai_analysis_prefixed_line(line: &str) -> Option<(usize, &str)> {
    let (line_number, line_text) = line.split_once(": ")?;
    let line_number = line_number.trim().parse::<usize>().ok()?;
    Some((line_number, line_text))
}

/// 按 UTF-8 边界截断证据片段正文。
fn truncate_log_ai_analysis_snippet_text(text: &str) -> String {
    if text.len() <= LOG_AI_ANALYSIS_MAX_EVIDENCE_BYTES {
        return text.to_string();
    }
    let marker = "\n...（片段已截断）";
    let budget = LOG_AI_ANALYSIS_MAX_EVIDENCE_BYTES.saturating_sub(marker.len());
    let mut end = 0;
    for (index, character) in text.char_indices() {
        let next_end = index.saturating_add(character.len_utf8());
        if next_end > budget {
            break;
        }
        end = next_end;
    }
    format!("{}{}", &text[..end], marker)
}

/// 流式执行最终汇总。
fn stream_log_ai_analysis_summary(
    profile: ModelProfile,
    completed_summaries: Vec<LogAiAnalysisCompletedChunk>,
    cancel: Arc<AtomicBool>,
    sender: &mpsc::Sender<LogAiAnalysisRuntimeEvent>,
) -> bool {
    if sender
        .send(LogAiAnalysisRuntimeEvent::SummaryStarted)
        .is_err()
    {
        return false;
    }
    let messages = log_ai_analysis_summary_messages(&completed_summaries);
    for attempt in 1..=LOG_AI_ANALYSIS_MODEL_REQUEST_MAX_ATTEMPTS {
        let outcome = stream_log_ai_analysis_messages_once(
            profile.clone(),
            messages.clone(),
            cancel.clone(),
            |event| match event {
                AiChatStreamEvent::Delta(delta) => sender
                    .send(LogAiAnalysisRuntimeEvent::SummaryDelta(delta))
                    .is_ok(),
                AiChatStreamEvent::ReasoningDelta(delta) => sender
                    .send(LogAiAnalysisRuntimeEvent::SummaryReasoningDelta(delta))
                    .is_ok(),
                AiChatStreamEvent::Done
                | AiChatStreamEvent::Stopped
                | AiChatStreamEvent::Error(_) => true,
            },
        );
        match outcome {
            LogAiAnalysisStreamAttemptOutcome::Finished { .. } => {
                return sender
                    .send(LogAiAnalysisRuntimeEvent::SummaryFinished)
                    .is_ok();
            }
            LogAiAnalysisStreamAttemptOutcome::Stopped => {
                let _ = sender.send(LogAiAnalysisRuntimeEvent::Stopped);
                return false;
            }
            LogAiAnalysisStreamAttemptOutcome::ReceiverClosed => return false,
            LogAiAnalysisStreamAttemptOutcome::Failed(message) => {
                if !should_retry_log_ai_analysis_request(&message)
                    || attempt >= LOG_AI_ANALYSIS_MODEL_REQUEST_MAX_ATTEMPTS
                {
                    let _ = sender.send(LogAiAnalysisRuntimeEvent::Error(
                        final_log_ai_analysis_request_error("最终汇总生成", attempt, &message),
                    ));
                    return false;
                }
                let next_attempt = attempt.saturating_add(1);
                if sender
                    .send(LogAiAnalysisRuntimeEvent::SummaryRetrying {
                        attempt: next_attempt,
                        max_attempts: LOG_AI_ANALYSIS_MODEL_REQUEST_MAX_ATTEMPTS,
                        message,
                    })
                    .is_err()
                {
                    return false;
                }
                if wait_before_log_ai_analysis_retry(attempt, &cancel, sender).is_none() {
                    return false;
                }
            }
        }
    }
    false
}

/// 把一个树行自身的可读取来源加入结果。
fn push_log_ai_analysis_row_source(
    row: &LogTreeRow,
    sources: &mut Vec<LogFileSource>,
    seen_keys: &mut HashSet<String>,
) {
    if row.kind != LogTreeEntryKind::File {
        return;
    }
    let Some(source) = row.source.clone() else {
        return;
    };
    let key = source.stable_key();
    if seen_keys.insert(key) {
        sources.push(source);
    }
}

/// 收集某个目录或压缩包节点下的所有文件来源。
fn collect_descendant_log_ai_analysis_sources(
    rows: &[LogTreeRow],
    parent_index: usize,
    parent_depth: usize,
    sources: &mut Vec<LogFileSource>,
    seen_keys: &mut HashSet<String>,
) {
    for row in rows.iter().skip(parent_index + 1) {
        if row.depth <= parent_depth {
            break;
        }
        push_log_ai_analysis_row_source(row, sources, seen_keys);
    }
}

/// 把超长日志行切成不会超过预算的 UTF-8 安全片段。
fn split_log_ai_analysis_line(line: &str, budget: usize) -> Vec<String> {
    let line_budget = budget.max(1).saturating_sub(32).max(1);
    if line.len() <= line_budget {
        return vec![line.to_string()];
    }

    let mut segments = Vec::new();
    let mut current = String::new();
    for character in line.chars() {
        let character_len = character.len_utf8();
        if !current.is_empty() && current.len().saturating_add(character_len) > line_budget {
            segments.push(std::mem::take(&mut current));
        }
        current.push(character);
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

/// 日志分块增量构造器。
///
/// 业务意图：
/// - 普通内存日志和分页日志都走同一套预算规则；分页日志通过逐行 `push_line` 避免一次性保存所有正文。
struct LogAiAnalysisChunkBuilder<'a> {
    /// 全局分块收集器。
    collector: &'a mut LogAiAnalysisChunkCollector,
    /// 当前目标日志序号。
    target_index: usize,
    /// 用户可见文件名。
    source_name: String,
    /// 当前字节预算。
    budget: usize,
    /// 当前缓冲正文。
    current_text: String,
    /// 当前缓冲起始行。
    current_start_line: Option<usize>,
    /// 当前缓冲结束行。
    current_end_line: usize,
    /// 当前缓冲 UTF-8 字节数。
    current_bytes: usize,
    /// 是否已经读取过至少一行。
    saw_line: bool,
}

impl<'a> LogAiAnalysisChunkBuilder<'a> {
    /// 创建增量分块构造器。
    fn new(
        collector: &'a mut LogAiAnalysisChunkCollector,
        target_index: usize,
        source_name: &str,
        budget: usize,
    ) -> Self {
        Self {
            collector,
            target_index,
            source_name: source_name.to_string(),
            budget: budget.max(1),
            current_text: String::new(),
            current_start_line: None,
            current_end_line: 0,
            current_bytes: 0,
            saw_line: false,
        }
    }

    /// 推入一行日志。
    fn push_line(&mut self, line_number: usize, line: &str) {
        self.saw_line = true;
        for segment in split_log_ai_analysis_line(line, self.budget) {
            let prefixed = format!("{line_number}: {segment}\n");
            let prefixed_bytes = prefixed.len();
            if !self.current_text.is_empty()
                && self.current_bytes.saturating_add(prefixed_bytes) > self.budget
            {
                self.flush_current(line_number);
            }

            if self.current_start_line.is_none() {
                self.current_start_line = Some(line_number);
            }
            self.current_end_line = line_number;
            self.current_bytes = self.current_bytes.saturating_add(prefixed_bytes);
            self.current_text.push_str(&prefixed);
        }
    }

    /// 完成构造并返回所有分块。
    fn finish(mut self) {
        if !self.saw_line {
            self.current_text.push_str("1: \n");
            self.current_start_line = Some(1);
            self.current_end_line = 1;
            self.current_bytes = self.current_text.len();
        }

        if !self.current_text.is_empty() {
            self.flush_current(1);
        }
    }

    /// 将当前缓冲刷入结果。
    fn flush_current(&mut self, fallback_line: usize) {
        self.collector.push_chunk(
            self.target_index,
            &self.source_name,
            self.current_start_line.unwrap_or(fallback_line),
            self.current_end_line,
            self.current_bytes,
            std::mem::take(&mut self.current_text),
        );
        self.current_start_line = None;
        self.current_bytes = 0;
    }
}

/// 本次后台构建出的分块结果。
struct PreparedLogAiAnalysisChunks {
    /// 全部分块范围。
    ranges: Vec<LogAiAnalysisChunkRange>,
    /// 本轮需要真实发送的分块正文。
    chunks_to_send: Vec<(usize, LogAiAnalysisChunk)>,
}

/// 分块范围和本轮正文收集器。
///
/// 业务意图：
/// - 大日志只需要保留当前轮即将发送的 `80` 批正文；其它批次只保存范围，用于窗口展示和继续分析。
/// - 这样可以避免为了显示总批次数而把整份超大日志文本常驻内存。
struct LogAiAnalysisChunkCollector {
    /// 全部分块范围。
    ranges: Vec<LogAiAnalysisChunkRange>,
    /// 本轮需要发送的分块正文。
    chunks_to_send: Vec<(usize, LogAiAnalysisChunk)>,
    /// 本轮起始分块序号。
    send_start: usize,
    /// 本轮结束分块序号，右开区间。
    send_end: usize,
}

impl LogAiAnalysisChunkCollector {
    /// 创建分块收集器。
    fn new(send_start: usize, send_limit: usize) -> Self {
        Self {
            ranges: Vec::new(),
            chunks_to_send: Vec::new(),
            send_start,
            send_end: send_start.saturating_add(send_limit),
        }
    }

    /// 写入一个完整分块。
    fn push_chunk(
        &mut self,
        target_index: usize,
        source_name: &str,
        start_line: usize,
        end_line: usize,
        byte_count: usize,
        text: String,
    ) {
        let range = LogAiAnalysisChunkRange {
            target_index,
            source_name: source_name.to_string(),
            start_line,
            end_line,
            byte_count,
        };
        let index = self.ranges.len();
        self.ranges.push(range.clone());
        if index >= self.send_start && index < self.send_end {
            self.chunks_to_send
                .push((index, LogAiAnalysisChunk { range, text }));
        }
    }

    /// 完成收集。
    fn finish(self) -> PreparedLogAiAnalysisChunks {
        PreparedLogAiAnalysisChunks {
            ranges: self.ranges,
            chunks_to_send: self.chunks_to_send,
        }
    }
}

#[cfg(test)]
mod tests {
    //! 日志智能分析纯业务测试。
    //!
    //! 业务意图：
    //! - 分块、提示词和树来源收集直接影响上传给模型的内容范围，必须用单元测试锁定边界行为。

    use std::path::PathBuf;

    use super::*;

    /// 构造测试用本地日志来源。
    fn test_source(name: &str) -> LogFileSource {
        LogFileSource::LocalFile {
            path: PathBuf::from(format!("/tmp/{name}.log")),
        }
    }

    /// 构造测试树行。
    fn test_row(
        id: usize,
        depth: usize,
        kind: LogTreeEntryKind,
        has_children: bool,
        source: Option<LogFileSource>,
    ) -> LogTreeRow {
        LogTreeRow {
            id,
            depth,
            label: format!("row-{id}"),
            kind,
            has_children,
            meta: None,
            error_message: None,
            source,
        }
    }

    /// 验证选中目录时会递归收集子文件，并且父子同时选中不会重复。
    #[test]
    fn 智能分析递归收集目录子日志并去重() {
        let source_a = test_source("a");
        let source_b = test_source("b");
        let tree = LoadedLogTree {
            summary: String::new(),
            rows: vec![
                test_row(1, 0, LogTreeEntryKind::Directory, true, None),
                test_row(2, 1, LogTreeEntryKind::File, false, Some(source_a.clone())),
                test_row(3, 1, LogTreeEntryKind::Directory, true, None),
                test_row(4, 2, LogTreeEntryKind::File, false, Some(source_b.clone())),
            ],
            error_count: 0,
            temporary_paths: Vec::new(),
        };
        let selected = HashSet::from([1, 4]);

        let sources = collect_log_ai_analysis_sources_from_tree(&tree, &selected);

        assert_eq!(sources, vec![source_a, source_b]);
    }

    /// 验证错误节点和目录本身不会被当成可发送日志。
    #[test]
    fn 智能分析忽略没有来源的错误节点() {
        let tree = LoadedLogTree {
            summary: String::new(),
            rows: vec![
                test_row(1, 0, LogTreeEntryKind::Error, false, None),
                test_row(2, 0, LogTreeEntryKind::Directory, false, None),
            ],
            error_count: 1,
            temporary_paths: Vec::new(),
        };
        let selected = HashSet::from([1, 2]);

        let sources = collect_log_ai_analysis_sources_from_tree(&tree, &selected);

        assert!(sources.is_empty());
    }

    /// 验证小日志会保留行号前缀并形成单个分块。
    #[test]
    fn 智能分析小日志单批发送并带行号() {
        let chunks = build_log_ai_analysis_chunks_from_lines(
            0,
            "app.log",
            vec!["ERROR timeout".to_string(), "ok".to_string()],
            100,
        );

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].range.start_line, 1);
        assert_eq!(chunks[0].range.end_line, 2);
        assert!(chunks[0].text.contains("1: ERROR timeout"));
        assert!(chunks[0].text.contains("2: ok"));
    }

    /// 验证大日志会按预算拆分，并且超长单行不会生成超过预算很多的请求正文。
    #[test]
    fn 智能分析大日志和超长单行会拆分() {
        let long_line = "x".repeat(120);
        let chunks = build_log_ai_analysis_chunks_from_lines(0, "big.log", vec![long_line], 50);

        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.range.start_line == 1));
        assert!(chunks.iter().all(|chunk| chunk.range.end_line == 1));
        assert!(chunks.iter().all(|chunk| chunk.range.byte_count <= 55));
    }

    /// 验证默认批次预算符合产品要求的 500KB。
    #[test]
    fn 智能分析默认按五百kb分批() {
        assert_eq!(LOG_AI_ANALYSIS_CHUNK_BYTE_BUDGET, 500 * 1024);
    }

    /// 验证请求体消息包含专业系统提示词和范围说明。
    #[test]
    fn 智能分析请求包含专业系统提示词和日志范围() {
        let chunk = LogAiAnalysisChunk {
            range: LogAiAnalysisChunkRange {
                target_index: 0,
                source_name: "app.log".to_string(),
                start_line: 10,
                end_line: 20,
                byte_count: 42,
            },
            text: "10: ERROR timeout".to_string(),
        };

        let messages = log_ai_analysis_chunk_messages(&chunk, 1, 3);

        assert_eq!(messages[0].role, "system");
        assert!(messages[0].content.contains("日志诊断专家"));
        assert!(messages[1].content.contains("app.log · 行 10-20"));
        assert!(messages[1].content.contains("证据：行 x-y"));
        assert!(messages[1].content.contains("10: ERROR timeout"));
    }

    /// 验证批次回复中的中文行号和 L 行号会提取为本地原日志片段。
    #[test]
    fn 智能分析按模型行号提取原日志证据片段() {
        let chunk = LogAiAnalysisChunk {
            range: LogAiAnalysisChunkRange {
                target_index: 0,
                source_name: "app.log".to_string(),
                start_line: 10,
                end_line: 14,
                byte_count: 120,
            },
            text: "10: INFO start\n11: ERROR timeout\n12: at service.Call\n13: WARN retry\n14: INFO done\n"
                .to_string(),
        };

        let snippets = extract_log_ai_analysis_evidence_snippets(
            2,
            &chunk,
            "结论：存在超时。\n证据：行 11-12\n证据：L13\n证据：行 99",
        );

        assert_eq!(snippets.len(), 2);
        assert_eq!(snippets[0].id, "E3-1");
        assert_eq!(snippets[0].start_line, 11);
        assert_eq!(snippets[0].end_line, 12);
        assert!(snippets[0].text.contains("11: ERROR timeout"));
        assert!(snippets[0].text.contains("12: at service.Call"));
        assert_eq!(snippets[1].id, "E3-2");
        assert!(snippets[1].text.contains("13: WARN retry"));
    }

    /// 验证原日志证据片段会按行数和字节数限制，避免最终结果区域过载。
    #[test]
    fn 智能分析原日志证据片段会限制大小() {
        let lines = (1..=40)
            .map(|line| format!("{line}: {}", "好".repeat(900)))
            .collect::<Vec<_>>()
            .join("\n");
        let chunk = LogAiAnalysisChunk {
            range: LogAiAnalysisChunkRange {
                target_index: 0,
                source_name: "big.log".to_string(),
                start_line: 1,
                end_line: 40,
                byte_count: lines.len(),
            },
            text: format!("{lines}\n"),
        };

        let snippets = extract_log_ai_analysis_evidence_snippets(0, &chunk, "证据：第 1-40 行");

        assert_eq!(snippets.len(), 1);
        assert_eq!(snippets[0].end_line, LOG_AI_ANALYSIS_MAX_EVIDENCE_LINES);
        assert!(snippets[0].text.len() <= LOG_AI_ANALYSIS_MAX_EVIDENCE_BYTES);
        assert!(snippets[0].text.is_char_boundary(snippets[0].text.len()));
        assert!(snippets[0].text.contains("片段已截断"));
    }

    /// 验证最终汇总可按证据 ID 精确筛选片段，且不会把 E1-1 误匹配为 E1-10。
    #[test]
    fn 智能分析最终汇总按证据id筛选片段() {
        let snippets = vec![
            LogAiAnalysisEvidenceSnippet {
                id: "E1-1".to_string(),
                chunk_index: 0,
                source_name: "app.log".to_string(),
                start_line: 1,
                end_line: 1,
                byte_count: 8,
                text: "1: error".to_string(),
            },
            LogAiAnalysisEvidenceSnippet {
                id: "E1-10".to_string(),
                chunk_index: 0,
                source_name: "app.log".to_string(),
                start_line: 10,
                end_line: 10,
                byte_count: 9,
                text: "10: warn".to_string(),
            },
        ];

        let selected = select_log_ai_analysis_summary_evidence_snippets("引用 E1-1", &snippets);
        assert_eq!(selected, vec![snippets[0].clone()]);

        let fallback = select_log_ai_analysis_summary_evidence_snippets("未引用证据", &snippets);
        assert_eq!(fallback, snippets);
    }

    /// 验证单批次结果没有证据 ID 时，UI 可按模型输出的行号引用找到本地原日志片段。
    #[test]
    fn 智能分析证据内联支持行号兜底匹配() {
        let snippets = vec![
            LogAiAnalysisEvidenceSnippet {
                id: "E1-1".to_string(),
                chunk_index: 0,
                source_name: "app.log".to_string(),
                start_line: 11,
                end_line: 12,
                byte_count: 32,
                text: "11: ERROR timeout\n12: at service.Call".to_string(),
            },
            LogAiAnalysisEvidenceSnippet {
                id: "E1-2".to_string(),
                chunk_index: 0,
                source_name: "app.log".to_string(),
                start_line: 30,
                end_line: 30,
                byte_count: 13,
                text: "30: INFO done".to_string(),
            },
        ];

        let selected = referenced_log_ai_analysis_evidence_snippets_for_text(
            "结论：存在超时。\n证据：行 11-12",
            &snippets,
        );

        assert_eq!(selected, vec![snippets[0].clone()]);
    }

    /// 验证证据 ID 和行号同时出现时优先使用 ID，避免多文件同名行号把无关片段插入结论附近。
    #[test]
    fn 智能分析证据内联优先使用证据id() {
        let snippets = vec![
            LogAiAnalysisEvidenceSnippet {
                id: "E1-1".to_string(),
                chunk_index: 0,
                source_name: "app.log".to_string(),
                start_line: 11,
                end_line: 12,
                byte_count: 32,
                text: "11: ERROR timeout\n12: at service.Call".to_string(),
            },
            LogAiAnalysisEvidenceSnippet {
                id: "E2-1".to_string(),
                chunk_index: 1,
                source_name: "other.log".to_string(),
                start_line: 30,
                end_line: 30,
                byte_count: 16,
                text: "30: ERROR other".to_string(),
            },
        ];

        let selected = referenced_log_ai_analysis_evidence_snippets_for_text(
            "结论引用 E2-1，同时文本里也提到行 11-12",
            &snippets,
        );

        assert_eq!(selected, vec![snippets[1].clone()]);
    }

    /// 验证智能分析只对请求链路故障重试，配置校验错误直接失败。
    #[test]
    fn 智能分析重试只覆盖可恢复请求错误() {
        assert!(should_retry_log_ai_analysis_request(
            "AI 请求失败：请求接口失败：connection reset"
        ));
        assert!(should_retry_log_ai_analysis_request(
            "AI 响应读取失败：unexpected eof"
        ));
        assert!(!should_retry_log_ai_analysis_request("模型 ID 不能为空"));
        assert!(!should_retry_log_ai_analysis_request(
            "Base URL 必须以 http:// 或 https:// 开头"
        ));
        assert_eq!(
            final_log_ai_analysis_request_error("日志分块分析", 3, "AI 请求失败"),
            "日志分块分析失败：已尝试 3 次仍失败：AI 请求失败"
        );
    }

    /// 验证最终汇总只包含批次分析结果，不重新包含原始日志全文。
    #[test]
    fn 智能分析最终汇总只发送批次摘要() {
        let messages = log_ai_analysis_summary_messages(&[LogAiAnalysisCompletedChunk {
            range: LogAiAnalysisChunkRange {
                target_index: 0,
                source_name: "app.log".to_string(),
                start_line: 1,
                end_line: 2,
                byte_count: 20,
            },
            response: "结论：发现超时".to_string(),
            evidence_snippets: vec![LogAiAnalysisEvidenceSnippet {
                id: "E1-1".to_string(),
                chunk_index: 0,
                source_name: "app.log".to_string(),
                start_line: 1,
                end_line: 2,
                byte_count: 20,
                text: "1: SECRET_TOKEN\n2: ERROR timeout".to_string(),
            }],
        }]);

        assert!(messages[1].content.contains("结论：发现超时"));
        assert!(messages[1].content.contains("E1-1"));
        assert!(messages[1].content.contains("行 1-2"));
        assert!(!messages[1].content.contains("SECRET_TOKEN"));
        assert!(!messages[1].content.contains("```text"));
    }

    /// 验证追问上下文只包含最终结论、批次摘要和已展示证据，不夹带完整原始日志批次正文。
    #[test]
    fn 智能分析追问请求不发送完整原始日志正文() {
        let completed_chunks = vec![LogAiAnalysisCompletedChunk {
            range: LogAiAnalysisChunkRange {
                target_index: 0,
                source_name: "app.log".to_string(),
                start_line: 1,
                end_line: 3,
                byte_count: 90,
            },
            response: "结论：存在接口超时，引用 E1-1".to_string(),
            evidence_snippets: vec![LogAiAnalysisEvidenceSnippet {
                id: "E1-1".to_string(),
                chunk_index: 0,
                source_name: "app.log".to_string(),
                start_line: 2,
                end_line: 2,
                byte_count: 22,
                text: "2: ERROR timeout".to_string(),
            }],
        }];
        let history = vec![LogAiAnalysisFollowUpExchange {
            question: "是否和数据库有关？".to_string(),
            answer: "证据不足，需要数据库慢查询或连接池日志。".to_string(),
        }];

        let messages = log_ai_analysis_follow_up_messages(
            &completed_chunks,
            "最终结论：P1 接口超时，证据 E1-1",
            &completed_chunks[0].evidence_snippets,
            &history,
            "需要优先怎么处理？",
        );
        let joined = messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(joined.contains("最终结论：P1 接口超时"));
        assert!(joined.contains("结论：存在接口超时"));
        assert!(joined.contains("2: ERROR timeout"));
        assert!(joined.contains("是否和数据库有关？"));
        assert!(joined.contains("需要优先怎么处理？"));
        assert!(!joined.contains("1: SECRET_TOKEN"));
        assert!(!joined.contains("3: LARGE_RAW_LINE"));
    }

    /// 验证追问请求在没有可见证据时使用明确占位文案，避免模型误以为收到完整日志。
    #[test]
    fn 智能分析追问请求缺少证据时使用占位文案() {
        let messages = log_ai_analysis_follow_up_messages(&[], "", &[], &[], "还有风险吗？");

        assert!(messages[1].content.contains("（无最终汇总结论）"));
        assert!(messages[1].content.contains("（无批次摘要）"));
        assert!(messages[1].content.contains("（无可见原日志片段）"));
    }
}
