// 笔记 SQLite 持久化实现。
//
// 业务意图：
// - 笔记目录树和正文必须跨重启保存，因此统一写入应用配置目录中的独立 SQLite 数据库。
// - 本文件只负责路径、schema 初始化和 CRUD，不处理 GPUI 状态或编辑器交互，便于后续迁移 schema 时集中维护。

use std::{
    fs,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, params};

use crate::config::app_config_dir;

use super::*;

/// 获取笔记数据库路径。
///
/// 跨平台约束：
/// - 路径沿用 `app_config_dir`，避免 macOS/Windows 分别散落数据库目录判断。
pub(crate) fn notes_database_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(NOTES_DATABASE_FILE_NAME))
}

/// 打开并初始化笔记 SQLite 数据库。
pub(crate) fn open_notes_database(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建笔记数据库目录失败：{error}"))?;
    }
    let connection =
        Connection::open(path).map_err(|error| format!("打开笔记数据库失败：{error}"))?;
    initialize_notes_database(&connection)?;
    Ok(connection)
}

/// 初始化笔记数据库 schema。
///
/// 边界条件：
/// - `user_version` 大于当前版本时说明数据库来自未来版本，第一版不能安全降级读取，直接返回错误。
/// - 外键必须在每条连接上开启，确保删除目录时子目录和笔记级联清理。
pub(crate) fn initialize_notes_database(connection: &Connection) -> Result<(), String> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("读取笔记数据库版本失败：{error}"))?;
    if version > NOTES_DATABASE_SCHEMA_VERSION {
        return Err(format!(
            "笔记数据库版本 {version} 高于当前支持版本 {NOTES_DATABASE_SCHEMA_VERSION}"
        ));
    }

    connection
        .execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS note_directories (
                id TEXT PRIMARY KEY NOT NULL,
                parent_id TEXT,
                title TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                FOREIGN KEY(parent_id) REFERENCES note_directories(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS notes (
                id TEXT PRIMARY KEY NOT NULL,
                directory_id TEXT,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                content_format TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                FOREIGN KEY(directory_id) REFERENCES note_directories(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_note_directories_parent_updated
                ON note_directories(parent_id, updated_at_ms DESC, title ASC);
            CREATE INDEX IF NOT EXISTS idx_notes_directory_updated
                ON notes(directory_id, updated_at_ms DESC, title ASC);
            "#,
        )
        .map_err(|error| format!("初始化笔记数据库失败：{error}"))?;

    if version == 0 {
        connection
            .pragma_update(None, "user_version", NOTES_DATABASE_SCHEMA_VERSION)
            .map_err(|error| format!("写入笔记数据库版本失败：{error}"))?;
    }
    Ok(())
}

/// 加载完整笔记树。
pub(crate) fn load_note_tree(path: &Path) -> Result<Vec<NoteTreeRow>, String> {
    let connection = open_notes_database(path)?;
    let directories = load_all_note_directories(&connection)?;
    let notes = load_all_note_summaries(&connection)?;
    Ok(build_note_tree_rows(&directories, &notes))
}

/// 读取所有目录。
fn load_all_note_directories(connection: &Connection) -> Result<Vec<NoteDirectory>, String> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT id, parent_id, title, created_at_ms, updated_at_ms
            FROM note_directories
            "#,
        )
        .map_err(|error| format!("读取笔记目录失败：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(NoteDirectory {
                id: row.get(0)?,
                parent_id: row.get(1)?,
                title: row.get(2)?,
                created_at_ms: row.get(3)?,
                updated_at_ms: row.get(4)?,
            })
        })
        .map_err(|error| format!("读取笔记目录失败：{error}"))?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析笔记目录失败：{error}"))
}

/// 读取所有笔记摘要。
fn load_all_note_summaries(connection: &Connection) -> Result<Vec<Note>, String> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT id, directory_id, title, content_format, created_at_ms, updated_at_ms
            FROM notes
            "#,
        )
        .map_err(|error| format!("读取笔记列表失败：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            let format = row.get::<_, String>(3)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                format,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(|error| format!("读取笔记列表失败：{error}"))?;

    let mut notes = Vec::new();
    for row in rows {
        let (id, directory_id, title, format, created_at_ms, updated_at_ms) =
            row.map_err(|error| format!("解析笔记列表失败：{error}"))?;
        notes.push(Note {
            id,
            directory_id,
            title,
            content: String::new(),
            content_format: NoteContentFormat::from_str(&format)?,
            created_at_ms,
            updated_at_ms,
        });
    }
    Ok(notes)
}

/// 加载单篇笔记完整正文。
pub(crate) fn load_note(path: &Path, note_id: &str) -> Result<Option<Note>, String> {
    let connection = open_notes_database(path)?;
    let row = connection
        .query_row(
            r#"
            SELECT id, directory_id, title, content, content_format, created_at_ms, updated_at_ms
            FROM notes
            WHERE id = ?1
            "#,
            params![note_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("读取笔记正文失败：{error}"))?;
    row.map(
        |(id, directory_id, title, content, content_format, created_at_ms, updated_at_ms)| {
            Ok(Note {
                id,
                directory_id,
                title,
                content,
                content_format: NoteContentFormat::from_str(&content_format)?,
                created_at_ms,
                updated_at_ms,
            })
        },
    )
    .transpose()
}

/// 创建目录。
pub(crate) fn create_note_directory(
    path: &Path,
    parent_id: Option<String>,
    title: String,
) -> Result<NoteDirectory, String> {
    let directory = new_note_directory(parent_id, title);
    let connection = open_notes_database(path)?;
    connection
        .execute(
            r#"
            INSERT INTO note_directories
                (id, parent_id, title, created_at_ms, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                directory.id,
                directory.parent_id,
                directory.title,
                directory.created_at_ms,
                directory.updated_at_ms,
            ],
        )
        .map_err(|error| format!("创建笔记目录失败：{error}"))?;
    Ok(directory)
}

/// 创建笔记。
pub(crate) fn create_note(
    path: &Path,
    directory_id: Option<String>,
    title: String,
) -> Result<Note, String> {
    let note = new_note(directory_id, title);
    let connection = open_notes_database(path)?;
    connection
        .execute(
            r#"
            INSERT INTO notes
                (id, directory_id, title, content, content_format, created_at_ms, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            params![
                note.id,
                note.directory_id,
                note.title,
                note.content,
                note.content_format.as_str(),
                note.created_at_ms,
                note.updated_at_ms,
            ],
        )
        .map_err(|error| format!("创建笔记失败：{error}"))?;
    Ok(note)
}

/// 重命名目录。
pub(crate) fn rename_note_directory(
    path: &Path,
    directory_id: &str,
    title: &str,
) -> Result<(), String> {
    let connection = open_notes_database(path)?;
    let now = current_note_time_millis();
    connection
        .execute(
            r#"
            UPDATE note_directories
            SET title = ?2, updated_at_ms = ?3
            WHERE id = ?1
            "#,
            params![directory_id, title, now],
        )
        .map_err(|error| format!("重命名笔记目录失败：{error}"))?;
    Ok(())
}

/// 重命名笔记。
pub(crate) fn rename_note(path: &Path, note_id: &str, title: &str) -> Result<(), String> {
    let connection = open_notes_database(path)?;
    let now = current_note_time_millis();
    connection
        .execute(
            r#"
            UPDATE notes
            SET title = ?2, updated_at_ms = ?3
            WHERE id = ?1
            "#,
            params![note_id, title, now],
        )
        .map_err(|error| format!("重命名笔记失败：{error}"))?;
    Ok(())
}

/// 原子保存笔记标题、正文、格式和更新时间。
///
/// 业务意图：
/// - 右侧编辑器允许用户同时修改标题和正文，保存按钮必须把这两类草稿作为同一次业务提交处理。
/// - 使用单条 SQLite `UPDATE` 语句具备语句级原子性，避免“正文已保存但标题保存失败”这类半保存状态。
///
/// 边界条件：
/// - 正文仍执行 1 MiB UTF-8 字节上限检查，超过上限时直接返回中文错误且不写库。
/// - 如果目标笔记已经被删除或数据库被外部进程改动，`rows_affected` 为 0 时返回错误，避免 UI 误以为保存成功。
pub(crate) fn update_note(
    path: &Path,
    note_id: &str,
    title: &str,
    content: &str,
    content_format: NoteContentFormat,
) -> Result<i64, String> {
    if content.len() > NOTE_CONTENT_MAX_BYTES {
        return Err(format!(
            "笔记内容超过 {} KiB 上限，未保存",
            NOTE_CONTENT_MAX_BYTES / 1024
        ));
    }
    let connection = open_notes_database(path)?;
    let now = current_note_time_millis();
    let changed_rows = connection
        .execute(
            r#"
            UPDATE notes
            SET title = ?2, content = ?3, content_format = ?4, updated_at_ms = ?5
            WHERE id = ?1
            "#,
            params![note_id, title, content, content_format.as_str(), now],
        )
        .map_err(|error| format!("保存笔记失败：{error}"))?;
    if changed_rows == 0 {
        return Err("保存的笔记不存在，未写入".to_string());
    }
    Ok(now)
}

/// 删除目录及其所有子目录和笔记。
pub(crate) fn delete_note_directory(path: &Path, directory_id: &str) -> Result<(), String> {
    let connection = open_notes_database(path)?;
    connection
        .execute(
            "DELETE FROM note_directories WHERE id = ?1",
            params![directory_id],
        )
        .map_err(|error| format!("删除笔记目录失败：{error}"))?;
    Ok(())
}

/// 删除单篇笔记。
pub(crate) fn delete_note(path: &Path, note_id: &str) -> Result<(), String> {
    let connection = open_notes_database(path)?;
    connection
        .execute("DELETE FROM notes WHERE id = ?1", params![note_id])
        .map_err(|error| format!("删除笔记失败：{error}"))?;
    Ok(())
}

/// 更新笔记正文、格式和更新时间。
///
/// 业务意图：
/// - 第一版对外存储 API 保留“只更新正文”的能力，便于后续自动保存或格式转换只改正文时复用。
/// - 当前 UI 保存标题和正文会走 `update_note` 的单条原子更新，因此该函数在普通构建中可能暂时没有直接调用方。
#[allow(dead_code)]
pub(crate) fn update_note_content(
    path: &Path,
    note_id: &str,
    content: &str,
    content_format: NoteContentFormat,
) -> Result<(), String> {
    if content.len() > NOTE_CONTENT_MAX_BYTES {
        return Err(format!(
            "笔记内容超过 {} KiB 上限，未保存",
            NOTE_CONTENT_MAX_BYTES / 1024
        ));
    }
    let connection = open_notes_database(path)?;
    let now = current_note_time_millis();
    connection
        .execute(
            r#"
            UPDATE notes
            SET content = ?2, content_format = ?3, updated_at_ms = ?4
            WHERE id = ?1
            "#,
            params![note_id, content, content_format.as_str(), now],
        )
        .map_err(|error| format!("保存笔记失败：{error}"))?;
    Ok(())
}
