// 设置窗口 GPUI 聚合模块。
//
// 业务意图：
// - 将设置窗口框架、页签视图、模型配置输入和保存动作集中在 `settings_panel`。
// - 配置文件路径、解析、默认值和读写逻辑仍由顶层 `crate::config` 业务域负责。

use super::*;

/// 设置窗口关于页签视图。
mod about_view;
/// 设置行为适配。
mod actions;
/// 设置窗口通用页签视图。
mod general_view;
/// 设置窗口日志页签视图。
mod log_view;
/// 设置页模型配置输入元素。
mod model_input;
/// 设置窗口模型页签视图。
mod model_view;
/// 设置页快搜关键字输入元素。
mod quick_search_input;
/// 设置页线程过滤输入元素。
mod thread_filter_input;
/// 设置窗口视图。
mod window_view;

pub(in crate::app) use window_view::SettingsWindowView;
