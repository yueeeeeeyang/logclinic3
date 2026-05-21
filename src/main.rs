#![cfg_attr(
    // Windows release 包使用 GUI 子系统，避免 GPUI 桌面程序双击启动时额外出现控制台窗口；debug 保留控制台便于查看启动错误。
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

//! LogClinic 桌面客户端的最小程序入口。
//!
//! 业务意图：
//! - crate 根只负责声明功能模块并把启动控制权交给 `app` 模块，避免主入口继续承载 UI、搜索、日志树、
//!   日志查看器和线程分析等大量业务实现。
//! - 具体窗口状态、GPUI 渲染和跨平台打开文件逻辑都在 `app` 及后续功能域模块中维护，便于后续继续拆分。
//!
//! 跨平台约束：
//! - macOS 和 Windows 都从同一个 `main` 进入；平台差异必须留在对应功能模块中处理，不能重新堆回入口文件。

mod ai_chat;
mod app;
mod archive;
mod config;
mod connections;
mod highlighting;
mod hprof;
mod launch;
mod log_ai_analysis;
mod log_document;
mod log_loader;
mod log_source;
mod notes;
mod plugin;
mod search;
mod shell_integration;
mod theme;
mod thread_analysis;

/// 程序入口。
///
/// 业务意图：
/// - 保持可执行程序入口稳定，同时让真实应用装配迁移到 `app::run`。
/// - 这个函数不直接创建窗口、不读写配置、不处理日志来源，确保入口文件长期保持轻量。
fn main() {
    app::run();
}
