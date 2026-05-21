// AI 对话 GPUI 面板单元测试。
//
// 业务意图：
// - 测试随 AI 对话模块迁移，直接覆盖 SQLite、SSE、请求体、模型选择和输入区高度等内部规则。
// - 断言保持迁移前语义不变，用于证明本次只是代码组织重构。

use std::{collections::HashSet, env, fs, path::PathBuf, sync::mpsc};

use rusqlite::{Connection, params};

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
        reasoning_content: String::new(),
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
        reasoning_content: "先看错误栈".to_string(),
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
        reasoning_content: String::new(),
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

/// 验证旧版 AI 对话数据库会补齐推理内容字段。
///
/// 业务意图：
/// - 深度思考功能新增 `reasoning_content` 列，用户已有历史库不能因为缺列而无法打开 AI 对话页。
#[test]
fn ai_对话旧数据库会迁移推理内容字段() {
    let path =
        test_ai_chat_database_file_path("migrate-reasoning").join(AI_CHAT_DATABASE_FILE_NAME);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("测试数据库目录应能创建");
    }
    {
        let connection = Connection::open(&path).expect("测试数据库应能打开");
        connection
            .execute_batch(
                r#"
                PRAGMA foreign_keys = ON;
                CREATE TABLE conversations (
                    id TEXT PRIMARY KEY NOT NULL,
                    title TEXT NOT NULL,
                    model_profile_id TEXT,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                );
                CREATE TABLE messages (
                    id TEXT PRIMARY KEY NOT NULL,
                    conversation_id TEXT NOT NULL,
                    role TEXT NOT NULL,
                    content TEXT NOT NULL,
                    status TEXT NOT NULL,
                    error_message TEXT,
                    sequence INTEGER NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    FOREIGN KEY(conversation_id) REFERENCES conversations(id) ON DELETE CASCADE
                );
                PRAGMA user_version = 1;
                "#,
            )
            .expect("旧 schema 应能创建");
        connection
            .execute(
                "INSERT INTO conversations
                    (id, title, model_profile_id, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, NULL, ?3, ?4)",
                params!["old-conversation", "旧对话", 1_i64, 1_i64],
            )
            .expect("旧会话应能写入");
        connection
            .execute(
                "INSERT INTO messages
                    (id, conversation_id, role, content, status, error_message,
                     sequence, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8)",
                params![
                    "old-message",
                    "old-conversation",
                    "assistant",
                    "旧回答",
                    "complete",
                    1_i64,
                    2_i64,
                    2_i64
                ],
            )
            .expect("旧消息应能写入");
    }

    let messages = load_ai_chat_messages(&path, "old-conversation").unwrap();
    assert_eq!(messages[0].content, "旧回答");
    assert_eq!(messages[0].reasoning_content, "");
    let connection = Connection::open(&path).expect("测试数据库应能重新打开");
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("schema 版本应能读取");
    assert_eq!(version, AI_CHAT_DATABASE_SCHEMA_VERSION);

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

/// 验证 AI 历史消息首帧测量阈值。
///
/// 业务意图：
/// - 首次进入 AI 对话页时，大历史会话不能同步测量全部消息，否则会触发 Markdown 解析、代码高亮初始化和文本排版的主线程卡顿。
/// - 阈值边界需要固定，避免后续调整列表状态时把所有会话重新改回全量测量。
#[test]
fn ai_对话大历史会话跳过首帧全量测量() {
    assert!(ai_chat_should_measure_all_messages(
        AI_CHAT_MESSAGE_MEASURE_ALL_THRESHOLD
    ));
    assert!(!ai_chat_should_measure_all_messages(
        AI_CHAT_MESSAGE_MEASURE_ALL_THRESHOLD + 1
    ));

    let large_list_state = ai_chat_message_list_state(AI_CHAT_MESSAGE_MEASURE_ALL_THRESHOLD + 1);
    assert_eq!(
        large_list_state.item_count(),
        AI_CHAT_MESSAGE_MEASURE_ALL_THRESHOLD + 1
    );
}

/// 验证 AI 历史加载状态能区分加载中和完成态。
///
/// 业务意图：
/// - AI 页现在先渲染页面骨架，再后台加载 SQLite 历史；状态判断必须稳定，否则加载动画、发送按钮和新建会话入口会出现错误启用。
#[test]
fn ai_对话历史加载状态区分加载中和已完成() {
    let (_sender, receiver) = mpsc::channel();
    assert!(AiChatLoadState::Loading { receiver }.is_loading());
    assert!(AiChatLoadState::Loaded.is_loaded());
    assert!(!AiChatLoadState::NotStarted.is_loaded());
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
            reasoning_content: String::new(),
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
            reasoning_content: "历史推理不应进入请求".to_string(),
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
            reasoning_content: "失败推理".to_string(),
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
            reasoning_content: "半截推理".to_string(),
            status: AiChatMessageStatus::Streaming,
            error_message: None,
            sequence: 4,
            created_at_ms: 4,
            updated_at_ms: 4,
        },
    ];

    let body =
        ai_chat_stream_request_body(" gpt-test ", &messages, false, AiChatReasoningEffort::High);
    assert_eq!(body["model"], "gpt-test");
    assert_eq!(body["stream"], true);
    assert!(body.get("reasoning_effort").is_none());
    assert_eq!(body["thinking"]["type"], "disabled");
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "用户问题");
    assert_eq!(body["messages"][1]["role"], "assistant");
    assert_eq!(body["messages"][1]["content"], "有效回答");
    assert!(body["messages"][1].get("reasoning_content").is_none());

    let reasoning_body =
        ai_chat_stream_request_body("gpt-test", &messages, true, AiChatReasoningEffort::High);
    assert_eq!(reasoning_body["reasoning_effort"], "high");
    assert_eq!(reasoning_body["thinking"]["type"], "enabled");

    let max_reasoning_body =
        ai_chat_stream_request_body("gpt-test", &messages, true, AiChatReasoningEffort::Max);
    assert_eq!(max_reasoning_body["reasoning_effort"], "max");
    assert_eq!(max_reasoning_body["thinking"]["type"], "enabled");
}

/// 验证深度思考单控件状态按关闭、高、最大循环。
///
/// 业务意图：
/// - 底部浮层使用单控件承载思考开关和强度选择；循环顺序必须稳定，避免用户误发错误强度。
#[test]
fn ai_对话深度思考单控件按关高最大循环() {
    let (enabled, effort) = next_ai_chat_deep_thinking_mode(false, AiChatReasoningEffort::Max);
    assert!(enabled);
    assert_eq!(effort, AiChatReasoningEffort::High);

    let (enabled, effort) = next_ai_chat_deep_thinking_mode(true, AiChatReasoningEffort::High);
    assert!(enabled);
    assert_eq!(effort, AiChatReasoningEffort::Max);

    let (enabled, effort) = next_ai_chat_deep_thinking_mode(true, AiChatReasoningEffort::Max);
    assert!(!enabled);
    assert_eq!(effort, AiChatReasoningEffort::High);
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
        reasoning_content: "已经生成的思考".to_string(),
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
    assert_eq!(messages[0].reasoning_content, "已经生成的思考");
    assert_eq!(messages[0].status, AiChatMessageStatus::Stopped);
    assert_eq!(persisted.content, "已经生成的部分内容");
    assert_eq!(persisted.reasoning_content, "已经生成的思考");
    assert_eq!(persisted.status, AiChatMessageStatus::Stopped);
}

/// 验证思考过程面板在推理阶段默认展开，正式回复开始后默认折叠。
///
/// 业务意图：
/// - 深度思考的实时推理内容需要在等待答案时直接可见；最终答案开始输出后，界面应自动把阅读焦点切回正式回复。
#[test]
fn ai_对话思考过程面板按生成阶段默认展开和折叠() {
    let mut message = AiChatMessage {
        id: "assistant-thinking".to_string(),
        conversation_id: "conversation".to_string(),
        role: AiChatMessageRole::Assistant,
        content: String::new(),
        reasoning_content: "先检查上下文".to_string(),
        status: AiChatMessageStatus::Streaming,
        error_message: None,
        sequence: 1,
        created_at_ms: 1,
        updated_at_ms: 1,
    };
    let mut expanded_message_ids = HashSet::new();
    let mut collapsed_message_ids = HashSet::new();

    assert!(ai_chat_reasoning_panel_is_expanded(
        &message,
        &expanded_message_ids,
        &collapsed_message_ids
    ));

    collapsed_message_ids.insert(message.id.clone());
    assert!(!ai_chat_reasoning_panel_is_expanded(
        &message,
        &expanded_message_ids,
        &collapsed_message_ids
    ));

    collapsed_message_ids.clear();
    message.content = "正式答案".to_string();
    assert!(!ai_chat_reasoning_panel_is_expanded(
        &message,
        &expanded_message_ids,
        &collapsed_message_ids
    ));

    expanded_message_ids.insert(message.id.clone());
    assert!(ai_chat_reasoning_panel_is_expanded(
        &message,
        &expanded_message_ids,
        &collapsed_message_ids
    ));
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

/// 验证 AI 对话左侧会话栏拖拽宽度会被限制在安全范围内。
///
/// UI 约束：
/// - 会话栏不能小到标题和新增按钮不可识别，也不能大到挤掉右侧消息和输入区。
#[test]
fn ai_对话左侧会话栏宽度拖拽会被限制() {
    let wide_window =
        MAIN_NAV_WIDTH + AI_CHAT_WORKSPACE_MIN_WIDTH + AI_CHAT_CONVERSATION_LIST_MAX_WIDTH + 120.0;
    assert_eq!(
        clamp_ai_chat_conversation_list_width(10.0, wide_window),
        AI_CHAT_CONVERSATION_LIST_MIN_WIDTH
    );
    assert_eq!(
        clamp_ai_chat_conversation_list_width(10_000.0, wide_window),
        AI_CHAT_CONVERSATION_LIST_MAX_WIDTH
    );

    let narrow_window = MAIN_NAV_WIDTH + AI_CHAT_WORKSPACE_MIN_WIDTH + 260.0;
    assert_eq!(
        clamp_ai_chat_conversation_list_width(10_000.0, narrow_window),
        260.0
    );
    assert_eq!(
        clamp_ai_chat_conversation_list_width(f32::NAN, wide_window),
        AI_CHAT_CONVERSATION_LIST_WIDTH
    );
}

/// 验证 AI 对话顶部栏高度与日志分析页一致。
///
/// UI 约束：
/// - AI 对话左侧会话栏和右侧对话 header 都使用 `AI_CHAT_TOP_BAR_HEIGHT`，该高度必须跟日志分析页操作栏一致，
///   避免用户在主导航切换页面时看到顶部 header 跳动。
#[test]
fn ai_对话顶部栏高度和日志分析一致() {
    assert_eq!(AI_CHAT_TOP_BAR_HEIGHT, TOOLBAR_HEIGHT);
}

/// 验证流式消息行高失效会按批次触发。
///
/// 业务意图：
/// - AI 对话 SSE 输出期间不能每个增量都重置虚拟列表行高，否则滚动条会持续闪烁。
/// - 离屏消息增长超过阈值或进入终态时仍必须允许显式失效，避免用户回看历史时沿用旧高度缓存。
#[test]
fn ai_对话流式行高失效按阈值批量触发() {
    assert!(!ai_chat_stream_row_invalidation_due(
        0,
        AI_CHAT_STREAM_ROW_INVALIDATE_BYTE_THRESHOLD - 1,
        false,
    ));
    assert!(ai_chat_stream_row_invalidation_due(
        0,
        AI_CHAT_STREAM_ROW_INVALIDATE_BYTE_THRESHOLD,
        false,
    ));
    assert!(ai_chat_stream_row_invalidation_due(4096, 4096, true));
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

/// 验证 AI 对话 SSE 会把推理增量和正式回复拆开。
///
/// 业务意图：
/// - 深度思考只展示服务端显式返回的推理字段，不能把它混入正式回复正文。
#[test]
fn ai_对话_sse_解析推理字段和正式回复() {
    let mut parser = AiChatSseParser::new();
    let events = parser
        .push_bytes(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"先分析\",\"content\":\"结论\"}}]}\n\n"
                .as_bytes(),
        )
        .unwrap();
    assert_eq!(
        events,
        vec![
            AiChatSseParsedEvent::ReasoningDelta("先分析".to_string()),
            AiChatSseParsedEvent::Delta("结论".to_string())
        ]
    );

    let mut reasoning_parser = AiChatSseParser::new();
    let reasoning_events = reasoning_parser
        .push_bytes("data: {\"choices\":[{\"delta\":{\"reasoning\":\"中间推理\"}}]}\n\n".as_bytes())
        .unwrap();
    assert_eq!(
        reasoning_events,
        vec![AiChatSseParsedEvent::ReasoningDelta("中间推理".to_string())]
    );

    let mut fallback_parser = AiChatSseParser::new();
    let fallback_events = fallback_parser
        .push_bytes(
            "data: {\"choices\":[{\"delta\":{\"reasoning_text\":\"备用字段\"}}]}\n\n".as_bytes(),
        )
        .unwrap();
    assert_eq!(
        fallback_events,
        vec![AiChatSseParsedEvent::ReasoningDelta("备用字段".to_string())]
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

/// 验证 AI 助手 Markdown 会解析常见块级结构和内联样式。
///
/// 业务意图：
/// - 助手回复常见的标题、段落、列表、引用、表格和分隔线必须进入结构化渲染路径，避免继续按纯文本堆叠。
/// - 内联样式通过 `StyledText` 高亮表达，测试只验证文本和样式范围存在，不绑定具体颜色，降低主题调整时的维护成本。
#[test]
fn ai_对话_markdown_解析常见块和内联样式() {
    let document = parse_ai_chat_markdown(
        "# 标题\n\n正文 **加粗** *斜体* ~~删除~~ `代码` [链接](https://example.com)\n\n- [x] 任务\n- 普通\n\n> 引用\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\n---\n",
        EffectiveTheme::Light,
    );

    assert!(document.blocks.iter().any(|block| {
        matches!(
            block,
            AppMarkdownBlock::Heading {
                level: 1,
                inlines: _
            }
        )
    }));
    assert!(
        document
            .blocks
            .iter()
            .any(|block| matches!(block, AppMarkdownBlock::List { .. }))
    );
    assert!(
        document
            .blocks
            .iter()
            .any(|block| matches!(block, AppMarkdownBlock::BlockQuote(_)))
    );
    assert!(
        document
            .blocks
            .iter()
            .any(|block| matches!(block, AppMarkdownBlock::Table { .. }))
    );
    assert!(
        document
            .blocks
            .iter()
            .any(|block| matches!(block, AppMarkdownBlock::ThematicBreak))
    );

    let paragraph = document
        .blocks
        .iter()
        .find_map(|block| match block {
            AppMarkdownBlock::Paragraph(inlines) => Some(inlines),
            _ => None,
        })
        .expect("正文段落应被解析出来");
    let (text, highlights) = flatten_ai_chat_markdown_inlines(
        paragraph,
        AppThemePalette::for_theme(EffectiveTheme::Light),
    );
    assert!(text.contains("正文 加粗 斜体 删除 代码 链接"));
    assert!(
        highlights.len() >= 5,
        "粗体、斜体、删除线、行内代码和链接文本都应生成样式范围"
    );
}

/// 验证 AI 助手 Markdown 代码块会保留文本并尝试语法高亮。
///
/// 边界条件：
/// - 已知语言应能走 syntect 高亮路径；未知语言应回退为纯文本语法，仍然保留原始代码行。
#[test]
fn ai_对话_markdown_代码块高亮和未知语言回退() {
    let known = parse_ai_chat_markdown("```rust\nfn main() {}\n```\n", EffectiveTheme::Light);
    let known_code = known
        .blocks
        .iter()
        .find_map(|block| match block {
            AppMarkdownBlock::CodeBlock { language, lines } => Some((language, lines)),
            _ => None,
        })
        .expect("已知语言代码块应被解析出来");
    assert_eq!(known_code.0.as_deref(), Some("rust"));
    assert_eq!(known_code.1[0].text, "fn main() {}");
    assert!(
        !known_code.1[0].highlights.is_empty(),
        "已知语言代码块应生成至少一个高亮范围"
    );

    let unknown = parse_ai_chat_markdown("```unknown-lang\nabc\n```\n", EffectiveTheme::Dark);
    let unknown_code = unknown
        .blocks
        .iter()
        .find_map(|block| match block {
            AppMarkdownBlock::CodeBlock { language, lines } => Some((language, lines)),
            _ => None,
        })
        .expect("未知语言代码块也应被解析出来");
    assert_eq!(unknown_code.0.as_deref(), Some("unknown-lang"));
    assert_eq!(unknown_code.1[0].text, "abc");
}

/// 验证原始 HTML 只按普通文本展示。
///
/// 安全约束：
/// - AI 回复来自外部模型，首版 Markdown 渲染不能执行 HTML，也不能把 HTML 注入成可交互节点。
#[test]
fn ai_对话_markdown_html_按文本展示() {
    let document = parse_ai_chat_markdown("<b>危险</b>", EffectiveTheme::Light);
    let paragraph = document
        .blocks
        .iter()
        .find_map(|block| match block {
            AppMarkdownBlock::Paragraph(inlines) => Some(inlines),
            _ => None,
        })
        .expect("HTML 文本应降级为段落");
    let (text, _) = flatten_ai_chat_markdown_inlines(
        paragraph,
        AppThemePalette::for_theme(EffectiveTheme::Light),
    );
    assert_eq!(text, "<b>危险</b>");
}

/// 验证 Markdown 普通换行按 CommonMark 软换行语义折叠为空格。
///
/// 业务意图：
/// - AI 回复经常为了可读性在列表项或长句中插入普通换行；这些换行不是段落边界，渲染时不应撑出额外空白。
/// - 命令参数和省略号也必须保持原文，避免展示层把 `--` 或 `...` 改写成其它 Unicode 标点。
#[test]
fn ai_对话_markdown_软换行折叠为空格且不改写标点() {
    let document = parse_ai_chat_markdown(
        "- 对英孚要谨慎：价格极高，且\n  千万别一次性付超过 3 个月的钱。\n\n运行 `cargo test -- --nocapture`...",
        EffectiveTheme::Light,
    );
    let list_item_text = document
        .blocks
        .iter()
        .find_map(|block| match block {
            AppMarkdownBlock::List { items, .. } => items.first(),
            _ => None,
        })
        .and_then(|blocks| blocks.first())
        .and_then(|block| match block {
            AppMarkdownBlock::Paragraph(inlines) => Some(inlines),
            _ => None,
        })
        .map(|inlines| {
            flatten_ai_chat_markdown_inlines(
                inlines,
                AppThemePalette::for_theme(EffectiveTheme::Light),
            )
            .0
        })
        .expect("列表项段落应被解析出来");
    assert_eq!(
        list_item_text,
        "对英孚要谨慎：价格极高，且 千万别一次性付超过 3 个月的钱。"
    );

    let trailing_paragraph = document
        .blocks
        .iter()
        .find_map(|block| match block {
            AppMarkdownBlock::Paragraph(inlines) => Some(inlines),
            _ => None,
        })
        .expect("列表后的普通段落应被解析出来");
    let (text, _) = flatten_ai_chat_markdown_inlines(
        trailing_paragraph,
        AppThemePalette::for_theme(EffectiveTheme::Light),
    );
    assert_eq!(text, "运行 cargo test -- --nocapture...");
}

/// 验证流式输出中的未闭合 Markdown 不会丢失可见内容。
///
/// 边界条件：
/// - SSE 增量过程中常出现未闭合代码块；解析器必须接受半成品内容，等后续增量补齐后再自然更新缓存。
#[test]
fn ai_对话_markdown_未闭合代码块保留内容() {
    let document = parse_ai_chat_markdown("```rust\nfn main()", EffectiveTheme::Light);
    let code = document
        .blocks
        .iter()
        .find_map(|block| match block {
            AppMarkdownBlock::CodeBlock { lines, .. } => Some(lines),
            _ => None,
        })
        .expect("未闭合代码块也应被解析成代码块");
    assert_eq!(code[0].text, "fn main()");
}

/// 验证 Markdown 缓存哈希只随原始内容变化。
///
/// 业务意图：
/// - 缓存键不能依赖渲染后的结构，否则 SSE 更新时难以精确判断当前消息是否需要重新解析。
#[test]
fn ai_对话_markdown_正文哈希随内容变化() {
    assert_eq!(
        ai_chat_markdown_content_hash("**相同**"),
        ai_chat_markdown_content_hash("**相同**")
    );
    assert_ne!(
        ai_chat_markdown_content_hash("**相同**"),
        ai_chat_markdown_content_hash("**不同**")
    );
}

/// 验证一键复制优先复制消息原始正文。
///
/// 业务意图：
/// - 气泡标题行的复制按钮应复制可继续粘贴的原始 Markdown/用户输入，不应带上 UI 中的“你”“助手”等标签。
#[test]
fn ai_对话消息复制使用原始正文() {
    let message = AiChatMessage {
        id: "copy-message".to_string(),
        conversation_id: "conversation".to_string(),
        role: AiChatMessageRole::Assistant,
        content: "**结论**\n\n- 条目".to_string(),
        reasoning_content: "推理".to_string(),
        status: AiChatMessageStatus::Complete,
        error_message: None,
        sequence: 1,
        created_at_ms: 1,
        updated_at_ms: 1,
    };

    assert_eq!(
        ai_chat_message_clipboard_text(&message),
        "**结论**\n\n- 条目"
    );
}

/// 验证空正式回复时仍可复制可见推理内容。
///
/// 边界条件：
/// - 深度思考流式阶段可能先展示推理、正式回复为空；此时一键复制不能返回空字符串。
#[test]
fn ai_对话消息复制在空正文时回退推理内容() {
    let message = AiChatMessage {
        id: "reasoning-message".to_string(),
        conversation_id: "conversation".to_string(),
        role: AiChatMessageRole::Assistant,
        content: String::new(),
        reasoning_content: "正在分析问题".to_string(),
        status: AiChatMessageStatus::Streaming,
        error_message: None,
        sequence: 1,
        created_at_ms: 1,
        updated_at_ms: 1,
    };

    assert_eq!(ai_chat_message_clipboard_text(&message), "正在分析问题");
}
