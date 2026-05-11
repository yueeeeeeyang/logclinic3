// AI 对话 SQLite 持久化实现。
//
// 业务意图：
// - AI 对话历史必须跨重启保存，因此会话和消息统一写入应用配置目录中的 SQLite 数据库。
// - 本文件只负责路径、schema 初始化和 CRUD，不处理 UI 状态或网络请求，便于后续迁移 schema 时集中维护。

use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, params};

use crate::config::app_config_dir;

use super::*;

/// 获取 AI 对话历史数据库路径。
///
/// 跨平台约束：
/// - 路径沿用 `app_config_dir`，避免 macOS/Windows 分别散落数据库目录判断。
/// - 非目标平台返回 `None` 时，AI 对话页仍可显示数据库不可用错误，不影响日志查看主流程。
pub(crate) fn ai_chat_database_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(AI_CHAT_DATABASE_FILE_NAME))
}

/// 打开并初始化 AI 对话 SQLite 数据库。
///
/// 业务意图：
/// - 所有 AI 对话历史操作统一经过这里打开连接，确保外键、schema 和版本检查在 macOS/Windows 上行为一致。
pub(crate) fn open_ai_chat_database(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("创建 AI 对话数据库目录失败：{error}"))?;
    }
    let connection =
        Connection::open(path).map_err(|error| format!("打开 AI 对话数据库失败：{error}"))?;
    initialize_ai_chat_database(&connection)?;
    Ok(connection)
}

/// 初始化 AI 对话数据库 schema。
///
/// 边界条件：
/// - `user_version` 大于当前版本时说明数据库来自未来版本，第一版不能安全降级读取，直接返回错误。
/// - `ON DELETE CASCADE` 保证删除会话时消息同步清理，避免孤儿消息继续参与后续查询。
pub(crate) fn initialize_ai_chat_database(connection: &Connection) -> Result<(), String> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("读取 AI 对话数据库版本失败：{error}"))?;
    if version > AI_CHAT_DATABASE_SCHEMA_VERSION {
        return Err(format!(
            "AI 对话数据库版本 {version} 高于当前支持版本 {AI_CHAT_DATABASE_SCHEMA_VERSION}"
        ));
    }

    connection
        .execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS conversations (
                id TEXT PRIMARY KEY NOT NULL,
                title TEXT NOT NULL,
                model_profile_id TEXT,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
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
            CREATE INDEX IF NOT EXISTS idx_conversations_updated_at
                ON conversations(updated_at_ms DESC);
            CREATE INDEX IF NOT EXISTS idx_messages_conversation_sequence
                ON messages(conversation_id, sequence);
            "#,
        )
        .map_err(|error| format!("初始化 AI 对话数据库失败：{error}"))?;

    if version == 0 {
        connection
            .pragma_update(None, "user_version", AI_CHAT_DATABASE_SCHEMA_VERSION)
            .map_err(|error| format!("写入 AI 对话数据库版本失败：{error}"))?;
    }
    Ok(())
}

/// 加载 AI 对话会话列表。
///
/// 业务意图：
/// - 左侧会话列表按最近更新时间倒序显示，最近使用的对话应优先出现在顶部。
pub(crate) fn load_ai_chat_conversations(path: &Path) -> Result<Vec<AiChatConversation>, String> {
    let connection = open_ai_chat_database(path)?;
    let mut statement = connection
        .prepare(
            r#"
            SELECT id, title, model_profile_id, created_at_ms, updated_at_ms
            FROM conversations
            ORDER BY updated_at_ms DESC, created_at_ms DESC
            "#,
        )
        .map_err(|error| format!("读取 AI 对话会话失败：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(AiChatConversation {
                id: row.get(0)?,
                title: row.get(1)?,
                model_profile_id: row.get(2)?,
                created_at_ms: row.get(3)?,
                updated_at_ms: row.get(4)?,
            })
        })
        .map_err(|error| format!("读取 AI 对话会话失败：{error}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析 AI 对话会话失败：{error}"))
}

/// 加载指定会话的消息。
///
/// 边界条件：
/// - 如果数据库中存在未知角色或状态，停止加载并显示错误，避免把损坏数据继续发给模型。
pub(crate) fn load_ai_chat_messages(
    path: &Path,
    conversation_id: &str,
) -> Result<Vec<AiChatMessage>, String> {
    let connection = open_ai_chat_database(path)?;
    let mut statement = connection
        .prepare(
            r#"
            SELECT id, conversation_id, role, content, status, error_message,
                   sequence, created_at_ms, updated_at_ms
            FROM messages
            WHERE conversation_id = ?1
            ORDER BY sequence ASC, created_at_ms ASC
            "#,
        )
        .map_err(|error| format!("读取 AI 对话消息失败：{error}"))?;
    let rows = statement
        .query_map(params![conversation_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
            ))
        })
        .map_err(|error| format!("读取 AI 对话消息失败：{error}"))?;

    let mut messages = Vec::new();
    for row in rows {
        let (
            id,
            conversation_id,
            role,
            content,
            status,
            error_message,
            sequence,
            created_at_ms,
            updated_at_ms,
        ) = row.map_err(|error| format!("解析 AI 对话消息失败：{error}"))?;
        messages.push(AiChatMessage {
            id,
            conversation_id,
            role: AiChatMessageRole::from_str(&role)?,
            content,
            status: AiChatMessageStatus::from_str(&status)?,
            error_message,
            sequence,
            created_at_ms,
            updated_at_ms,
        });
    }
    Ok(messages)
}

/// 插入 AI 对话会话。
pub(crate) fn insert_ai_chat_conversation(
    path: &Path,
    conversation: &AiChatConversation,
) -> Result<(), String> {
    let connection = open_ai_chat_database(path)?;
    connection
        .execute(
            r#"
            INSERT INTO conversations
                (id, title, model_profile_id, created_at_ms, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                conversation.id,
                conversation.title,
                conversation.model_profile_id,
                conversation.created_at_ms,
                conversation.updated_at_ms,
            ],
        )
        .map_err(|error| format!("保存 AI 对话会话失败：{error}"))?;
    Ok(())
}

/// 更新 AI 对话会话标题、模型配置和更新时间。
pub(crate) fn update_ai_chat_conversation(
    path: &Path,
    conversation: &AiChatConversation,
) -> Result<(), String> {
    let connection = open_ai_chat_database(path)?;
    connection
        .execute(
            r#"
            UPDATE conversations
            SET title = ?2, model_profile_id = ?3, updated_at_ms = ?4
            WHERE id = ?1
            "#,
            params![
                conversation.id,
                conversation.title,
                conversation.model_profile_id,
                conversation.updated_at_ms,
            ],
        )
        .map_err(|error| format!("更新 AI 对话会话失败：{error}"))?;
    Ok(())
}

/// 删除 AI 对话会话。
pub(crate) fn delete_ai_chat_conversation(
    path: &Path,
    conversation_id: &str,
) -> Result<(), String> {
    let connection = open_ai_chat_database(path)?;
    connection
        .execute(
            "DELETE FROM conversations WHERE id = ?1",
            params![conversation_id],
        )
        .map_err(|error| format!("删除 AI 对话会话失败：{error}"))?;
    Ok(())
}

/// 插入 AI 对话消息。
pub(crate) fn insert_ai_chat_message(path: &Path, message: &AiChatMessage) -> Result<(), String> {
    let connection = open_ai_chat_database(path)?;
    connection
        .execute(
            r#"
            INSERT INTO messages
                (id, conversation_id, role, content, status, error_message,
                 sequence, created_at_ms, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                message.id,
                message.conversation_id,
                message.role.as_str(),
                message.content,
                message.status.as_str(),
                message.error_message,
                message.sequence,
                message.created_at_ms,
                message.updated_at_ms,
            ],
        )
        .map_err(|error| format!("保存 AI 对话消息失败：{error}"))?;
    Ok(())
}

/// 更新 AI 对话消息内容和状态。
pub(crate) fn update_ai_chat_message(path: &Path, message: &AiChatMessage) -> Result<(), String> {
    let connection = open_ai_chat_database(path)?;
    connection
        .execute(
            r#"
            UPDATE messages
            SET content = ?2, status = ?3, error_message = ?4, updated_at_ms = ?5
            WHERE id = ?1
            "#,
            params![
                message.id,
                message.content,
                message.status.as_str(),
                message.error_message,
                message.updated_at_ms,
            ],
        )
        .map_err(|error| format!("更新 AI 对话消息失败：{error}"))?;
    Ok(())
}
