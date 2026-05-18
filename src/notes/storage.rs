// 笔记物理 Markdown 文件持久化实现。
//
// 业务意图：
// - 新版本笔记不再运行时写入 SQLite，而是把目录树映射为真实目录，把笔记正文映射为 `.md` / `.markdown` 文件。
// - 本文件只负责路径、扫描、迁移和 CRUD，不处理 GPUI 状态或编辑器交互，便于后续增加同步目录或导入导出能力。
//
// 边界条件：
// - 所有路径 ID 都是相对笔记根目录的 `/` 分隔路径，禁止绝对路径、`..` 和符号链接逃逸。
// - 旧 `notes.db` 只作为首次迁移来源保留；迁移失败必须保留原数据库并避免生成半成品 `notes/`。
// - 删除真实文件和目录时进入系统回收站/废纸篓，不直接永久删除用户数据。

use std::{
    collections::HashMap,
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;

use crate::config::app_config_dir;

use super::*;

/// 获取笔记物理文件根目录。
///
/// 跨平台约束：
/// - 路径沿用 `app_config_dir`，避免 macOS/Windows 分别散落用户数据目录判断。
pub(crate) fn notes_root_dir() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(NOTES_ROOT_DIR_NAME))
}

/// 获取旧版本笔记数据库路径。
///
/// 业务意图：
/// - 运行时不再把笔记写入 SQLite，但自动迁移仍需要找到旧 `notes.db`。
pub(crate) fn legacy_notes_database_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(NOTES_DATABASE_FILE_NAME))
}

/// 准备笔记根目录并按需迁移旧 SQLite 数据。
fn ensure_notes_root_ready(root: &Path) -> Result<(), String> {
    migrate_legacy_notes_database_if_needed(root)?;
    fs::create_dir_all(root).map_err(|error| format!("创建笔记目录失败：{error}"))
}

/// 加载完整笔记树。
pub(crate) fn load_note_tree(root: &Path) -> Result<Vec<NoteTreeRow>, String> {
    ensure_notes_root_ready(root)?;
    let mut directories = Vec::new();
    let mut notes = Vec::new();
    scan_note_directory(root, root, None, &mut directories, &mut notes)?;
    Ok(build_note_tree_rows(&directories, &notes))
}

/// 递归扫描真实目录，生成目录和笔记摘要。
fn scan_note_directory(
    root: &Path,
    directory: &Path,
    parent_id: Option<String>,
    directories: &mut Vec<NoteDirectory>,
    notes: &mut Vec<Note>,
) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("读取笔记目录 {} 失败：{error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("读取笔记目录项失败：{error}"))?;
        let path = entry.path();
        let file_name = entry.file_name();
        if is_hidden_note_path_name(&file_name) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("读取笔记路径 {} 失败：{error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            let id = note_relative_id(root, &path)?;
            directories.push(NoteDirectory {
                id: id.clone(),
                parent_id: parent_id.clone(),
                title: file_name.to_string_lossy().into_owned(),
                created_at_ms: metadata_time_millis(metadata.created().ok()),
                updated_at_ms: metadata_time_millis(metadata.modified().ok()),
            });
            scan_note_directory(root, &path, Some(id), directories, notes)?;
        } else if metadata.is_file() && is_markdown_note_path(&path) {
            let id = note_relative_id(root, &path)?;
            notes.push(Note {
                id,
                directory_id: parent_id.clone(),
                title: note_title_from_path(&path),
                content: String::new(),
                content_format: NoteContentFormat::Markdown,
                created_at_ms: metadata_time_millis(metadata.created().ok()),
                updated_at_ms: metadata_time_millis(metadata.modified().ok()),
            });
        }
    }
    Ok(())
}

/// 加载单篇笔记完整正文。
pub(crate) fn load_note(root: &Path, note_id: &str) -> Result<Option<Note>, String> {
    ensure_notes_root_ready(root)?;
    let path = note_path_for_id(root, note_id)?;
    if !path.exists() {
        return Ok(None);
    }
    if !is_markdown_note_path(&path) {
        return Err("选择的笔记不是 Markdown 文件".to_string());
    }
    let metadata =
        fs::metadata(&path).map_err(|error| format!("读取笔记文件元数据失败：{error}"))?;
    let content = read_markdown_file(&path)?;
    Ok(Some(Note {
        id: note_relative_id(root, &path)?,
        directory_id: note_parent_id(root, &path)?,
        title: note_title_from_path(&path),
        content,
        content_format: NoteContentFormat::Markdown,
        created_at_ms: metadata_time_millis(metadata.created().ok()),
        updated_at_ms: metadata_time_millis(metadata.modified().ok()),
    }))
}

/// 创建目录。
pub(crate) fn create_note_directory(
    root: &Path,
    parent_id: Option<String>,
    title: String,
) -> Result<NoteDirectory, String> {
    ensure_notes_root_ready(root)?;
    let parent = directory_path_for_optional_id(root, parent_id.as_deref())?;
    fs::create_dir_all(&parent).map_err(|error| format!("创建父级笔记目录失败：{error}"))?;
    let path = unique_note_child_path(&parent, &title, "未命名目录", None, None);
    fs::create_dir(&path).map_err(|error| format!("创建笔记目录失败：{error}"))?;
    let metadata =
        fs::metadata(&path).map_err(|error| format!("读取笔记目录元数据失败：{error}"))?;
    Ok(NoteDirectory {
        id: note_relative_id(root, &path)?,
        parent_id,
        title: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "未命名目录".to_string()),
        created_at_ms: metadata_time_millis(metadata.created().ok()),
        updated_at_ms: metadata_time_millis(metadata.modified().ok()),
    })
}

/// 创建笔记。
pub(crate) fn create_note(
    root: &Path,
    directory_id: Option<String>,
    title: String,
) -> Result<Note, String> {
    ensure_notes_root_ready(root)?;
    let parent = directory_path_for_optional_id(root, directory_id.as_deref())?;
    fs::create_dir_all(&parent).map_err(|error| format!("创建父级笔记目录失败：{error}"))?;
    let path = unique_note_child_path(
        &parent,
        &note_base_title(&title),
        "未命名笔记",
        Some(NOTE_MARKDOWN_EXTENSION),
        None,
    );
    write_markdown_file_atomic(&path, "")?;
    let metadata =
        fs::metadata(&path).map_err(|error| format!("读取笔记文件元数据失败：{error}"))?;
    Ok(Note {
        id: note_relative_id(root, &path)?,
        directory_id,
        title: note_title_from_path(&path),
        content: String::new(),
        content_format: NoteContentFormat::Markdown,
        created_at_ms: metadata_time_millis(metadata.created().ok()),
        updated_at_ms: metadata_time_millis(metadata.modified().ok()),
    })
}

/// 重命名目录。
pub(crate) fn rename_note_directory(
    root: &Path,
    directory_id: &str,
    title: &str,
) -> Result<String, String> {
    ensure_notes_root_ready(root)?;
    let current_path = directory_path_for_id(root, directory_id)?;
    let parent = current_path
        .parent()
        .ok_or_else(|| "笔记目录没有父目录，无法重命名".to_string())?;
    let next_path = unique_note_child_path(parent, title, "未命名目录", None, Some(&current_path));
    if current_path != next_path {
        fs::rename(&current_path, &next_path)
            .map_err(|error| format!("重命名笔记目录 {} 失败：{error}", current_path.display()))?;
    }
    note_relative_id(root, &next_path)
}

/// 重命名笔记。
pub(crate) fn rename_note(root: &Path, note_id: &str, title: &str) -> Result<String, String> {
    ensure_notes_root_ready(root)?;
    let current_path = note_path_for_id(root, note_id)?;
    let parent = current_path
        .parent()
        .ok_or_else(|| "笔记文件没有父目录，无法重命名".to_string())?;
    let next_path = unique_note_child_path(
        parent,
        &note_base_title(title),
        "未命名笔记",
        Some(NOTE_MARKDOWN_EXTENSION),
        Some(&current_path),
    );
    if current_path != next_path {
        fs::rename(&current_path, &next_path)
            .map_err(|error| format!("重命名笔记失败：{error}"))?;
    }
    note_relative_id(root, &next_path)
}

/// 保存笔记标题、正文和更新时间。
///
/// 业务意图：
/// - 右侧编辑器允许用户同时修改标题和正文；物理文件存储下标题变化意味着文件重命名，正文变化意味着写入 Markdown。
/// - 写入使用临时文件加替换流程，避免崩溃时留下半截 Markdown。
pub(crate) fn update_note(
    root: &Path,
    note_id: &str,
    title: &str,
    content: &str,
    _content_format: NoteContentFormat,
) -> Result<Note, String> {
    if content.len() > NOTE_CONTENT_MAX_BYTES {
        return Err(format!(
            "笔记内容超过 {} KiB 上限，未保存",
            NOTE_CONTENT_MAX_BYTES / 1024
        ));
    }
    ensure_notes_root_ready(root)?;
    let current_path = note_path_for_id(root, note_id)?;
    if !current_path.exists() {
        return Err("保存的笔记不存在，未写入".to_string());
    }
    let parent = current_path
        .parent()
        .ok_or_else(|| "笔记文件没有父目录，无法保存".to_string())?;
    let next_path = unique_note_child_path(
        parent,
        &note_base_title(title),
        "未命名笔记",
        Some(NOTE_MARKDOWN_EXTENSION),
        Some(&current_path),
    );
    let renamed = current_path != next_path;
    if renamed {
        fs::rename(&current_path, &next_path)
            .map_err(|error| format!("重命名笔记文件 {} 失败：{error}", current_path.display()))?;
    }
    if let Err(error) = write_markdown_file_atomic(&next_path, content) {
        if renamed {
            return match fs::rename(&next_path, &current_path) {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(format!(
                    "{error}；回滚笔记文件名 {} 失败：{rollback_error}",
                    current_path.display()
                )),
            };
        }
        return Err(error);
    }
    let metadata =
        fs::metadata(&next_path).map_err(|error| format!("读取笔记文件元数据失败：{error}"))?;
    Ok(Note {
        id: note_relative_id(root, &next_path)?,
        directory_id: note_parent_id(root, &next_path)?,
        title: note_title_from_path(&next_path),
        content: content.to_string(),
        content_format: NoteContentFormat::Markdown,
        created_at_ms: metadata_time_millis(metadata.created().ok()),
        updated_at_ms: metadata_time_millis(metadata.modified().ok()),
    })
}

/// 删除目录及其所有子目录和笔记。
pub(crate) fn delete_note_directory(root: &Path, directory_id: &str) -> Result<(), String> {
    ensure_notes_root_ready(root)?;
    let path = directory_path_for_id(root, directory_id)?;
    trash::delete(&path).map_err(|error| format!("移动笔记目录到回收站失败：{error}"))
}

/// 删除单篇笔记。
pub(crate) fn delete_note(root: &Path, note_id: &str) -> Result<(), String> {
    ensure_notes_root_ready(root)?;
    let path = note_path_for_id(root, note_id)?;
    trash::delete(&path).map_err(|error| format!("移动笔记到回收站失败：{error}"))
}

/// 更新笔记正文、格式和更新时间。
///
/// 业务意图：
/// - 该函数保留给测试和后续自动保存路径；物理文件存储下只写 Markdown 正文，不再写格式字段。
#[allow(dead_code)]
pub(crate) fn update_note_content(
    root: &Path,
    note_id: &str,
    content: &str,
    content_format: NoteContentFormat,
) -> Result<(), String> {
    let note = load_note(root, note_id)?.ok_or_else(|| "保存的笔记不存在，未写入".to_string())?;
    update_note(root, note_id, &note.title, content, content_format).map(|_| ())
}

/// 读取 UTF-8 Markdown 文件。
fn read_markdown_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| format!("读取笔记文件失败：{error}"))?;
    let content = String::from_utf8(bytes)
        .map_err(|_| format!("笔记文件 {} 不是有效 UTF-8", path.display()))?;
    Ok(content
        .strip_prefix('\u{feff}')
        .unwrap_or(&content)
        .to_string())
}

/// 原子写入 Markdown 文件。
fn write_markdown_file_atomic(path: &Path, content: &str) -> Result<(), String> {
    if content.len() > NOTE_CONTENT_MAX_BYTES {
        return Err(format!(
            "笔记内容超过 {} KiB 上限，未保存",
            NOTE_CONTENT_MAX_BYTES / 1024
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建笔记目录失败：{error}"))?;
    }
    let temp_path = temporary_sibling_path(path, "tmp");
    fs::write(&temp_path, content).map_err(|error| format!("写入临时笔记文件失败：{error}"))?;
    replace_file_with_temp(path, &temp_path)
}

/// 用临时文件替换目标文件，Windows 下通过备份路径避免覆盖失败导致内容丢失。
fn replace_file_with_temp(path: &Path, temp_path: &Path) -> Result<(), String> {
    if !path.exists() {
        return fs::rename(temp_path, path).map_err(|error| format!("保存笔记文件失败：{error}"));
    }

    let backup_path = temporary_sibling_path(path, "bak");
    fs::rename(path, &backup_path).map_err(|error| format!("备份原笔记文件失败：{error}"))?;
    match fs::rename(temp_path, path) {
        Ok(()) => {
            let _ = fs::remove_file(&backup_path);
            Ok(())
        }
        Err(error) => {
            let _ = fs::rename(&backup_path, path);
            Err(format!("替换笔记文件失败：{error}"))
        }
    }
}

/// 构造同目录临时路径。
fn temporary_sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "note".into());
    parent.join(format!(
        ".{file_name}.{suffix}-{}-{}",
        std::process::id(),
        current_note_time_millis()
    ))
}

/// 如果旧 SQLite 存在且物理目录尚未初始化，则迁移旧数据。
fn migrate_legacy_notes_database_if_needed(root: &Path) -> Result<(), String> {
    // 旧库自动迁移只允许发生在真实应用笔记根目录，避免测试、导入或未来自定义目录误读配置目录中的 `notes.db`。
    if notes_root_dir().as_deref() != Some(root) {
        return Ok(());
    }
    if note_root_has_user_entries(root)? {
        return Ok(());
    }
    let Some(database_path) = legacy_notes_database_path() else {
        return Ok(());
    };
    if !database_path.exists() {
        return Ok(());
    }

    let parent = root
        .parent()
        .ok_or_else(|| "笔记根目录没有父目录，无法迁移旧数据库".to_string())?;
    fs::create_dir_all(parent).map_err(|error| format!("创建笔记迁移目录失败：{error}"))?;
    let temp_root = parent.join(format!(
        ".notes-migrating-{}-{}",
        std::process::id(),
        current_note_time_millis()
    ));
    if temp_root.exists() {
        fs::remove_dir_all(&temp_root).map_err(|error| format!("清理旧迁移目录失败：{error}"))?;
    }
    fs::create_dir(&temp_root).map_err(|error| format!("创建笔记迁移临时目录失败：{error}"))?;

    let migration_result = migrate_legacy_notes_database_to_directory(&database_path, &temp_root);
    if let Err(error) = migration_result {
        let _ = fs::remove_dir_all(&temp_root);
        return Err(error);
    }

    if root.exists() {
        fs::remove_dir(root).map_err(|error| format!("移除空笔记目录失败：{error}"))?;
    }
    fs::rename(&temp_root, root).map_err(|error| format!("写入迁移后笔记目录失败：{error}"))
}

/// 判断物理笔记根目录是否已有用户条目。
fn note_root_has_user_entries(root: &Path) -> Result<bool, String> {
    if !root.exists() {
        return Ok(false);
    }
    let mut entries = fs::read_dir(root).map_err(|error| format!("读取笔记根目录失败：{error}"))?;
    match entries.next() {
        Some(Ok(_)) => Ok(true),
        Some(Err(error)) => Err(format!("读取笔记根目录项失败：{error}")),
        None => Ok(false),
    }
}

/// 把旧 SQLite 数据库导出到指定物理目录。
fn migrate_legacy_notes_database_to_directory(
    database_path: &Path,
    target_root: &Path,
) -> Result<(), String> {
    // 迁移写入临时目录或测试目录时，调用方只保证父目录存在；这里创建目标根目录，后续目录和文件才能按旧树结构落盘。
    fs::create_dir_all(target_root).map_err(|error| format!("创建迁移目标目录失败：{error}"))?;
    let connection = open_legacy_notes_database(database_path)?;
    let directories = load_legacy_note_directories(&connection)?;
    let notes = load_legacy_notes(&connection)?;
    let mut directories_by_parent: HashMap<Option<String>, Vec<NoteDirectory>> = HashMap::new();
    for directory in directories {
        directories_by_parent
            .entry(directory.parent_id.clone())
            .or_default()
            .push(directory);
    }
    let mut notes_by_parent: HashMap<Option<String>, Vec<Note>> = HashMap::new();
    for note in notes {
        notes_by_parent
            .entry(note.directory_id.clone())
            .or_default()
            .push(note);
    }
    migrate_legacy_children(None, target_root, &directories_by_parent, &notes_by_parent)
}

/// 递归迁移旧数据库中的目录和笔记。
fn migrate_legacy_children(
    parent_id: Option<String>,
    target_parent: &Path,
    directories_by_parent: &HashMap<Option<String>, Vec<NoteDirectory>>,
    notes_by_parent: &HashMap<Option<String>, Vec<Note>>,
) -> Result<(), String> {
    if let Some(directories) = directories_by_parent.get(&parent_id) {
        let mut directories = directories.clone();
        sort_note_directories_for_storage(&mut directories);
        for directory in directories {
            let directory_path =
                unique_note_child_path(target_parent, &directory.title, "未命名目录", None, None);
            fs::create_dir(&directory_path)
                .map_err(|error| format!("迁移笔记目录 {} 失败：{error}", directory.title))?;
            migrate_legacy_children(
                Some(directory.id),
                &directory_path,
                directories_by_parent,
                notes_by_parent,
            )?;
        }
    }

    if let Some(notes) = notes_by_parent.get(&parent_id) {
        let mut notes = notes.clone();
        sort_notes_for_storage(&mut notes);
        for note in notes {
            let path = unique_note_child_path(
                target_parent,
                &note_base_title(&note.title),
                "未命名笔记",
                Some(NOTE_MARKDOWN_EXTENSION),
                None,
            );
            let content = legacy_note_to_markdown(&note)?;
            write_markdown_file_atomic(&path, &content)?;
        }
    }
    Ok(())
}

/// 打开旧 SQLite 数据库并校验 schema 版本。
fn open_legacy_notes_database(path: &Path) -> Result<Connection, String> {
    let connection =
        Connection::open(path).map_err(|error| format!("打开旧笔记数据库失败：{error}"))?;
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("读取旧笔记数据库版本失败：{error}"))?;
    if version > NOTES_DATABASE_SCHEMA_VERSION {
        return Err(format!(
            "旧笔记数据库版本 {version} 高于当前支持版本 {NOTES_DATABASE_SCHEMA_VERSION}"
        ));
    }
    Ok(connection)
}

/// 读取旧 SQLite 中所有目录。
fn load_legacy_note_directories(connection: &Connection) -> Result<Vec<NoteDirectory>, String> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT id, parent_id, title, created_at_ms, updated_at_ms
            FROM note_directories
            "#,
        )
        .map_err(|error| format!("读取旧笔记目录失败：{error}"))?;
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
        .map_err(|error| format!("读取旧笔记目录失败：{error}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("解析旧笔记目录失败：{error}"))
}

/// 读取旧 SQLite 中所有笔记。
fn load_legacy_notes(connection: &Connection) -> Result<Vec<Note>, String> {
    let mut statement = connection
        .prepare(
            r#"
            SELECT id, directory_id, title, content, content_format, created_at_ms, updated_at_ms
            FROM notes
            "#,
        )
        .map_err(|error| format!("读取旧笔记列表失败：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            let content_format = row.get::<_, String>(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                content_format,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(|error| format!("读取旧笔记列表失败：{error}"))?;

    let mut notes = Vec::new();
    for row in rows {
        let (id, directory_id, title, content, content_format, created_at_ms, updated_at_ms) =
            row.map_err(|error| format!("解析旧笔记列表失败：{error}"))?;
        notes.push(Note {
            id,
            directory_id,
            title,
            content,
            content_format: NoteContentFormat::from_str(&content_format)?,
            created_at_ms,
            updated_at_ms,
        });
    }
    Ok(notes)
}

/// 把旧笔记正文转换为 Markdown。
fn legacy_note_to_markdown(note: &Note) -> Result<String, String> {
    match note.content_format {
        NoteContentFormat::PlainText | NoteContentFormat::Markdown => Ok(note.content.clone()),
        NoteContentFormat::RichText => {
            let document = NoteRichTextDocument::from_json(&note.content)?;
            Ok(document.to_markdown())
        }
    }
}

/// 存储层排序目录。
fn sort_note_directories_for_storage(directories: &mut [NoteDirectory]) {
    directories.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.id.cmp(&right.id))
    });
}

/// 存储层排序笔记。
fn sort_notes_for_storage(notes: &mut [Note]) {
    notes.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.id.cmp(&right.id))
    });
}

/// 根据目录 ID 返回真实目录路径。
fn directory_path_for_id(root: &Path, directory_id: &str) -> Result<PathBuf, String> {
    let path = path_for_note_id(root, directory_id)?;
    reject_note_symlink(&path)?;
    if path.exists() && !path.is_dir() {
        return Err("选择的笔记目录不是目录".to_string());
    }
    Ok(path)
}

/// 根据可选目录 ID 返回真实目录路径。
fn directory_path_for_optional_id(
    root: &Path,
    directory_id: Option<&str>,
) -> Result<PathBuf, String> {
    match directory_id {
        Some(directory_id) => directory_path_for_id(root, directory_id),
        None => Ok(root.to_path_buf()),
    }
}

/// 根据笔记 ID 返回真实文件路径。
fn note_path_for_id(root: &Path, note_id: &str) -> Result<PathBuf, String> {
    let path = path_for_note_id(root, note_id)?;
    reject_note_symlink(&path)?;
    if path.exists() && !path.is_file() {
        return Err("选择的笔记不是文件".to_string());
    }
    Ok(path)
}

/// 拒绝符号链接路径，避免物理笔记 ID 被外部文件系统状态改成根目录外路径。
fn reject_note_symlink(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err("笔记路径不能是符号链接".to_string());
        }
    }
    Ok(())
}

/// 解析相对路径 ID，禁止越过笔记根目录。
fn path_for_note_id(root: &Path, id: &str) -> Result<PathBuf, String> {
    let relative = Path::new(id);
    if id.trim().is_empty() || relative.is_absolute() {
        return Err("笔记路径无效".to_string());
    }
    let mut path = root.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(segment) => path.push(segment),
            _ => return Err("笔记路径不能包含上级目录或特殊路径段".to_string()),
        }
    }
    Ok(path)
}

/// 计算路径相对笔记根目录的稳定 ID。
fn note_relative_id(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "笔记路径不在笔记根目录下".to_string())?;
    let parts = relative
        .components()
        .map(|component| match component {
            Component::Normal(segment) => Ok(segment.to_string_lossy().into_owned()),
            _ => Err("笔记路径包含非法路径段".to_string()),
        })
        .collect::<Result<Vec<_>, String>>()?;
    if parts.is_empty() {
        Err("笔记路径为空".to_string())
    } else {
        Ok(parts.join("/"))
    }
}

/// 计算笔记文件父目录 ID。
fn note_parent_id(root: &Path, path: &Path) -> Result<Option<String>, String> {
    let Some(parent) = path.parent() else {
        return Ok(None);
    };
    if parent == root {
        Ok(None)
    } else {
        Ok(Some(note_relative_id(root, parent)?))
    }
}

/// 判断文件名是否隐藏。
fn is_hidden_note_path_name(name: &OsStr) -> bool {
    name.to_string_lossy().starts_with('.')
}

/// 判断路径是否是 Markdown 笔记。
fn is_markdown_note_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        })
        .unwrap_or(false)
}

/// 从 Markdown 文件名恢复标题。
fn note_title_from_path(path: &Path) -> String {
    path.file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| "未命名笔记".to_string())
}

/// 返回笔记标题对应的基础文件名。
fn note_base_title(title: &str) -> String {
    let trimmed = title.trim();
    trimmed
        .strip_suffix(".markdown")
        .or_else(|| trimmed.strip_suffix(".md"))
        .unwrap_or(trimmed)
        .to_string()
}

/// 生成同级唯一子路径。
fn unique_note_child_path(
    parent: &Path,
    title: &str,
    fallback: &str,
    extension: Option<&str>,
    exclude: Option<&Path>,
) -> PathBuf {
    let base = sanitize_note_file_name(title, fallback);
    for index in 1.. {
        let candidate_name = if index == 1 {
            base.clone()
        } else {
            format!("{base} ({index})")
        };
        let file_name = if let Some(extension) = extension {
            format!("{candidate_name}.{extension}")
        } else {
            candidate_name
        };
        let candidate = parent.join(file_name);
        if exclude.is_some_and(|path| path == candidate.as_path()) || !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("无限序列总能找到可用的笔记路径")
}

/// 清理文件名中的跨平台非法字符。
fn sanitize_note_file_name(title: &str, fallback: &str) -> String {
    let mut sanitized = title
        .trim()
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    sanitized = sanitized.trim_matches([' ', '.']).to_string();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        sanitized = fallback.to_string();
    }
    if is_windows_reserved_file_name(&sanitized) {
        sanitized.push('_');
    }
    sanitized
}

/// 判断 Windows 保留文件名。
fn is_windows_reserved_file_name(name: &str) -> bool {
    let upper = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// 把文件元数据时间转换为 Unix epoch 毫秒。
fn metadata_time_millis(time: Option<SystemTime>) -> i64 {
    time.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_else(current_note_time_millis)
}

#[cfg(test)]
mod tests {
    use std::{env, fs};

    use rusqlite::params;

    use super::*;

    /// 构造独立的迁移测试目录。
    ///
    /// 业务意图：
    /// - 迁移测试会创建真实 SQLite 文件和 Markdown 目录，必须隔离到临时目录，避免污染用户配置目录中的真实笔记。
    fn migration_test_dir(name: &str) -> PathBuf {
        let root = env::temp_dir().join(format!(
            "logclinic3-notes-migration-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("迁移测试目录应能创建");
        root
    }

    /// 创建旧版 SQLite schema。
    ///
    /// 边界条件：
    /// - 这里只覆盖迁移读取所需的最小字段，避免测试和历史运行时实现过度耦合。
    fn create_legacy_database(path: &Path) -> Connection {
        let connection = Connection::open(path).expect("旧笔记测试数据库应能创建");
        connection
            .execute_batch(
                r#"
                PRAGMA user_version = 1;
                CREATE TABLE note_directories (
                    id TEXT PRIMARY KEY,
                    parent_id TEXT,
                    title TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                );
                CREATE TABLE notes (
                    id TEXT PRIMARY KEY,
                    directory_id TEXT,
                    title TEXT NOT NULL,
                    content TEXT NOT NULL,
                    content_format TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL
                );
                "#,
            )
            .expect("旧笔记测试 schema 应能创建");
        connection
    }

    /// 验证旧 SQLite 目录和重名笔记会迁移为真实 Markdown 文件。
    #[test]
    fn 旧_sqlite_迁移为物理_markdown_目录并处理重名() {
        let root = migration_test_dir("plain");
        let database_path = root.join("notes.db");
        let target_root = root.join("notes");
        let connection = create_legacy_database(&database_path);
        connection
            .execute(
                "INSERT INTO note_directories (id, parent_id, title, created_at_ms, updated_at_ms) VALUES (?1, NULL, ?2, 1, 10)",
                params!["dir-1", "旧目录"],
            )
            .expect("旧目录应能写入测试数据库");
        for (id, content) in [("note-1", "第一篇"), ("note-2", "第二篇")] {
            connection
                .execute(
                    "INSERT INTO notes (id, directory_id, title, content, content_format, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, 1, 10)",
                    params![id, "dir-1", "同名", content, NoteContentFormat::PlainText.as_str()],
                )
                .expect("旧笔记应能写入测试数据库");
        }
        drop(connection);

        migrate_legacy_notes_database_to_directory(&database_path, &target_root).unwrap();

        assert_eq!(
            fs::read_to_string(target_root.join("旧目录").join("同名.md")).unwrap(),
            "第一篇"
        );
        assert_eq!(
            fs::read_to_string(target_root.join("旧目录").join("同名 (2).md")).unwrap(),
            "第二篇"
        );
        assert!(database_path.exists());
        let _ = fs::remove_dir_all(&root);
    }

    /// 验证旧富文本 JSON 会在迁移时导出为 Markdown。
    ///
    /// 业务意图：
    /// - 老版本用户的正文可能是富文本 JSON；迁移不能把 JSON 原文直接写入 `.md`，否则用户打开笔记会看到实现细节。
    #[test]
    fn 旧_sqlite_富文本_json_迁移为_markdown() {
        let root = migration_test_dir("rich");
        let database_path = root.join("notes.db");
        let target_root = root.join("notes");
        let connection = create_legacy_database(&database_path);
        let document = NoteRichTextDocument::from_plain_text("旧正文");
        let rich_text = document.to_json().unwrap();
        connection
            .execute(
                "INSERT INTO notes (id, directory_id, title, content, content_format, created_at_ms, updated_at_ms) VALUES (?1, NULL, ?2, ?3, ?4, 1, 10)",
                params!["note-rich", "富文本", rich_text, NoteContentFormat::RichText.as_str()],
            )
            .expect("旧富文本笔记应能写入测试数据库");
        drop(connection);

        migrate_legacy_notes_database_to_directory(&database_path, &target_root).unwrap();

        let migrated = fs::read_to_string(target_root.join("富文本.md")).unwrap();
        assert_eq!(migrated.trim(), "旧正文");
        let _ = fs::remove_dir_all(&root);
    }
}
