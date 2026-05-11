// AI 对话功能域单元测试。
//
// 业务意图：
// - 测试随 AI 对话模块迁移，直接覆盖 SQLite、SSE、请求体、模型选择和输入区高度等内部规则。
// - 断言保持迁移前语义不变，用于证明本次只是代码组织重构。

use std::{env, fs, path::PathBuf};

use rusqlite::Connection;

use super::view::ai_chat_placeholder_description;
use super::*;

/// 构造唯一的 AI 对话数据库测试路径。
///
/// 业务意图：
/// - AI 对话数据库会保存用户问题和模型回答，测试必须使用临时路径，避免污染真实应用历史。
fn test_ai_chat_database_file_path(name: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "logclinic3-ai-chat-test-{}-{}",
        std::process::id(),
        name
    ))
}

/// 验证 AI 对话占位提示会优先提示缺少模型配置。
///
/// 业务意图：
/// - AI 对话尚未接入真实请求时，空模型配置是用户下一步必须处理的阻塞项，截图中的占位区域应直接提示新增模型。
/// - 已存在模型配置时才继续展示“未接入对话请求”的占位说明，避免误导用户重复配置模型。
#[test]
fn ai_对话无模型配置时提示至少配置一个模型() {
    assert_eq!(ai_chat_placeholder_description(&[]), "需要至少配置一个模型");
    assert_eq!(
        ai_chat_placeholder_description(&[ModelProfile {
            id: "openai".to_string(),
            name: "OpenAI".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "sk-test".to_string(),
            model: "gpt-4.1-mini".to_string(),
        }]),
        "当前版本尚未接入对话请求。"
    );
}

/// 验证 AI 对话数据库会初始化 schema 并按会话顺序读写消息。
///
/// 业务意图：
/// - AI 对话历史需要跨重启完整恢复；schema 版本、会话排序和消息顺序是多会话列表可用性的基础。
#[test]
fn ai_对话数据库初始化并读写会话消息() {
    let path = test_ai_chat_database_file_path("roundtrip").join(AI_CHAT_DATABASE_FILE_NAME);
    let conversation = AiChatConversation {
        id: "conversation-a".to_string(),
        title: "排查错误".to_string(),
        model_profile_id: Some("model-a".to_string()),
        created_at_ms: 10,
        updated_at_ms: 20,
    };
    insert_ai_chat_conversation(&path, &conversation).expect("AI 会话应能写入 SQLite");
    let user_message = AiChatMessage {
        id: "message-user".to_string(),
        conversation_id: conversation.id.clone(),
        role: AiChatMessageRole::User,
        content: "为什么失败".to_string(),
        status: AiChatMessageStatus::Complete,
        error_message: None,
        sequence: 1,
        created_at_ms: 21,
        updated_at_ms: 21,
    };
    let assistant_message = AiChatMessage {
        id: "message-assistant".to_string(),
        conversation_id: conversation.id.clone(),
        role: AiChatMessageRole::Assistant,
        content: "需要看错误栈".to_string(),
        status: AiChatMessageStatus::Complete,
        error_message: None,
        sequence: 2,
        created_at_ms: 22,
        updated_at_ms: 22,
    };
    insert_ai_chat_message(&path, &assistant_message).expect("助手消息应能写入");
    insert_ai_chat_message(&path, &user_message).expect("用户消息应能写入");

    let connection = Connection::open(&path).expect("测试数据库应能打开");
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema 版本应能读取");
    assert_eq!(version, AI_CHAT_DATABASE_SCHEMA_VERSION);
    assert_eq!(
        load_ai_chat_conversations(&path).unwrap(),
        vec![conversation.clone()]
    );
    assert_eq!(
        load_ai_chat_messages(&path, &conversation.id).unwrap(),
        vec![user_message, assistant_message]
    );

    let _ = fs::remove_file(&path);
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir_all(parent);
    }
}

/// 验证删除 AI 会话会级联删除消息。
///
/// 边界条件：
/// - 多会话列表删除当前会话时不能留下孤儿消息，否则后续统计或迁移会误读旧内容。
#[test]
fn ai_对话删除会话会级联删除消息() {
    let path = test_ai_chat_database_file_path("cascade").join(AI_CHAT_DATABASE_FILE_NAME);
    let conversation = AiChatConversation {
        id: "conversation-delete".to_string(),
        title: "待删除".to_string(),
        model_profile_id: None,
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    let message = AiChatMessage {
        id: "message-delete".to_string(),
        conversation_id: conversation.id.clone(),
        role: AiChatMessageRole::User,
        content: "删除测试".to_string(),
        status: AiChatMessageStatus::Complete,
        error_message: None,
        sequence: 1,
        created_at_ms: 2,
        updated_at_ms: 2,
    };
    insert_ai_chat_conversation(&path, &conversation).expect("会话应能写入");
    insert_ai_chat_message(&path, &message).expect("消息应能写入");
    delete_ai_chat_conversation(&path, &conversation.id).expect("会话应能删除");
    assert!(load_ai_chat_conversations(&path).unwrap().is_empty());
    assert!(
        load_ai_chat_messages(&path, &conversation.id)
            .unwrap()
            .is_empty()
    );

    let _ = fs::remove_file(&path);
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir_all(parent);
    }
}

/// 验证 AI 对话默认模型选择规则。
///
/// 业务意图：
/// - 新会话应优先使用默认模型；没有默认模型时使用第一条配置，保证只配置一个模型即可开始对话。
#[test]
fn ai_对话默认模型优先默认否则首个() {
    let profiles = vec![
        ModelProfile {
            id: "first".to_string(),
            name: "First".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            api_key: String::new(),
            model: "first-model".to_string(),
        },
        ModelProfile {
            id: "default".to_string(),
            name: "Default".to_string(),
            base_url: "https://api.example.com/v1".to_string(),
            api_key: String::new(),
            model: "default-model".to_string(),
        },
    ];

    assert_eq!(
        ai_chat_default_model_profile_id(&profiles, Some("default")),
        Some("default".to_string())
    );
    assert_eq!(
        ai_chat_default_model_profile_id(&profiles, Some("missing")),
        Some("first".to_string())
    );
    assert_eq!(ai_chat_default_model_profile_id(&[], None), None);
}

/// 验证 AI 对话请求体只包含可用上下文且不带日志内容。
///
/// 边界条件：
/// - 失败和正在生成的助手消息不能再次发给模型，否则会把错误文案或半截内容污染下一轮上下文。
#[test]
fn ai_对话请求体过滤失败和流式消息() {
    let messages = vec![
        AiChatMessage {
            id: "user".to_string(),
            conversation_id: "c".to_string(),
            role: AiChatMessageRole::User,
            content: "用户问题".to_string(),
            status: AiChatMessageStatus::Complete,
            error_message: None,
            sequence: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
        },
        AiChatMessage {
            id: "assistant-ok".to_string(),
            conversation_id: "c".to_string(),
            role: AiChatMessageRole::Assistant,
            content: "有效回答".to_string(),
            status: AiChatMessageStatus::Complete,
            error_message: None,
            sequence: 2,
            created_at_ms: 2,
            updated_at_ms: 2,
        },
        AiChatMessage {
            id: "assistant-failed".to_string(),
            conversation_id: "c".to_string(),
            role: AiChatMessageRole::Assistant,
            content: "失败回答".to_string(),
            status: AiChatMessageStatus::Failed,
            error_message: Some("失败".to_string()),
            sequence: 3,
            created_at_ms: 3,
            updated_at_ms: 3,
        },
        AiChatMessage {
            id: "assistant-streaming".to_string(),
            conversation_id: "c".to_string(),
            role: AiChatMessageRole::Assistant,
            content: "半截回答".to_string(),
            status: AiChatMessageStatus::Streaming,
            error_message: None,
            sequence: 4,
            created_at_ms: 4,
            updated_at_ms: 4,
        },
    ];

    let body = ai_chat_stream_request_body(" gpt-test ", &messages);
    assert_eq!(body["model"], "gpt-test");
    assert_eq!(body["stream"], true);
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "用户问题");
    assert_eq!(body["messages"][1]["role"], "assistant");
    assert_eq!(body["messages"][1]["content"], "有效回答");
}

/// 验证 AI 对话实体 ID 在连续生成时不会冲突。
///
/// 边界条件：
/// - 部分 Windows 或虚拟化环境的系统时钟精度不足，连续创建用户消息和助手消息可能得到相同时间戳。
/// - ID 生成必须额外包含进程内单调序号，避免 SQLite 主键冲突导致发送失败。
#[test]
fn ai_对话实体_id_连续生成唯一() {
    let mut ids = std::collections::HashSet::new();
    for _ in 0..128 {
        assert!(ids.insert(new_ai_chat_entity_id("ai-message")));
    }
}

/// 验证停止流式回复会把助手消息从生成中改为已停止。
///
/// 业务意图：
/// - 用户点击停止或切换会话时，已经收到的部分内容应被保留，并且消息不能在重启后继续显示为“生成中”。
#[test]
fn ai_对话停止流式消息会保留内容并进入停止状态() {
    let mut messages = vec![AiChatMessage {
        id: "assistant-streaming".to_string(),
        conversation_id: "conversation".to_string(),
        role: AiChatMessageRole::Assistant,
        content: "已经生成的部分内容".to_string(),
        status: AiChatMessageStatus::Streaming,
        error_message: None,
        sequence: 1,
        created_at_ms: 1,
        updated_at_ms: 1,
    }];

    let persisted = finish_ai_chat_assistant_message_in_list(
        &mut messages,
        "assistant-streaming",
        AiChatMessageStatus::Stopped,
        None,
    )
    .expect("应返回需要落库的助手消息副本");

    assert_eq!(messages[0].content, "已经生成的部分内容");
    assert_eq!(messages[0].status, AiChatMessageStatus::Stopped);
    assert_eq!(persisted.content, "已经生成的部分内容");
    assert_eq!(persisted.status, AiChatMessageStatus::Stopped);
}

/// 验证 AI 输入区拖拽高度会被限制在安全范围内。
///
/// UI 约束：
/// - 输入区不能小到遮挡底部模型选择和发送按钮，也不能大到挤掉主要消息阅读区域。
#[test]
fn ai_对话输入区高度拖拽会被限制() {
    assert_eq!(
        clamp_ai_chat_input_height(10.0, 900.0),
        AI_CHAT_INPUT_MIN_HEIGHT
    );
    assert_eq!(
        clamp_ai_chat_input_height(10_000.0, 900.0),
        AI_CHAT_INPUT_MAX_HEIGHT
    );
    assert_eq!(
        clamp_ai_chat_input_height(10_000.0, 400.0),
        400.0 * AI_CHAT_INPUT_MAX_VIEWPORT_RATIO
    );
    assert_eq!(
        clamp_ai_chat_input_height(AI_CHAT_INPUT_DEFAULT_HEIGHT, f32::NAN),
        AI_CHAT_INPUT_DEFAULT_HEIGHT
    );
}

/// 验证 AI 对话 SSE 解析支持跨 chunk 和 DONE 事件。
///
/// 边界条件：
/// - 模型可能输出中文，HTTP 字节读取可能切开 UTF-8 字符和 SSE 事件边界，解析器必须等待完整字节后再解析。
#[test]
fn ai_对话_sse_解析支持跨_chunk() {
    let mut parser = AiChatSseParser::new();
    let first = parser
        .push_bytes("data: {\"choices\":[{\"delta\":{\"content\":\"你".as_bytes())
        .unwrap();
    assert!(first.is_empty());
    let second = parser
        .push_bytes("好\"}}]}\n\ndata: [DONE]\n\n".as_bytes())
        .unwrap();
    assert_eq!(
        second,
        vec![
            AiChatSseParsedEvent::Delta("你好".to_string()),
            AiChatSseParsedEvent::Done
        ]
    );
}

/// 验证 AI 对话 SSE 无效 JSON 会返回中文错误。
#[test]
fn ai_对话_sse_无效_json_返回错误() {
    let mut parser = AiChatSseParser::new();
    let error = parser
        .push_bytes(b"data: {broken}\n\n")
        .expect_err("无效 JSON 应返回错误");
    assert!(error.contains("AI 响应 SSE JSON 解析失败"));
}

/// 验证 AI 会话标题会折叠空白并截断。
#[test]
fn ai_对话标题折叠空白并截断() {
    assert_eq!(
        ai_chat_title_from_user_message("  第一行\n第二行  "),
        "第一行 第二行"
    );
    let long = "一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十一";
    let title = ai_chat_title_from_user_message(long);
    assert!(title.ends_with("..."));
    assert_eq!(title.chars().count(), AI_CHAT_TITLE_MAX_CHARS + 3);
}
