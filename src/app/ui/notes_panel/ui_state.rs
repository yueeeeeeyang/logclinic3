// 笔记 GPUI 面板状态和纯状态辅助函数。
//
// 业务意图：
// - 该文件只承载笔记树、当前笔记、只读选区、编辑草稿、确认弹窗和输入布局等 UI 状态。
// - 类型可见性限制在 app 模块内，避免把第一版笔记内部状态暴露成 crate 级 API。

use std::{cell::RefCell, collections::HashSet, ops::Range};

use super::*;

/// 当前选中的笔记树节点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct NotesTreeSelection {
    /// 节点 ID。
    pub(in crate::app) id: String,
    /// 节点类型。
    pub(in crate::app) kind: NoteTreeRowKind,
}

/// 笔记只读阅读模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum NoteReaderMode {
    /// Markdown 预览模式；普通文本不会使用该模式。
    Preview,
    /// 源码只读模式；支持精确复制原始文本。
    Source,
}

/// 笔记文本位置。
///
/// 业务意图：
/// - 只读源码阅读器不是系统文本控件，因此需要轻量位置类型保存跨行选择范围。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::app) struct NoteTextPosition {
    /// 0 基行号。
    pub(in crate::app) line_index: usize,
    /// 0 基字符列。
    pub(in crate::app) column: usize,
}

/// 笔记源码只读选择范围。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct NoteTextSelection {
    /// 鼠标按下锚点。
    pub(in crate::app) anchor: NoteTextPosition,
    /// 当前拖拽焦点。
    pub(in crate::app) focus: NoteTextPosition,
}

impl NoteTextSelection {
    /// 返回按文档顺序排列后的端点。
    pub(in crate::app) fn normalized(&self) -> (NoteTextPosition, NoteTextPosition) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// 判断选择是否为空。
    pub(in crate::app) fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }
}

/// 笔记编辑器中的单行排版缓存。
pub(in crate::app) struct NoteEditorLineLayout {
    /// 当前可视行对应的原始文本 UTF-8 字节范围。
    pub(in crate::app) byte_range: Range<usize>,
    /// 当前行的 GPUI 字形布局。
    pub(in crate::app) line: ShapedLine,
    /// 当前行在窗口中的绘制边界。
    pub(in crate::app) bounds: Bounds<Pixels>,
}

/// 笔记编辑器绘制状态。
pub(in crate::app) struct NoteEditorPrepaint {
    /// 当前帧需要绘制的所有文本行。
    pub(in crate::app) lines: Vec<NoteEditorPaintLine>,
    /// 当前选择范围对应的高亮矩形。
    pub(in crate::app) selections: Vec<PaintQuad>,
    /// 当前光标矩形。
    pub(in crate::app) cursor: Option<PaintQuad>,
}

/// 笔记编辑器单行绘制数据。
pub(in crate::app) struct NoteEditorPaintLine {
    /// 当前行对应的原始文本范围。
    pub(in crate::app) byte_range: Range<usize>,
    /// 当前行边界。
    pub(in crate::app) bounds: Bounds<Pixels>,
    /// 已排版的文本行。
    pub(in crate::app) line: ShapedLine,
}

/// 未保存修改确认动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum NotesUnsavedChoice {
    /// 保存当前草稿后继续原动作。
    Save,
    /// 放弃当前草稿后继续原动作。
    Discard,
    /// 取消原动作并留在当前笔记。
    Cancel,
}

/// 触发未保存确认的后续动作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum NotesPendingAction {
    /// 切换到另一个树节点。
    Select(NotesTreeSelection),
    /// 切换到主导航其它功能页。
    SwitchFeature(MainFeature),
    /// 创建目录；`None` 表示根目录，`Some` 表示指定目录下的子目录。
    CreateDirectory(Option<String>),
    /// 创建笔记；`None` 表示根层笔记，`Some` 表示指定目录下的笔记。
    CreateNote(Option<String>),
    /// 删除指定树节点。
    Delete(NotesTreeSelection),
    /// 重命名指定树节点。
    Rename(NotesTreeSelection),
}

/// 未保存修改确认弹窗状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct NotesUnsavedDialog {
    /// 用户确认后要继续执行的动作。
    pub(in crate::app) action: NotesPendingAction,
}

/// 删除确认弹窗状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct NotesDeleteConfirmDialog {
    /// 待删除节点。
    pub(in crate::app) target: NotesTreeSelection,
    /// 展示标题。
    pub(in crate::app) title: String,
}

/// 重命名弹窗状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct NotesRenameDialog {
    /// 待重命名节点。
    pub(in crate::app) target: NotesTreeSelection,
    /// 打开弹窗时的原始标题；用于判断用户是否真正修改，避免无意义写库。
    pub(in crate::app) original_title: String,
}

/// 笔记树右键菜单状态。
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct NotesTreeContextMenu {
    /// 右键落点对应的笔记树节点。
    pub(in crate::app) target: NotesTreeSelection,
    /// 菜单左上角相对笔记树面板的横坐标。
    pub(in crate::app) x: f32,
    /// 菜单左上角相对笔记树面板的纵坐标。
    pub(in crate::app) y: f32,
}

/// 笔记树右键菜单命令。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum NotesTreeContextMenuAction {
    /// 在目录下创建子目录。
    NewDirectory,
    /// 在目录下创建笔记。
    NewNote,
    /// 重命名目录或笔记。
    Rename,
    /// 删除目录或笔记。
    Delete,
}

/// 笔记 Markdown 预览缓存。
///
/// 业务意图：
/// - Markdown 预览会解析完整正文并对代码块做语法高亮，1 MiB 笔记在窗口重绘时重复解析会明显拖慢 UI。
/// - 第一版只缓存当前最近预览的一篇笔记，避免无上限持有多篇大笔记的解析结果。
#[derive(Clone)]
pub(in crate::app) struct NoteMarkdownPreviewCacheEntry {
    /// 缓存对应的笔记 ID。
    pub(in crate::app) note_id: String,
    /// 缓存对应的正文哈希；正文变化后必须重新解析。
    pub(in crate::app) content_hash: u64,
    /// 缓存对应的主题；主题变化后代码高亮颜色需要重新生成。
    pub(in crate::app) theme: EffectiveTheme,
    /// 已解析的 Markdown 展示文档。
    pub(in crate::app) document: AppMarkdownDocument,
}

/// 笔记页面完整工作区状态。
pub(in crate::app) struct NotesWorkspaceState {
    /// 完整笔记树行。
    pub(in crate::app) tree_rows: Vec<NoteTreeRow>,
    /// 当前按展开状态可见的笔记树行。
    pub(in crate::app) visible_rows: Vec<NoteTreeRow>,
    /// 展开的目录 ID 集合。
    pub(in crate::app) expanded_directory_ids: HashSet<String>,
    /// 当前选中节点。
    pub(in crate::app) selected: Option<NotesTreeSelection>,
    /// 当前右侧打开的笔记。
    pub(in crate::app) active_note: Option<Note>,
    /// 笔记树虚拟列表滚动句柄。
    pub(in crate::app) tree_scroll_handle: UniformListScrollHandle,
    /// 数据库错误。
    pub(in crate::app) database_error: Option<String>,
    /// 阅读器模式。
    pub(in crate::app) reader_mode: NoteReaderMode,
    /// 最近一次 Markdown 预览缓存。
    pub(in crate::app) markdown_preview_cache: RefCell<Option<NoteMarkdownPreviewCacheEntry>>,
    /// 只读源码选择范围。
    pub(in crate::app) source_selection: Option<NoteTextSelection>,
    /// 只读源码拖拽锚点。
    pub(in crate::app) source_selection_drag_anchor: Option<NoteTextPosition>,
    /// 当前是否处于编辑状态。
    pub(in crate::app) is_editing: bool,
    /// 编辑器正文草稿。
    pub(in crate::app) editor_text: String,
    /// 编辑器标题草稿；第一版标题保存时同步重命名笔记。
    pub(in crate::app) editor_title: String,
    /// 标题输入框选择范围。
    pub(in crate::app) title_selection_range: Range<usize>,
    /// 标题输入框输入法组合文本范围。
    pub(in crate::app) title_marked_range: Option<Range<usize>>,
    /// 标题输入框焦点句柄。
    pub(in crate::app) title_focus: gpui::FocusHandle,
    /// 标题输入框最近一次单行排版结果。
    pub(in crate::app) title_last_layout: Option<ShapedLine>,
    /// 标题输入框最近一次绘制边界。
    pub(in crate::app) title_last_bounds: Option<Bounds<Pixels>>,
    /// 编辑器格式草稿。
    pub(in crate::app) editor_format: NoteContentFormat,
    /// 编辑器选择范围。
    pub(in crate::app) editor_selection_range: Range<usize>,
    /// 编辑器输入法组合文本范围。
    pub(in crate::app) editor_marked_range: Option<Range<usize>>,
    /// 编辑器焦点句柄。
    pub(in crate::app) editor_focus: gpui::FocusHandle,
    /// 编辑器最近一次绘制的逐行布局。
    pub(in crate::app) editor_last_layouts: Vec<NoteEditorLineLayout>,
    /// 编辑器最近一次整体绘制边界。
    pub(in crate::app) editor_last_bounds: Option<Bounds<Pixels>>,
    /// 编辑器拖拽选择锚点。
    pub(in crate::app) editor_selection_drag: Option<usize>,
    /// 编辑器撤销栈。
    pub(in crate::app) editor_undo_stack: Vec<String>,
    /// 编辑器重做栈。
    pub(in crate::app) editor_redo_stack: Vec<String>,
    /// 未保存修改确认弹窗。
    pub(in crate::app) unsaved_dialog: Option<NotesUnsavedDialog>,
    /// 重命名弹窗。
    pub(in crate::app) rename_dialog: Option<NotesRenameDialog>,
    /// 笔记树右键菜单。
    pub(in crate::app) tree_context_menu: Option<NotesTreeContextMenu>,
    /// 删除确认弹窗。
    pub(in crate::app) delete_confirm_dialog: Option<NotesDeleteConfirmDialog>,
}

impl NotesWorkspaceState {
    /// 从 SQLite 创建笔记工作区状态。
    pub(in crate::app) fn load_or_initialize(context: &mut Context<MainView>) -> Self {
        let (tree_rows, database_error) = if let Some(path) = notes_database_path() {
            match load_note_tree(&path) {
                Ok(rows) => (rows, None),
                Err(error) => (Vec::new(), Some(error)),
            }
        } else {
            (
                Vec::new(),
                Some("当前平台没有可用的应用配置目录，无法保存笔记".to_string()),
            )
        };
        let mut state = Self {
            tree_rows,
            visible_rows: Vec::new(),
            expanded_directory_ids: HashSet::new(),
            selected: None,
            active_note: None,
            tree_scroll_handle: UniformListScrollHandle::new(),
            database_error,
            reader_mode: NoteReaderMode::Preview,
            markdown_preview_cache: RefCell::new(None),
            source_selection: None,
            source_selection_drag_anchor: None,
            is_editing: false,
            editor_text: String::new(),
            editor_title: String::new(),
            title_selection_range: 0..0,
            title_marked_range: None,
            title_focus: context.focus_handle(),
            title_last_layout: None,
            title_last_bounds: None,
            editor_format: NoteContentFormat::Markdown,
            editor_selection_range: 0..0,
            editor_marked_range: None,
            editor_focus: context.focus_handle(),
            editor_last_layouts: Vec::new(),
            editor_last_bounds: None,
            editor_selection_drag: None,
            editor_undo_stack: Vec::new(),
            editor_redo_stack: Vec::new(),
            unsaved_dialog: None,
            rename_dialog: None,
            tree_context_menu: None,
            delete_confirm_dialog: None,
        };
        state.initialize_expanded_directories();
        state.rebuild_visible_rows();
        state
    }

    /// 初始化默认展开目录。
    fn initialize_expanded_directories(&mut self) {
        self.expanded_directory_ids = self
            .tree_rows
            .iter()
            .filter(|row| {
                row.kind == NoteTreeRowKind::Directory
                    && row.has_children
                    && row.depth < NOTES_TREE_DEFAULT_EXPANDED_DEPTH
            })
            .map(|row| row.id.clone())
            .collect();
    }

    /// 按展开状态重建可见行。
    pub(in crate::app) fn rebuild_visible_rows(&mut self) {
        self.visible_rows.clear();
        let mut collapsed_depth: Option<usize> = None;
        for row in &self.tree_rows {
            if let Some(depth) = collapsed_depth {
                if row.depth > depth {
                    continue;
                }
                collapsed_depth = None;
            }
            self.visible_rows.push(row.clone());
            if row.kind == NoteTreeRowKind::Directory
                && row.has_children
                && !self.expanded_directory_ids.contains(&row.id)
            {
                collapsed_depth = Some(row.depth);
            }
        }
    }

    /// 判断当前编辑草稿是否存在未保存修改。
    pub(in crate::app) fn has_unsaved_changes(&self) -> bool {
        let Some(note) = &self.active_note else {
            return false;
        };
        self.is_editing
            && (self.editor_text != note.content
                || self.editor_title != note.title
                || self.editor_format != note.content_format)
    }
}
