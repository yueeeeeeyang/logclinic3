// 搜索弹窗 GPUI 聚合模块。
//
// 业务意图：
// - 将搜索弹窗输入、任务触发、结果面板和跳转交互放在 `search_panel`，避免与顶层 `crate::search` 业务域重名。
// - 搜索算法、分页搜索和结果模型仍由顶层搜索业务域提供。

use super::*;

/// 搜索行为适配。
mod actions;
/// 搜索对话框视图。
mod dialog_view;
/// 搜索输入元素。
mod input_element;
/// 搜索结果面板视图。
mod results_panel;

pub(in crate::app) use dialog_view::SearchDialogWindowView;
