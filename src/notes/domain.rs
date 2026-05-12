// 笔记业务领域类型和纯状态辅助函数。
//
// 业务意图：
// - 这里定义目录、笔记、内容格式和树行等不依赖 GPUI 的模型，供 SQLite 存储层和 UI 工作区共同使用。
// - 本模块不持有窗口、焦点、滚动条或排版缓存，避免 notes 业务域反向依赖 app 壳层。

use std::{
    collections::{HashMap, HashSet},
    sync::atomic::Ordering,
    time::{SystemTime, UNIX_EPOCH},
};

use super::*;

/// 笔记内容格式。
///
/// 业务意图：
/// - 普通文本和 Markdown 在保存层必须显式区分，避免 UI 只能通过标题、后缀或内容猜测展示方式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoteContentFormat {
    /// 普通纯文本，阅读器按源码逐行显示。
    PlainText,
    /// Markdown 文本，阅读器默认预览，同时允许切换到源码只读模式精确复制。
    Markdown,
}

impl NoteContentFormat {
    /// 返回写入 SQLite 的稳定字符串。
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PlainText => "plain_text",
            Self::Markdown => "markdown",
        }
    }

    /// 从 SQLite 字符串恢复笔记格式。
    ///
    /// 错误处理：
    /// - 数据库可能被用户或旧版本手工修改，未知格式直接返回中文错误，避免 UI 用错误规则展示正文。
    pub(crate) fn from_str(raw: &str) -> Result<Self, String> {
        match raw {
            "plain_text" => Ok(Self::PlainText),
            "markdown" => Ok(Self::Markdown),
            other => Err(format!("笔记数据库包含未知内容格式：{other}")),
        }
    }

    /// 返回 UI 中展示的中文名称。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::PlainText => "文本",
            Self::Markdown => "Markdown",
        }
    }
}

/// 笔记目录。
///
/// 业务意图：
/// - 目录只负责树结构和命名，不直接保存正文；正文始终属于笔记，避免“目录绑定一篇笔记”和“目录容器”语义混在一起。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NoteDirectory {
    /// 目录稳定 ID，作为子目录和笔记外键。
    pub(crate) id: String,
    /// 父目录 ID；`None` 表示根层目录。
    pub(crate) parent_id: Option<String>,
    /// 用户可见目录名。
    pub(crate) title: String,
    /// 创建时间，Unix epoch 毫秒。
    pub(crate) created_at_ms: i64,
    /// 最近更新时间，Unix epoch 毫秒，用于同级排序。
    pub(crate) updated_at_ms: i64,
}

/// 笔记正文。
///
/// 业务意图：
/// - 笔记保存标题、所属目录、格式和完整正文，是右侧阅读器/编辑器的最小持久化单元。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Note {
    /// 笔记稳定 ID。
    pub(crate) id: String,
    /// 所属目录 ID；`None` 表示根层笔记。
    pub(crate) directory_id: Option<String>,
    /// 用户可见标题。
    pub(crate) title: String,
    /// 原始正文，使用 UTF-8 保存到 SQLite。
    pub(crate) content: String,
    /// 正文格式。
    pub(crate) content_format: NoteContentFormat,
    /// 创建时间，Unix epoch 毫秒。
    pub(crate) created_at_ms: i64,
    /// 最近更新时间，Unix epoch 毫秒，用于同级排序。
    pub(crate) updated_at_ms: i64,
}

/// 笔记树节点类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoteTreeRowKind {
    /// 目录节点，可展开并创建子目录或笔记。
    Directory,
    /// 笔记节点，点击后在右侧打开正文。
    Note,
}

/// 左侧笔记树扁平行。
///
/// 业务意图：
/// - SQLite 保存的是父子关系，UI 虚拟列表需要按展开状态消费扁平行；该结构承载树节点展示所需的稳定信息。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NoteTreeRow {
    /// 节点 ID，目录和笔记分别在各自表内唯一。
    pub(crate) id: String,
    /// 父目录 ID；根层节点为 `None`。
    pub(crate) parent_id: Option<String>,
    /// 展示标题。
    pub(crate) title: String,
    /// 节点类型。
    pub(crate) kind: NoteTreeRowKind,
    /// 树深度，根层为 0。
    pub(crate) depth: usize,
    /// 是否存在子节点；只对目录有意义。
    pub(crate) has_children: bool,
    /// 最近更新时间，供 UI 辅助展示和测试排序。
    pub(crate) updated_at_ms: i64,
}

/// 当前 Unix epoch 毫秒。
///
/// 边界条件：
/// - 系统时间早于 epoch 时回退 0，避免排序字段写入负数后影响同级展示。
pub(crate) fn current_note_time_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// 生成笔记实体 ID。
pub(crate) fn new_note_entity_id(prefix: &str) -> String {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = NOTE_ENTITY_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{seed}-{sequence}")
}

/// 创建默认目录模型。
pub(crate) fn new_note_directory(parent_id: Option<String>, title: String) -> NoteDirectory {
    let now = current_note_time_millis();
    NoteDirectory {
        id: new_note_entity_id("note-directory"),
        parent_id,
        title,
        created_at_ms: now,
        updated_at_ms: now,
    }
}

/// 创建默认笔记模型。
pub(crate) fn new_note(directory_id: Option<String>, title: String) -> Note {
    let now = current_note_time_millis();
    Note {
        id: new_note_entity_id("note"),
        directory_id,
        title,
        content: String::new(),
        content_format: NoteContentFormat::Markdown,
        created_at_ms: now,
        updated_at_ms: now,
    }
}

/// 构建左侧笔记树扁平行。
///
/// 业务意图：
/// - 按“目录在前、笔记在后；同类型按更新时间倒序再按标题升序”生成稳定展示顺序。
/// - UI 展开/收起只需要在该完整行列表上过滤，不需要每次重新访问 SQLite。
pub(crate) fn build_note_tree_rows(
    directories: &[NoteDirectory],
    notes: &[Note],
) -> Vec<NoteTreeRow> {
    let mut directories_by_parent: HashMap<Option<String>, Vec<NoteDirectory>> = HashMap::new();
    for directory in directories {
        directories_by_parent
            .entry(directory.parent_id.clone())
            .or_default()
            .push(directory.clone());
    }

    let mut notes_by_parent: HashMap<Option<String>, Vec<Note>> = HashMap::new();
    for note in notes {
        notes_by_parent
            .entry(note.directory_id.clone())
            .or_default()
            .push(note.clone());
    }

    for children in directories_by_parent.values_mut() {
        sort_note_directories(children);
    }
    for children in notes_by_parent.values_mut() {
        sort_notes(children);
    }

    let mut rows = Vec::new();
    let mut visiting = HashSet::new();
    push_note_tree_children(
        None,
        0,
        &directories_by_parent,
        &notes_by_parent,
        &mut visiting,
        &mut rows,
    );
    rows
}

/// 递归压平指定父目录下的目录和笔记。
fn push_note_tree_children(
    parent_id: Option<String>,
    depth: usize,
    directories_by_parent: &HashMap<Option<String>, Vec<NoteDirectory>>,
    notes_by_parent: &HashMap<Option<String>, Vec<Note>>,
    visiting: &mut HashSet<String>,
    rows: &mut Vec<NoteTreeRow>,
) {
    if let Some(directories) = directories_by_parent.get(&parent_id) {
        for directory in directories {
            if !visiting.insert(directory.id.clone()) {
                continue;
            }
            let child_parent = Some(directory.id.clone());
            let has_children = directories_by_parent
                .get(&child_parent)
                .is_some_and(|children| !children.is_empty())
                || notes_by_parent
                    .get(&child_parent)
                    .is_some_and(|children| !children.is_empty());
            rows.push(NoteTreeRow {
                id: directory.id.clone(),
                parent_id: directory.parent_id.clone(),
                title: directory.title.clone(),
                kind: NoteTreeRowKind::Directory,
                depth,
                has_children,
                updated_at_ms: directory.updated_at_ms,
            });
            push_note_tree_children(
                child_parent,
                depth.saturating_add(1),
                directories_by_parent,
                notes_by_parent,
                visiting,
                rows,
            );
            visiting.remove(&directory.id);
        }
    }

    if let Some(notes) = notes_by_parent.get(&parent_id) {
        for note in notes {
            rows.push(NoteTreeRow {
                id: note.id.clone(),
                parent_id: note.directory_id.clone(),
                title: note.title.clone(),
                kind: NoteTreeRowKind::Note,
                depth,
                has_children: false,
                updated_at_ms: note.updated_at_ms,
            });
        }
    }
}

/// 按同级目录展示规则排序。
pub(crate) fn sort_note_directories(directories: &mut [NoteDirectory]) {
    directories.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.id.cmp(&right.id))
    });
}

/// 按同级笔记展示规则排序。
pub(crate) fn sort_notes(notes: &mut [Note]) {
    notes.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.id.cmp(&right.id))
    });
}
