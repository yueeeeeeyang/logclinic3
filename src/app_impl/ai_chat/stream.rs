// AI 对话 OpenAI 兼容流式请求和 SSE 解析。
//
// 业务意图：
// - 后台线程负责阻塞 HTTP 读取和 SSE 字节解析，前台只接收增量事件并更新 GPUI 状态。
// - 请求体仍严格只包含 AI 对话页消息历史，不自动注入日志内容，避免重构改变隐私边界。

use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use super::*;

/// 构造 AI 对话流式请求体。
///
/// 业务意图：
/// - 第一版只发送用户在 AI 对话页中的消息历史，不自动附加当前日志内容，避免误把本地日志或敏感数据上传给模型服务。
///
/// 边界条件：
/// - 失败的助手消息和正在生成的助手消息不能作为上下文；空内容也不能发送，避免部分兼容接口拒绝请求。
pub(in crate::app) fn ai_chat_stream_request_body(
    model: &str,
    messages: &[AiChatMessage],
) -> serde_json::Value {
    let request_messages = messages
        .iter()
        .filter(|message| !message.content.trim().is_empty())
        .filter(|message| {
            !(message.role == AiChatMessageRole::Assistant
                && matches!(
                    message.status,
                    AiChatMessageStatus::Failed | AiChatMessageStatus::Streaming
                ))
        })
        .map(|message| {
            serde_json::json!({
                "role": message.role.as_str(),
                "content": message.content,
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "model": model.trim(),
        "messages": request_messages,
        "stream": true
    })
}

/// AI 对话 SSE 解析事件。
#[derive(Debug, PartialEq, Eq)]
pub(in crate::app) enum AiChatSseParsedEvent {
    /// 回复增量文本。
    Delta(String),
    /// 服务端结束标记。
    Done,
}

/// AI 对话 SSE 增量解析器。
///
/// 业务意图：
/// - `reqwest::blocking::Response` 按字节读取时可能把 UTF-8 字符、SSE 事件或 JSON 对象切在任意边界，解析器必须跨 chunk 保留缓冲。
pub(in crate::app) struct AiChatSseParser {
    /// 已经确认是合法 UTF-8 的 SSE 文本缓冲。
    text_buffer: String,
    /// 末尾尚未组成完整 UTF-8 字符的字节。
    pending_bytes: Vec<u8>,
}

impl AiChatSseParser {
    /// 创建空解析器。
    pub(in crate::app) fn new() -> Self {
        Self {
            text_buffer: String::new(),
            pending_bytes: Vec::new(),
        }
    }

    /// 推入一段响应字节并返回已完整解析出的 SSE 事件。
    pub(in crate::app) fn push_bytes(
        &mut self,
        bytes: &[u8],
    ) -> Result<Vec<AiChatSseParsedEvent>, String> {
        self.pending_bytes.extend_from_slice(bytes);
        loop {
            match std::str::from_utf8(&self.pending_bytes) {
                Ok(valid) => {
                    self.text_buffer.push_str(valid);
                    self.pending_bytes.clear();
                    break;
                }
                Err(error) if error.error_len().is_none() => {
                    let valid_up_to = error.valid_up_to();
                    if valid_up_to > 0 {
                        let valid = std::str::from_utf8(&self.pending_bytes[..valid_up_to])
                            .map_err(|utf8_error| {
                                format!("AI 响应 UTF-8 解析失败：{utf8_error}")
                            })?;
                        self.text_buffer.push_str(valid);
                        self.pending_bytes.drain(..valid_up_to);
                    }
                    break;
                }
                Err(error) => {
                    return Err(format!("AI 响应包含非法 UTF-8：{error}"));
                }
            }
        }

        self.text_buffer = self.text_buffer.replace("\r\n", "\n").replace('\r', "\n");
        let mut parsed = Vec::new();
        while let Some(separator_index) = self.text_buffer.find("\n\n") {
            let raw_event = self.text_buffer[..separator_index].to_string();
            self.text_buffer.drain(..separator_index + 2);
            if let Some(event) = parse_ai_chat_sse_event(&raw_event)? {
                parsed.push(event);
            }
        }
        Ok(parsed)
    }
}

/// 解析单个 SSE 事件。
///
/// 边界条件：
/// - OpenAI 兼容服务可能发送注释、空事件或不含 `delta.content` 的角色/结束事件；这些事件应忽略而不是报错。
pub(in crate::app) fn parse_ai_chat_sse_event(
    raw_event: &str,
) -> Result<Option<AiChatSseParsedEvent>, String> {
    let mut data_lines = Vec::new();
    for line in raw_event.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.strip_prefix(' ').unwrap_or(data));
        }
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let data = data_lines.join("\n");
    if data.trim() == "[DONE]" {
        return Ok(Some(AiChatSseParsedEvent::Done));
    }
    let value = serde_json::from_str::<serde_json::Value>(&data)
        .map_err(|error| format!("AI 响应 SSE JSON 解析失败：{error}"))?;
    let content = value
        .get("choices")
        .and_then(|choices| choices.as_array())
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("delta"))
        .and_then(|delta| delta.get("content"))
        .and_then(|content| content.as_str());
    Ok(content.map(|content| AiChatSseParsedEvent::Delta(content.to_string())))
}

/// 执行一次 OpenAI 兼容 AI 对话流式请求。
///
/// 业务意图：
/// - 该函数只在后台线程运行，负责 HTTP 和 SSE 字节解析；UI 更新统一通过 `sender` 发回前台，避免跨线程直接触碰 GPUI 状态。
pub(in crate::app) fn stream_openai_compatible_ai_chat(
    profile: ModelProfile,
    messages: Vec<AiChatMessage>,
    cancel: Arc<AtomicBool>,
    sender: mpsc::Sender<AiChatStreamEvent>,
) {
    let result = (|| -> Result<(), String> {
        validate_model_profile_fields(&profile.name, &profile.base_url, &profile.model)?;
        let url = model_test_chat_completions_url(&profile.base_url)?;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(AI_CHAT_REQUEST_TIMEOUT_SECONDS))
            .build()
            .map_err(|error| format!("AI 请求失败：创建 HTTP 客户端失败：{error}"))?;
        let mut request = client
            .post(url)
            .json(&ai_chat_stream_request_body(&profile.model, &messages));
        if let Some(authorization) = model_test_authorization_header(&profile.api_key) {
            request = request.header(reqwest::header::AUTHORIZATION, authorization);
        }

        let mut response = request
            .send()
            .map_err(|error| format!("AI 请求失败：请求接口失败：{error}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            let snippet = model_test_http_error_body_snippet(&body);
            if snippet.is_empty() {
                return Err(format!("AI 请求失败：HTTP 状态码 {status}"));
            }
            return Err(format!("AI 请求失败：HTTP 状态码 {status}，{snippet}"));
        }

        let mut parser = AiChatSseParser::new();
        let mut buffer = [0u8; 4096];
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = sender.send(AiChatStreamEvent::Stopped);
                return Ok(());
            }
            let read = response
                .read(&mut buffer)
                .map_err(|error| format!("AI 响应读取失败：{error}"))?;
            if read == 0 {
                let _ = sender.send(AiChatStreamEvent::Done);
                return Ok(());
            }
            for event in parser.push_bytes(&buffer[..read])? {
                match event {
                    AiChatSseParsedEvent::Delta(content) => {
                        if sender.send(AiChatStreamEvent::Delta(content)).is_err() {
                            return Ok(());
                        }
                    }
                    AiChatSseParsedEvent::Done => {
                        let _ = sender.send(AiChatStreamEvent::Done);
                        return Ok(());
                    }
                }
            }
        }
    })();

    if let Err(message) = result {
        let _ = sender.send(AiChatStreamEvent::Error(message));
    }
}
