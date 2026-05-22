// 连接页 GPUI 模块聚合入口。
//
// 业务意图：
// - 连接功能包含持久化、SSH 后台任务、终端模拟器和 GPUI 渲染，UI 层按 actions/state/view 拆分，
//   避免继续扩张主窗口壳层文件。
// - 连接页只管理 SSH/SMB 等外部连接；本地终端已拆到独立主导航页，避免远程连接树承担本机 shell 生命周期。

use super::*;

mod actions;
mod file_manager_window;
mod ui_state;
mod view;

pub(in crate::app) use file_manager_window::*;
pub(in crate::app) use ui_state::*;
