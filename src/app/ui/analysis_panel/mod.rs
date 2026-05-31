// 分析类窗口 GPUI 聚合模块。
//
// 业务意图：
// - 将 HPROF 分析页和线程分析窗口放在同一 UI 子域，统一表达“分析结果展示与跳转”的壳层职责。
// - HPROF dominator 计算和线程日志解析继续留在顶层业务域，避免 UI 反向拥有解析规则。

use super::*;

/// HPROF 线程详情独立窗口视图。
mod hprof_thread_details_view;
/// HPROF dump 分析 GPUI 视图。
mod hprof_view;
/// 日志智能分析独立窗口视图。
mod log_ai_analysis_window;
/// 线程分析堆栈详情独立窗口视图。
mod thread_stack_window_view;
/// 线程分析独立窗口视图。
mod thread_window_view;

pub(in crate::app) use hprof_thread_details_view::HprofThreadDetailsWindowView;
pub(in crate::app) use hprof_view::HprofAnalysisView;
pub(in crate::app) use log_ai_analysis_window::{
    LOG_AI_ANALYSIS_WINDOW_HEIGHT, LOG_AI_ANALYSIS_WINDOW_WIDTH, LogAiAnalysisWindowView,
};
pub(in crate::app) use thread_stack_window_view::{
    THREAD_STACK_WINDOW_HEIGHT, THREAD_STACK_WINDOW_MIN_HEIGHT, THREAD_STACK_WINDOW_MIN_WIDTH,
    THREAD_STACK_WINDOW_WIDTH, ThreadStackWindowView,
};
#[cfg(test)]
pub(in crate::app) use thread_window_view::ThreadAnalysisResultTab;
pub(in crate::app) use thread_window_view::{
    SearchResultsResizeDrag, SearchTarget, ThreadAnalysisWindowView,
};
