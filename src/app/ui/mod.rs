// LogClinic GPUI 壳层模块聚合入口。
//
// 业务意图：
// - `app/ui` 只承载主窗口、独立窗口、输入元素、菜单和页面渲染等 GPUI 适配代码。
// - 顶层业务域继续保留在 `crate::ai_chat`、`crate::search`、`crate::thread_analysis`、`crate::hprof`
//   等模块中，避免 UI 文件名和业务域文件名混淆。
// - 本模块集中声明 UI 子模块，并把 `app` 根模块仍需使用的根视图类型、常量和状态类型受控重导出。

use super::*;

/// 设置窗口关于页签视图。
mod about_settings_view;
/// AI 对话 GPUI 面板。
#[path = "ai_chat_panel/mod.rs"]
mod chat_panel;
/// HPROF dump 分析 GPUI 视图。
mod hprof_analysis_view;
/// 日志正文单行视图。
mod log_line_view;
/// 日志 tab 行为适配。
mod log_tab_actions;
/// 日志 tab 栏视图。
mod log_tab_bar_view;
/// 日志 tab 右键菜单。
mod log_tab_menu;
/// 日志正文选中文本和复制辅助。
mod log_text_selection;
/// 日志正文鼠标选区行为。
mod log_text_selection_actions;
/// 左侧日志树视图。
mod log_tree_view;
/// 日志正文右键菜单。
mod log_viewer_menu;
/// 日志正文滚动条和分页滚动辅助。
mod log_viewer_scrollbar;
/// 日志正文视图。
mod log_viewer_view;
/// 主窗口输入适配。
mod main_input_adapter;
/// 主窗口键盘快捷键适配。
mod main_keyboard;
/// 主窗口导航视图。
mod main_navigation_view;
/// 主视图 UI 状态。
mod main_view_state;
/// 搜索行为适配。
mod search_actions;
/// 搜索对话框视图。
mod search_dialog_view;
/// 搜索输入元素。
mod search_input_element;
/// 搜索结果面板视图。
mod search_results_panel;
/// 设置行为适配。
mod settings_actions;
/// 设置窗口通用页签视图。
mod settings_general_view;
/// 设置窗口日志页签视图。
mod settings_log_view;
/// 设置页模型配置输入元素。
mod settings_model_input;
/// 设置窗口模型页签视图。
mod settings_model_view;
/// 设置页快搜关键字输入元素。
mod settings_quick_search_input;
/// 设置页线程过滤输入元素。
mod settings_thread_filter_input;
/// 设置窗口视图。
mod settings_window_view;
/// 主窗口布局常量。
mod shell_layout;
/// 主窗口 UI 类型。
mod shell_types;
/// 线程分析独立窗口视图。
#[path = "thread_analysis_window_view.rs"]
mod thread_dump_window_view;
/// 工作区鼠标事件适配。
mod workspace_events;
/// 工作区视图。
mod workspace_view;

pub(in crate::app) use chat_panel::*;
pub(in crate::app) use hprof_analysis_view::HprofAnalysisView;
pub(in crate::app) use main_view_state::*;
pub(in crate::app) use search_dialog_view::SearchDialogWindowView;
pub(in crate::app) use settings_window_view::SettingsWindowView;
pub(in crate::app) use shell_layout::*;
pub(in crate::app) use shell_types::*;
pub(in crate::app) use thread_dump_window_view::{
    SearchResultsResizeDrag, SearchTarget, ThreadAnalysisWindowView,
};
