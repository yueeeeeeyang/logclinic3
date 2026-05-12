// 笔记业务功能域入口。
//
// 业务意图：
// - 顶层 notes 模块承载笔记领域类型、树构建和 SQLite 持久化，不依赖 GPUI 或 `app`。
// - `app/ui/notes_panel` 只负责页面渲染、输入控件和 UI 事件适配，依赖方向固定为 `app -> notes`。

mod constants;
mod domain;
mod storage;

pub(crate) use constants::*;
pub(crate) use domain::*;
pub(crate) use storage::*;

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf};

    use rusqlite::Connection;

    use super::*;

    /// 构造唯一的笔记数据库测试路径。
    ///
    /// 业务意图：
    /// - 笔记数据库保存用户正文，测试必须使用临时路径，避免污染真实应用笔记。
    fn test_notes_database_file_path(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "logclinic3-notes-test-{}-{}",
            std::process::id(),
            name
        ))
    }

    /// 验证笔记数据库 schema 初始化和基础 CRUD。
    #[test]
    fn 笔记数据库初始化并读写目录和笔记() {
        let path = test_notes_database_file_path("roundtrip").join(NOTES_DATABASE_FILE_NAME);
        let root = create_note_directory(&path, None, "根目录".to_string()).unwrap();
        let child =
            create_note_directory(&path, Some(root.id.clone()), "子目录".to_string()).unwrap();
        let note = create_note(&path, Some(child.id.clone()), "新建笔记".to_string()).unwrap();
        update_note_content(&path, &note.id, "# 标题", NoteContentFormat::Markdown).unwrap();

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, NOTES_DATABASE_SCHEMA_VERSION);

        let loaded = load_note(&path, &note.id).unwrap().unwrap();
        assert_eq!(loaded.content, "# 标题");
        assert_eq!(loaded.content_format, NoteContentFormat::Markdown);
        let tree = load_note_tree(&path).unwrap();
        assert_eq!(tree.len(), 3);
        assert_eq!(tree[0].kind, NoteTreeRowKind::Directory);
        assert_eq!(tree[1].depth, 1);
        assert_eq!(tree[2].kind, NoteTreeRowKind::Note);

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证删除目录会级联删除子目录和笔记。
    #[test]
    fn 删除笔记目录会级联删除子内容() {
        let path = test_notes_database_file_path("cascade").join(NOTES_DATABASE_FILE_NAME);
        let root = create_note_directory(&path, None, "根目录".to_string()).unwrap();
        let child =
            create_note_directory(&path, Some(root.id.clone()), "子目录".to_string()).unwrap();
        let note = create_note(&path, Some(child.id.clone()), "笔记".to_string()).unwrap();

        delete_note_directory(&path, &root.id).unwrap();
        assert!(load_note_tree(&path).unwrap().is_empty());
        assert!(load_note(&path, &note.id).unwrap().is_none());

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证目录和笔记可独立重命名，且删除单篇笔记不会影响所在目录。
    #[test]
    fn 笔记和目录支持重命名以及单篇删除() {
        let path = test_notes_database_file_path("rename-delete").join(NOTES_DATABASE_FILE_NAME);
        let root = create_note_directory(&path, None, "旧目录".to_string()).unwrap();
        let note = create_note(&path, Some(root.id.clone()), "旧笔记".to_string()).unwrap();

        rename_note_directory(&path, &root.id, "新目录").unwrap();
        rename_note(&path, &note.id, "新笔记").unwrap();
        let tree = load_note_tree(&path).unwrap();
        assert_eq!(tree[0].title, "新目录");
        assert_eq!(tree[1].title, "新笔记");

        delete_note(&path, &note.id).unwrap();
        assert!(load_note(&path, &note.id).unwrap().is_none());
        let tree_after_delete = load_note_tree(&path).unwrap();
        assert_eq!(tree_after_delete.len(), 1);
        assert_eq!(tree_after_delete[0].id, root.id);

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证正文超过 1 MiB 时拒绝保存，并保留数据库中已有内容。
    #[test]
    fn 笔记内容超过上限不会写入数据库() {
        let path = test_notes_database_file_path("content-limit").join(NOTES_DATABASE_FILE_NAME);
        let note = create_note(&path, None, "容量测试".to_string()).unwrap();
        update_note_content(&path, &note.id, "原始内容", NoteContentFormat::PlainText).unwrap();

        let oversized = "A".repeat(NOTE_CONTENT_MAX_BYTES + 1);
        let error = update_note_content(&path, &note.id, &oversized, NoteContentFormat::PlainText)
            .unwrap_err();
        assert!(error.contains("超过"));
        let loaded = load_note(&path, &note.id).unwrap().unwrap();
        assert_eq!(loaded.content, "原始内容");

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证保存笔记会在同一次写入中更新标题、正文和格式。
    ///
    /// 业务意图：
    /// - UI 编辑器保存按钮允许标题和正文一起提交，存储层必须提供单次业务写入，避免半保存状态。
    #[test]
    fn 保存笔记会同时更新标题正文和格式() {
        let path = test_notes_database_file_path("atomic-update").join(NOTES_DATABASE_FILE_NAME);
        let note = create_note(&path, None, "旧标题".to_string()).unwrap();

        let updated_at_ms = update_note(
            &path,
            &note.id,
            "新标题",
            "新正文",
            NoteContentFormat::PlainText,
        )
        .unwrap();
        let loaded = load_note(&path, &note.id).unwrap().unwrap();
        assert_eq!(loaded.title, "新标题");
        assert_eq!(loaded.content, "新正文");
        assert_eq!(loaded.content_format, NoteContentFormat::PlainText);
        assert_eq!(loaded.updated_at_ms, updated_at_ms);

        let error = update_note(
            &path,
            "missing-note",
            "标题",
            "正文",
            NoteContentFormat::Markdown,
        )
        .unwrap_err();
        assert!(error.contains("不存在"));

        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    /// 验证同级排序遵循目录优先、更新时间倒序和标题升序。
    #[test]
    fn 笔记树同级排序稳定() {
        let directories = vec![
            NoteDirectory {
                id: "dir-b".to_string(),
                parent_id: None,
                title: "B".to_string(),
                created_at_ms: 1,
                updated_at_ms: 10,
            },
            NoteDirectory {
                id: "dir-a".to_string(),
                parent_id: None,
                title: "A".to_string(),
                created_at_ms: 1,
                updated_at_ms: 10,
            },
        ];
        let notes = vec![Note {
            id: "note-new".to_string(),
            directory_id: None,
            title: "N".to_string(),
            content: String::new(),
            content_format: NoteContentFormat::PlainText,
            created_at_ms: 1,
            updated_at_ms: 99,
        }];
        let rows = build_note_tree_rows(&directories, &notes);
        assert_eq!(rows[0].id, "dir-a");
        assert_eq!(rows[1].id, "dir-b");
        assert_eq!(rows[2].id, "note-new");
    }

    /// 验证未知内容格式会返回中文错误。
    #[test]
    fn 未知笔记格式会报错() {
        let error = NoteContentFormat::from_str("future").unwrap_err();
        assert!(error.contains("未知内容格式"));
    }
}
