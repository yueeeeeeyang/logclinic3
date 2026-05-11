//! LogClinic 桌面客户端的应用装配与主视图协调模块。
//!
//! 业务意图：
//! - 该模块承接从历史 `main.rs` 中迁出的主窗口装配、根状态协调和全局事件分发。
//! - 功能域较大的 UI 代码已经拆到 `app` 目录子模块，由正常 `mod` 边界约束跨域访问。
//! - 这里仍保留跨域协调逻辑，例如窗口句柄、后台任务回调、主视图状态汇总和全局快捷键。
//!
//! 跨平台约束：
//! - macOS 和 Windows 都需要从同一个应用装配入口启动主窗口，平台打开文件和窗口尺寸差异由专门函数处理。
//! - 窗口尺寸使用 GPUI 的逻辑像素表达，由 GPUI 负责映射到具体平台窗口系统。
//! - 当前只持久化主窗口宽高、主题偏好和日志字号，不持久化窗口位置，避免跨显示器恢复造成窗口不可见。

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeSet, HashSet},
    env, fs,
    ops::{Deref, DerefMut, Range},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::archive::{
    cleanup_materialized_file, cleanup_stale_large_log_cache, materialize_source_for_paging,
};
use crate::config::*;
use crate::highlighting::{SyntaxTheme, highlight_line};
use crate::launch::{log_source_paths_from_launch_arguments, log_source_paths_from_open_urls};
use crate::log_document;
use crate::log_document::{EncodingChoice, LogContentError, LogTextEncoding, decode_log_bytes};
use crate::log_document::{LargeLogOpenResult, LogTabDocument, open_log_source_for_tab};
use crate::search::{
    SearchFileError, SearchMatchMode, SearchOptions, SearchProgress, SearchResultItem, SearchScope,
    collect_current_directory_sources, count_query_occurrences, search_lines,
    source_location_label,
};
use crate::search::{count_query_occurrences_paged, search_paged_document};
use crate::theme::{AppThemePalette, EffectiveTheme, ThemePreference};
use crate::thread_analysis::{
    ThreadAnalysisData, ThreadAnalysisFilterRule, ThreadStateKind, ThreadTimelineCell,
    analyze_thread_dump_sources, parse_thread_analysis_filter_rules,
    visible_thread_indexes_for_state_kinds,
};
#[cfg(test)]
use crate::thread_analysis::{
    ThreadSnapshot, ThreadStateSample, build_thread_analysis_data, parse_thread_dump_snapshots,
    thread_stack_matches_filter_rule,
};
use crate::{
    log_loader::{
        LoadedLogTree, LogTreeEntryKind, LogTreeRow as LoadedLogTreeRow, load_log_sources,
    },
    log_source::LogFileSource,
};
use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt as _, AnyWindowHandle, App, AppContext, Application, AsyncApp, Bounds,
    ClickEvent, ClipboardItem, Context, DisplayId, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, ExternalPaths, FontWeight, GlobalElementId, InteractiveElement,
    IntoElement, KeyBinding, KeyDownEvent, Keystroke, LayoutId, ListAlignment,
    ListHorizontalSizingBehavior, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, ParentElement, PathPromptOptions, Pixels, Point, Render, ScrollHandle,
    ScrollStrategy, ScrollWheelEvent, ShapedLine, SharedString, StatefulInteractiveElement, Style,
    Styled as _, StyledText, TextRun, TitlebarOptions, UTF16Selection, UnderlineStyle,
    UniformListScrollHandle, Window, WindowAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, actions, div, fill, list, point, px, relative, rgb, size, uniform_list,
};
use lucide_icons::{Icon, LUCIDE_FONT_BYTES};
use main_view::MainView;

actions!(logclinic, [OpenSearchDialog]);

/// 设置窗口关于页签功能域。
mod about_window;
/// AI 对话页面、持久化和流式请求功能域。
mod ai_chat;
/// 主窗口 UI 常量。
mod constants;
/// HPROF dump 分析独立窗口功能域。
mod hprof_analysis;
/// 主窗口输入、剪贴板和文本选择适配。
mod input;
/// 主窗口键盘快捷键和键盘滚动协调。
mod keyboard;
/// 日志正文单行渲染与标记辅助。
mod log_line_render;
/// 日志正文选区、复制和高亮坐标映射。
mod log_selection;
/// 日志 tab 栏和编码菜单渲染。
mod log_tab_bar;
/// 日志来源打开、tab 控制和编码重载协调。
mod log_tab_controller;
/// 左侧日志目录树功能域。
mod log_tree_methods;
/// 日志正文右键菜单协调。
mod log_viewer_context_menu;
/// 日志正文查看器和 tab 正文功能域。
mod log_viewer_methods;
/// 日志正文滚动条和分页滚动辅助。
mod log_viewer_scrollbars;
/// 日志正文文本选择辅助。
mod log_viewer_selection;
/// 主窗口根实体和构造功能域。
mod main_view;
/// 主窗口导航、工具栏和加载入口协调。
mod navigation;
/// 主窗口搜索任务和搜索窗口状态编排。
mod search_controller;
/// 搜索窗口自绘输入元素。
mod search_input_element;
/// 主窗口搜索结果面板功能域。
mod search_results_methods;
/// 搜索独立窗口功能域。
mod search_window;
/// 设置页输入状态和表单行为适配。
mod settings_controller;
/// 设置窗口通用页签渲染。
mod settings_general_tab;
/// 设置窗口日志页签渲染。
mod settings_log_tab;
/// 设置页模型配置自绘输入元素。
mod settings_model_input;
/// 设置窗口模型页签渲染。
mod settings_model_tab;
/// 设置页快搜关键字自绘输入元素。
mod settings_quick_search_input;
/// 设置页线程过滤自绘输入元素。
mod settings_thread_filter_input;
/// 设置独立窗口功能域。
mod settings_window;
/// 主窗口拆分后的纯状态容器。
mod state;
/// 日志 tab 右键菜单和 tab 关闭动作。
mod tab_context_menu;
/// 应用层纯状态测试。
#[cfg(test)]
mod tests;
/// 线程日志分析独立窗口功能域。
mod thread_analysis;
/// 主窗口纯 UI 状态类型。
mod types;
/// 主窗口运行时和启动装配功能域。
mod window_runtime;
/// 主窗口日志工作区、搜索工作区和页面渲染协调。
mod workspace;
/// 主工作区鼠标事件和面板尺寸协调。
mod workspace_events;
pub(crate) use window_runtime::run;
#[cfg(test)]
pub(crate) use window_runtime::{MainWindowStartupDecision, decide_main_window_startup};

use ai_chat::*;
use constants::*;
use hprof_analysis::HprofAnalysisView;
use search_window::SearchDialogWindowView;
use settings_window::SettingsWindowView;
use state::*;
use thread_analysis::{SearchResultsResizeDrag, SearchTarget, ThreadAnalysisWindowView};
use types::*;

impl Render for MainView {
    /// 渲染主窗口内容。
    ///
    /// 实现原因：
    /// - 左侧固定大导航提供全局入口，右侧根据当前主功能显示日志分析、HPROF 解析或 AI 对话占位页。
    /// - 日志加载拖拽仍挂在根节点，用户从任意功能页拖入日志时都会切回日志分析页并复用原加载流程。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette();

        div()
            .relative()
            .flex()
            .size_full()
            .bg(rgb(palette.background))
            .track_focus(&self.root_focus_handle)
            .key_context("main-view-root")
            .on_key_down(context.listener(Self::handle_root_key_down))
            .on_action(
                context.listener(|view, _: &OpenSearchDialog, window, context| {
                    view.schedule_open_search_dialog(window, context);
                }),
            )
            .on_mouse_move(context.listener(Self::handle_root_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(Self::handle_root_mouse_up),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(Self::handle_root_mouse_up),
            )
            .can_drop(|dragged, _window, _app| dragged.is::<ExternalPaths>())
            .on_drop(
                context.listener(|view, external_paths: &ExternalPaths, _window, context| {
                    // GPUI 会把系统文件拖放转成 `ExternalPaths`；这里只取真实文件系统路径，
                    // 目录、普通文件和压缩包的具体解释仍交给加载模块统一处理。
                    view.navigation.active_main_feature = MainFeature::LogAnalysis;
                    view.start_log_source_load(
                        external_paths.paths().to_vec(),
                        "正在加载拖入的日志".to_string(),
                        context,
                    );
                }),
            )
            .child(self.render_main_navigation(context))
            .child(
                div()
                    .id("main-feature-page")
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .bg(rgb(palette.background))
                    .child(self.render_main_feature_page(context)),
            )
            .child(self.render_main_navigation_tooltip_overlay())
            .child(self.render_load_source_menu_dismiss_overlay(context))
            .child(self.render_load_source_menu(context))
            .child(self.render_save_overwrite_confirm_dialog(context))
    }
}
