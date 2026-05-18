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
/// 设置窗口插件页签视图。
mod plugin_view;
/// 设置页快搜关键字输入元素。
mod quick_search_input;
/// 设置窗口存储页签视图。
mod storage_view;
/// 设置页线程过滤输入元素。
mod thread_filter_input;
/// 设置窗口视图。
mod window_view;

pub(in crate::app) use window_view::SettingsWindowView;

/// 用系统文件管理器打开目录。
///
/// 业务意图：
/// - 插件页和存储页都需要把用户带到某个真实目录，统一跨平台实现可以避免 macOS/Windows 行为分歧。
///
/// 跨平台约束：
/// - macOS 使用 `open`，Windows 使用 `explorer`，其它平台尝试 `xdg-open`；失败时返回中文错误并由调用方展示。
pub(in crate::app) fn open_directory_in_platform_file_manager(path: &Path) -> Result<(), String> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = std::process::Command::new("open");
        command.arg(path);
        command
    } else if cfg!(target_os = "windows") {
        let mut command = std::process::Command::new("explorer");
        command.arg(path);
        command
    } else {
        let mut command = std::process::Command::new("xdg-open");
        command.arg(path);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("启动文件管理器失败：{error}"))
}
