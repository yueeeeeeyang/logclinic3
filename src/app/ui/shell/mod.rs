// 主窗口 GPUI 外壳聚合模块。
//
// 业务意图：
// - 将主窗口布局常量、共享 UI 类型、导航、键盘、输入适配和工作区协调集中在 `shell` 子域。
// - 子模块只服务于 `MainView` 根实体，不向日志读取、搜索、配置或 HPROF 业务域暴露 UI 细节。

use super::*;

/// 主窗口输入适配。
mod input_adapter;
/// 主窗口键盘快捷键适配。
mod keyboard;
/// 主窗口布局常量。
mod layout;
/// 主窗口状态组合。
mod main_view_state;
/// 主窗口导航视图。
mod navigation_view;
/// 主窗口 UI 类型。
mod types;
/// 工作区鼠标事件适配。
mod workspace_events;
/// 工作区视图。
mod workspace_view;

pub(in crate::app) use layout::*;
pub(in crate::app) use main_view_state::*;
pub(in crate::app) use types::*;
