// 连接页 GPUI 模块聚合入口。
//
// 业务意图：
// - 连接功能包含持久化、SSH 后台任务、终端模拟器和 GPUI 渲染，UI 层按 actions/state/view 拆分，
//   避免继续扩张主窗口壳层文件。
// - 连接页同时显示 SSH 配置列表和本地终端入口，状态层面向统一终端后端，避免 UI 分叉处理输入和渲染。

use super::*;

mod actions;
mod ui_state;
mod view;

pub(in crate::app) use ui_state::*;
