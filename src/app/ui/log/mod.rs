// 日志工作区 GPUI 聚合模块。
//
// 业务意图：
// - 将日志 tab、左侧目录树、正文渲染、选区、右键菜单和滚动条适配放在同一 UI 子域。
// - 这里不处理日志读取和解码规则；具体来源、分页和编码仍由顶层业务模块负责。

use super::*;

/// 日志正文单行视图。
mod line_view;
/// 日志 tab 行为适配。
mod tab_actions;
/// 日志 tab 栏视图。
mod tab_bar_view;
/// 日志 tab 右键菜单。
mod tab_menu;
/// 日志正文选中文本和复制辅助。
mod text_selection;
/// 日志正文鼠标选区行为。
mod text_selection_actions;
/// 左侧日志树搜索输入元素。
mod tree_search_input;
/// 左侧日志树视图。
mod tree_view;
/// 日志正文右键菜单。
mod viewer_menu;
/// 日志正文滚动条和分页滚动辅助。
mod viewer_scrollbar;
/// 日志正文视图。
mod viewer_view;

use tree_search_input::*;
