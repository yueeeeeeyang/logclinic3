// 独立本地终端页 UI 状态。
//
// 业务意图：
// - 本文件只保存主窗口会话内的本地终端 tab、右键菜单和轮询状态。
// - 终端真实输入输出仍由 `LocalPtyBackend` 后台线程负责，GPUI 状态只能通过事件队列轮询更新，避免阻塞 UI 线程。
//
// 边界条件：
// - 本地终端不持久化，不和连接分类、SSH/SMB 配置或连接数据库耦合。
// - 关闭 tab 必须显式 shutdown 后端，否则 PTY 子进程可能继续运行。

use super::*;

/// 本地终端页完整工作区状态。
pub(in crate::app) struct TerminalWorkspaceState {
    /// 下一个本地终端 tab ID。
    ///
    /// 业务意图：
    /// - 每次新建或复制本地终端都创建独立 PTY，会话之间不能共享输入输出或进程生命周期。
    pub(in crate::app) next_tab_id: usize,
    /// 当前激活的本地终端 tab ID。
    pub(in crate::app) active_tab_id: Option<usize>,
    /// 已打开的本地终端 tab。
    pub(in crate::app) tabs: Vec<ConnectionTerminalTab>,
    /// tab 栏横向滚动句柄。
    pub(in crate::app) tab_bar_scroll_handle: ScrollHandle,
    /// tab 右键菜单。
    pub(in crate::app) tab_context_menu: Option<TerminalTabContextMenu>,
    /// 终端正文右键菜单。
    pub(in crate::app) terminal_context_menu: Option<TerminalContextMenu>,
    /// 页面底部或空态使用的中文状态消息。
    pub(in crate::app) status_message: Option<String>,
    /// 是否已经安排后台事件轮询。
    ///
    /// 线程约束：
    /// - 一个页面只需要一个 30ms 轮询循环；多个 tab 共用该循环，避免每个 PTY 都触发独立 UI 定时器。
    pub(in crate::app) terminal_poll_scheduled: bool,
}

impl TerminalWorkspaceState {
    /// 创建空的本地终端页状态。
    pub(in crate::app) fn new() -> Self {
        Self {
            next_tab_id: 1,
            active_tab_id: None,
            tabs: Vec::new(),
            tab_bar_scroll_handle: ScrollHandle::new(),
            tab_context_menu: None,
            terminal_context_menu: None,
            status_message: None,
            terminal_poll_scheduled: false,
        }
    }

    /// 返回当前激活 tab 的可变引用。
    pub(in crate::app) fn active_tab_mut(&mut self) -> Option<&mut ConnectionTerminalTab> {
        let active_tab_id = self.active_tab_id?;
        self.tabs.iter_mut().find(|tab| tab.id == active_tab_id)
    }

    /// 返回当前激活 tab 的只读引用。
    pub(in crate::app) fn active_tab(&self) -> Option<&ConnectionTerminalTab> {
        let active_tab_id = self.active_tab_id?;
        self.tabs.iter().find(|tab| tab.id == active_tab_id)
    }

    /// 关闭单个本地终端 tab。
    ///
    /// 边界条件：
    /// - 如果关闭的是激活 tab，优先激活同位置右侧 tab；没有右侧 tab 时回退到左侧 tab。
    pub(in crate::app) fn close_tab(&mut self, tab_id: usize) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.id == tab_id) {
            self.tabs[index].backend.shutdown();
            self.tabs.remove(index);
            if self.active_tab_id == Some(tab_id) {
                self.active_tab_id = self
                    .tabs
                    .get(index)
                    .or_else(|| {
                        index
                            .checked_sub(1)
                            .and_then(|previous| self.tabs.get(previous))
                    })
                    .map(|tab| tab.id);
            }
        }
        self.tab_context_menu = None;
        self.terminal_context_menu = None;
    }

    /// 关闭指定 tab 之外的其它本地终端 tab。
    pub(in crate::app) fn close_other_tabs(&mut self, tab_id: usize) {
        for tab in self.tabs.iter().filter(|tab| tab.id != tab_id) {
            tab.backend.shutdown();
        }
        self.tabs.retain(|tab| tab.id == tab_id);
        self.active_tab_id = self.tabs.first().map(|tab| tab.id);
        self.tab_context_menu = None;
        self.terminal_context_menu = None;
        self.tab_bar_scroll_handle
            .set_offset(point(px(0.0), px(0.0)));
    }

    /// 关闭全部本地终端 tab。
    pub(in crate::app) fn close_all_tabs(&mut self) {
        for tab in &self.tabs {
            tab.backend.shutdown();
        }
        self.tabs.clear();
        self.active_tab_id = None;
        self.tab_context_menu = None;
        self.terminal_context_menu = None;
        self.tab_bar_scroll_handle
            .set_offset(point(px(0.0), px(0.0)));
    }
}

/// 本地终端 tab 右键菜单状态。
pub(in crate::app) struct TerminalTabContextMenu {
    /// 菜单目标 tab ID。
    pub(in crate::app) tab_id: usize,
    /// 菜单在终端页工作区内部的横坐标。
    pub(in crate::app) x: f32,
    /// 菜单在终端页工作区内部的纵坐标。
    pub(in crate::app) y: f32,
}

/// 本地终端 tab 右键菜单动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum TerminalTabContextMenuAction {
    /// 复制当前 tab，打开一个新的独立本地终端。
    Duplicate,
    /// 关闭右键点击的 tab。
    Current,
    /// 关闭右键点击 tab 之外的其它 tab。
    OtherTabs,
    /// 关闭全部本地终端 tab。
    AllTabs,
}

/// 本地终端正文右键菜单状态。
pub(in crate::app) struct TerminalContextMenu {
    /// 菜单目标 tab ID。
    pub(in crate::app) tab_id: usize,
    /// 菜单在终端页工作区内部的横坐标。
    pub(in crate::app) x: f32,
    /// 菜单在终端页工作区内部的纵坐标。
    pub(in crate::app) y: f32,
}

/// 本地终端正文右键菜单动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum TerminalContextMenuAction {
    /// 打开本机文件管理窗口。
    FileManager,
    /// 复制当前终端选区。
    Copy,
    /// 粘贴系统剪贴板内容。
    Paste,
}
