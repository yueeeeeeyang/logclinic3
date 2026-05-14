// 笔记 GPUI 面板状态和纯状态辅助函数。
//
// 业务意图：
// - 该文件只承载笔记树、当前笔记、只读选区、编辑草稿、确认弹窗和输入布局等 UI 状态。
// - 类型可见性限制在 app 模块内，避免把第一版笔记内部状态暴露成 crate 级 API。

use std::{collections::HashSet, ops::Range};

use super::*;

/// 当前选中的笔记树节点。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct NotesTreeSelection {
    /// 节点 ID。
    pub(in crate::app) id: String,
    /// 节点类型。
    pub(in crate::app) kind: NoteTreeRowKind,
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
    #[allow(dead_code)]
    pub(in crate::app) fn normalized(&self) -> (NoteTextPosition, NoteTextPosition) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// 判断选择是否为空。
    #[allow(dead_code)]
    pub(in crate::app) fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }
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

/// 笔记树宽度拖拽状态。
///
/// 业务意图：
/// - 用户按住笔记树右侧边界拖动时，需要记录按下时的窗口横坐标和起始宽度，确保宽度变化与鼠标位移线性一致。
/// - 状态只在当前主窗口生命周期内有效，不写入磁盘，避免临时排版调整影响下一次打开应用。
#[derive(Clone, Copy, Debug)]
pub(in crate::app) struct NotesTreeResizeDrag {
    /// 鼠标按下时的窗口横坐标。
    pub(in crate::app) start_x: Pixels,
    /// 鼠标按下时的笔记树宽度。
    pub(in crate::app) start_width: f32,
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
    /// 笔记树当前宽度。
    ///
    /// 业务意图：
    /// - 默认值和 AI 对话左侧栏一致，拖拽后在当前会话内即时生效。
    /// - 宽度不持久化，避免用户一次临时查看长目录名后影响后续启动的标准布局。
    pub(in crate::app) tree_width: f32,
    /// 笔记树宽度拖拽状态。
    ///
    /// 边界条件：
    /// - `None` 表示普通鼠标移动不会改变布局；只有从右侧拖拽命中区按下后才进入拖拽模式。
    pub(in crate::app) tree_resize_drag: Option<NotesTreeResizeDrag>,
    /// 笔记树虚拟列表滚动句柄。
    pub(in crate::app) tree_scroll_handle: UniformListScrollHandle,
    /// 数据库错误。
    pub(in crate::app) database_error: Option<String>,
    /// 只读源码选择范围。
    pub(in crate::app) source_selection: Option<NoteTextSelection>,
    /// 只读源码拖拽锚点。
    pub(in crate::app) source_selection_drag_anchor: Option<NoteTextPosition>,
    /// 当前是否处于编辑状态。
    pub(in crate::app) is_editing: bool,
    /// 富文本正文编辑器状态。
    ///
    /// 业务意图：
    /// - 该状态保存正文文档、线性选区、IME 组合区、待输入样式、撤销重做和排版缓存。
    /// - 数据库仍只在保存按钮或未保存确认“保存”路径写入，避免编辑器输入过程产生隐式持久化。
    pub(in crate::app) rich_editor: RichTextEditorState,
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
    /// 未保存修改确认弹窗。
    pub(in crate::app) unsaved_dialog: Option<NotesUnsavedDialog>,
    /// 重命名弹窗。
    pub(in crate::app) rename_dialog: Option<NotesRenameDialog>,
    /// 笔记树右键菜单。
    pub(in crate::app) tree_context_menu: Option<NotesTreeContextMenu>,
    /// 笔记树顶部新增菜单是否打开。
    ///
    /// 业务意图：
    /// - 左侧工具栏只保留一个“新增”入口，点击后再选择新建目录或新建笔记，降低工具栏图标密度。
    /// - 菜单是临时 UI 状态，不能影响树选择、编辑草稿和持久化数据。
    pub(in crate::app) tree_create_menu_open: bool,
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
            tree_width: NOTES_TREE_DEFAULT_WIDTH,
            tree_resize_drag: None,
            tree_scroll_handle: UniformListScrollHandle::new(),
            database_error,
            source_selection: None,
            source_selection_drag_anchor: None,
            is_editing: false,
            rich_editor: RichTextEditorState::new(context),
            editor_title: String::new(),
            title_selection_range: 0..0,
            title_marked_range: None,
            title_focus: context.focus_handle(),
            title_last_layout: None,
            title_last_bounds: None,
            unsaved_dialog: None,
            rename_dialog: None,
            tree_context_menu: None,
            tree_create_menu_open: false,
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
        let serialized = self.rich_editor.serialized_content().unwrap_or_default();
        Self::note_editor_has_unsaved_changes(
            self.is_editing,
            &self.editor_title,
            &serialized,
            note,
        )
    }

    /// 判断给定笔记草稿是否需要保存。
    ///
    /// 业务意图：
    /// - 笔记模块已改为富文本；旧纯文本和 Markdown 笔记进入编辑态后即使标题和正文未变，
    ///   保存也应把格式转换为富文本 JSON，因此旧格式本身也算未保存修改。
    /// - 抽成纯函数便于测试，不需要启动真实 GPUI 窗口或构造输入控件实体。
    pub(in crate::app) fn note_editor_has_unsaved_changes(
        is_editing: bool,
        editor_title: &str,
        serialized_rich_text: &str,
        note: &Note,
    ) -> bool {
        is_editing
            && (serialized_rich_text != note.content
                || editor_title != note.title
                || note.content_format != NoteContentFormat::RichText)
    }
}
