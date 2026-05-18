// 笔记业务功能域入口。
//
// 业务意图：
// - 顶层 notes 模块承载笔记领域类型、树构建和物理 Markdown 文件持久化，不依赖 GPUI 或 `app`。
// - `app/ui/notes_panel` 只负责页面渲染、输入控件和 UI 事件适配，依赖方向固定为 `app -> notes`。

mod constants;
mod domain;
mod rich_text;
mod storage;

pub(crate) use constants::*;
pub(crate) use domain::*;
pub(crate) use rich_text::*;
pub(crate) use storage::*;

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf};

    use super::*;

    /// 构造唯一的笔记根目录测试路径。
    ///
    /// 业务意图：
    /// - 物理文件存储会创建真实目录和 Markdown 文件，测试必须使用临时路径，避免污染真实应用笔记。
    fn test_notes_root_dir(name: &str) -> PathBuf {
        let root = env::temp_dir().join(format!(
            "logclinic3-notes-test-{}-{}",
            std::process::id(),
            name
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    /// 验证物理 Markdown 存储初始化和基础 CRUD。
    #[test]
    fn 笔记物理目录初始化并读写目录和笔记() {
        let path = test_notes_root_dir("roundtrip");
        let root = create_note_directory(&path, None, "根目录".to_string()).unwrap();
        let child =
            create_note_directory(&path, Some(root.id.clone()), "子目录".to_string()).unwrap();
        let note = create_note(&path, Some(child.id.clone()), "新建笔记".to_string()).unwrap();
        assert_eq!(note.content_format, NoteContentFormat::Markdown);
        assert!(
            path.join("根目录")
                .join("子目录")
                .join("新建笔记.md")
                .exists()
        );
        update_note_content(&path, &note.id, "# 标题", NoteContentFormat::Markdown).unwrap();

        let loaded = load_note(&path, &note.id).unwrap().unwrap();
        assert_eq!(loaded.content, "# 标题");
        assert_eq!(loaded.content_format, NoteContentFormat::Markdown);
        let tree = load_note_tree(&path).unwrap();
        assert_eq!(tree.len(), 3);
        assert_eq!(tree[0].kind, NoteTreeRowKind::Directory);
        assert_eq!(tree[1].depth, 1);
        assert_eq!(tree[2].kind, NoteTreeRowKind::Note);

        let _ = fs::remove_dir_all(&path);
    }

    /// 验证扫描只识别 Markdown 文件并忽略隐藏项和非 Markdown 文件。
    #[test]
    fn 笔记扫描只识别_markdown_文件() {
        let path = test_notes_root_dir("scan");
        fs::create_dir_all(path.join("目录")).unwrap();
        fs::write(path.join("目录").join("可见.md"), "正文").unwrap();
        fs::write(path.join("目录").join("兼容.markdown"), "正文").unwrap();
        fs::write(path.join("目录").join("忽略.txt"), "正文").unwrap();
        fs::create_dir_all(path.join(".hidden")).unwrap();
        fs::write(path.join(".hidden").join("隐藏.md"), "正文").unwrap();

        let tree = load_note_tree(&path).unwrap();
        let note_titles = tree
            .iter()
            .filter(|row| row.kind == NoteTreeRowKind::Note)
            .map(|row| row.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(note_titles.len(), 2);
        assert!(note_titles.contains(&"可见"));
        assert!(note_titles.contains(&"兼容"));

        let _ = fs::remove_dir_all(&path);
    }

    /// 验证目录和笔记可独立重命名，并处理同名冲突。
    #[test]
    fn 笔记和目录支持重命名以及同名冲突() {
        let path = test_notes_root_dir("rename");
        let root = create_note_directory(&path, None, "旧目录".to_string()).unwrap();
        let note = create_note(&path, Some(root.id.clone()), "旧笔记".to_string()).unwrap();
        let _conflict = create_note(&path, Some(root.id.clone()), "新笔记".to_string()).unwrap();

        let next_note_id = rename_note(&path, &note.id, "新笔记").unwrap();
        let next_root_id = rename_note_directory(&path, &root.id, "新目录").unwrap();
        let tree = load_note_tree(&path).unwrap();
        assert_eq!(tree[0].title, "新目录");
        assert_eq!(next_root_id, "新目录");
        assert_eq!(next_note_id, "旧目录/新笔记 (2).md");
        assert!(tree.iter().any(|row| row.id == "新目录/新笔记 (2).md"));

        let _ = fs::remove_dir_all(&path);
    }

    /// 验证正文超过 1 MiB 时拒绝保存，并保留文件中已有内容。
    #[test]
    fn 笔记内容超过上限不会写入文件() {
        let path = test_notes_root_dir("content-limit");
        let note = create_note(&path, None, "容量测试".to_string()).unwrap();
        update_note_content(&path, &note.id, "原始内容", NoteContentFormat::PlainText).unwrap();

        let oversized = "A".repeat(NOTE_CONTENT_MAX_BYTES + 1);
        let error = update_note_content(&path, &note.id, &oversized, NoteContentFormat::PlainText)
            .unwrap_err();
        assert!(error.contains("超过"));
        let loaded = load_note(&path, &note.id).unwrap().unwrap();
        assert_eq!(loaded.content, "原始内容");

        let _ = fs::remove_dir_all(&path);
    }

    /// 验证保存笔记会更新标题、正文和文件路径。
    ///
    /// 业务意图：
    /// - UI 编辑器保存按钮允许标题和正文一起提交；物理文件存储下标题变化必须返回新的路径 ID。
    #[test]
    fn 保存笔记会同时更新标题正文和路径() {
        let path = test_notes_root_dir("atomic-update");
        let note = create_note(&path, None, "旧标题".to_string()).unwrap();

        let updated = update_note(
            &path,
            &note.id,
            "新标题",
            "新正文",
            NoteContentFormat::PlainText,
        )
        .unwrap();
        let loaded = load_note(&path, &updated.id).unwrap().unwrap();
        assert_eq!(loaded.title, "新标题");
        assert_eq!(loaded.content, "新正文");
        assert_eq!(loaded.content_format, NoteContentFormat::Markdown);
        assert_eq!(updated.id, "新标题.md");

        let error = update_note(
            &path,
            "missing-note",
            "标题",
            "正文",
            NoteContentFormat::Markdown,
        )
        .unwrap_err();
        assert!(error.contains("不存在"));

        let _ = fs::remove_dir_all(&path);
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
        assert_eq!(
            NoteContentFormat::from_str("rich_text").unwrap(),
            NoteContentFormat::RichText
        );
        let error = NoteContentFormat::from_str("future").unwrap_err();
        assert!(error.contains("未知内容格式"));
    }

    /// 验证非法富文本 JSON 会返回中文错误。
    ///
    /// 业务意图：
    /// - 旧 SQLite 迁移可能遇到损坏富文本 JSON；解析层必须返回可展示的中文错误，而不是 panic。
    #[test]
    fn 非法富文本_json_会报中文错误() {
        let note = Note {
            id: "note-broken".to_string(),
            directory_id: None,
            title: "损坏".to_string(),
            content: "{broken".to_string(),
            content_format: NoteContentFormat::RichText,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let error = rich_text_document_from_note(&note).unwrap_err();
        assert!(error.contains("解析富文本笔记失败"));
    }
}
