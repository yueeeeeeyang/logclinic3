// LogClinic GPUI 壳层模块聚合入口。
//
// 业务意图：
// - `app/ui` 只承载主窗口、独立窗口、输入元素、菜单和页面渲染等 GPUI 适配代码。
// - 顶层业务域继续保留在 `crate::ai_chat`、`crate::search`、`crate::thread_analysis`、`crate::hprof`
//   等模块中，避免 UI 文件名和业务域文件名混淆。
// - 本模块集中声明 UI 子模块，并把 `app` 根模块仍需使用的根视图类型、常量和状态类型受控重导出。

use super::*;

/// HPROF 和线程分析 GPUI 视图。
mod analysis_panel;
/// AI 对话 GPUI 面板。
#[path = "ai_chat_panel/mod.rs"]
mod chat_panel;
/// 日志工作区 GPUI 视图和动作。
mod log;
/// 应用内 Markdown 解析与渲染。
mod markdown;
/// 笔记 GPUI 面板。
mod notes_panel;
/// 插件声明式窗口和命令调度。
mod plugin_panel;
/// 搜索弹窗和结果面板 GPUI 适配。
mod search_panel;
/// 设置窗口 GPUI 视图和动作。
mod settings_panel;
/// 主窗口外壳、导航、输入和工作区协调。
mod shell;

#[cfg(test)]
pub(in crate::app) use analysis_panel::ThreadStackWindowView;
pub(in crate::app) use analysis_panel::{
    HprofAnalysisView, LOG_AI_ANALYSIS_WINDOW_HEIGHT, LOG_AI_ANALYSIS_WINDOW_WIDTH,
    LogAiAnalysisWindowView, SearchResultsResizeDrag, SearchTarget, ThreadAnalysisWindowView,
};
pub(in crate::app) use chat_panel::*;
#[cfg(test)]
pub(in crate::app) use log::LogMinimapScrollInfo;
pub(in crate::app) use markdown::*;
pub(in crate::app) use notes_panel::*;
pub(in crate::app) use plugin_panel::PluginPageWindowView;
pub(in crate::app) use search_panel::SearchDialogWindowView;
pub(in crate::app) use settings_panel::SettingsWindowView;
pub(in crate::app) use shell::*;
