//! LogClinic 桌面客户端的应用装配与主视图协调模块。
//!
//! 业务意图：
//! - 该模块承接从历史 `main.rs` 中迁出的主窗口装配、根状态协调和全局事件分发。
//! - 体积较大的 GPUI 壳层代码已经拆到 `app/ui` 目录，由正常 `mod` 边界约束跨域访问。
//! - 这里仍保留跨域协调逻辑，例如窗口句柄、后台任务回调、主视图状态汇总和全局快捷键。
//!
//! 跨平台约束：
//! - macOS 和 Windows 都需要从同一个应用装配入口启动主窗口，平台打开文件和窗口尺寸差异由专门函数处理。
//! - 窗口尺寸使用 GPUI 的逻辑像素表达，由 GPUI 负责映射到具体平台窗口系统。
//! - 当前只持久化主窗口宽高、主题偏好和日志字号，不持久化窗口位置，避免跨显示器恢复造成窗口不可见。

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    env, fs,
    ops::{Deref, DerefMut, Range},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::archive::{
    ArchiveFormat, ArchiveMemberSource, ArchiveScanEntryKind, ArchiveScanNode,
    cleanup_materialized_file, cleanup_stale_large_log_cache, materialize_source_for_paging,
    read_archive_member, read_archive_member_from_bytes, scan_archive_with_progress,
    single_gzip_member_display_name, single_gzip_member_path_for_archive,
    write_temporary_nested_archive_bytes,
};
use crate::config::*;
use crate::connections::*;
use crate::highlighting::{SyntaxTheme, highlight_line};
use crate::launch::{
    classify_launch_paths, log_source_paths_from_launch_arguments, log_source_paths_from_open_urls,
};
use crate::log_ai_analysis::*;
use crate::log_document;
use crate::log_document::{
    EncodingChoice, LogContentError, LogTextEncoding, decode_log_bytes, longest_log_line_index,
};
use crate::log_document::{LargeLogOpenResult, LogTabDocument, open_log_source_for_tab};
use crate::plugin::*;
use crate::search::{
    SearchFileError, SearchMatchMode, SearchMatchPosition, SearchOptions, SearchProgress,
    SearchResultItem, SearchScope, collect_current_directory_sources, count_query_occurrences,
    find_search_result_after_position, find_search_result_before_position, search_lines,
    source_location_label,
};
use crate::search::{
    count_query_occurrences_paged, find_paged_search_result_after_position,
    find_paged_search_result_before_position, search_paged_document,
};
use crate::shell_integration::{
    ShellIntegrationStatus, query_shell_integration_status, register_shell_integration,
    unregister_shell_integration,
};
use crate::theme::{AppThemePalette, EffectiveTheme, ThemePreference};
use crate::thread_analysis::{
    ThreadAnalysisData, ThreadAnalysisFilterRule, ThreadAnalysisProgress, ThreadConcurrencyRow,
    ThreadDumpLineFilterSummary, ThreadStateKind, ThreadTimelineCell,
    analyze_thread_dump_sources_with_progress, filter_thread_dump_lines_with_analysis_rules,
    has_java_thread_dump_snapshots, parse_thread_analysis_filter_rules,
    parse_thread_analysis_name_filter_rules, visible_thread_indexes_for_state_kinds,
};
#[cfg(test)]
use crate::thread_analysis::{
    ThreadAnalysisFilterRuleKind, ThreadSnapshot, ThreadStateSample, build_thread_analysis_data,
    parse_thread_dump_snapshots, thread_name_matches_filter_rule, thread_stack_matches_filter_rule,
    wildcard_pattern_matches_text,
};
use crate::{
    log_loader::{
        LoadedLogTree, LogLoadProgress, LogTreeEntryKind, LogTreeRow as LoadedLogTreeRow,
        load_log_sources_with_progress,
    },
    log_source::LogFileSource,
};
use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt as _, AnyWindowHandle, App, AppContext, Application, AsyncApp, Bounds,
    ClickEvent, ClipboardItem, Context, DisplayId, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, ExternalPaths, FocusHandle, FontStyle, FontWeight, GlobalElementId,
    HighlightStyle, InteractiveElement, IntoElement, KeyBinding, KeyDownEvent, Keystroke, LayoutId,
    ListAlignment, ListHorizontalSizingBehavior, ListState, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, ParentElement, PathPromptOptions, Pixels, Point,
    Render, ScrollHandle, ScrollStrategy, ScrollWheelEvent, ShapedLine, SharedString,
    StatefulInteractiveElement, StrikethroughStyle, Style, Styled as _, StyledText, TextRun,
    TitlebarOptions, UTF16Selection, UnderlineStyle, UniformListScrollHandle, Window,
    WindowAppearance, WindowBounds, WindowHandle, WindowKind, WindowOptions, actions, div, fill,
    list, point, px, relative, rgb, rgba, size, uniform_list,
};
use lucide_icons::{Icon, LUCIDE_FONT_BYTES};
use main_view::MainView;
use native_appearance::apply_native_theme_preference;

actions!(logclinic, [OpenSearchDialog]);

/// 主窗口根实体和构造功能域。
mod main_view;
/// 原生窗口标题栏外观适配。
mod native_appearance;
/// 应用层纯状态测试。
#[cfg(test)]
mod tests;
/// GPUI 壳层视图、输入和窗口适配模块。
mod ui;
/// 主窗口运行时和启动装配功能域。
mod window_runtime;
pub(crate) use window_runtime::run;
#[cfg(test)]
pub(crate) use window_runtime::{
    MainWindowStartupDecision, decide_main_window_startup, fit_main_window_size_to_display,
};

use ui::*;

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
