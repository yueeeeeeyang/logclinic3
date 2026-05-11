// HPROF 线程详情独立窗口视图。
//
// 业务意图：
// - 线程详情窗口是 HPROF 分析页的从属 UI，只负责展示线程属性和堆栈，不参与 dump 解析或 dominator 计算。
// - 从主分析视图拆出后，HPROF 页文件只保留分析流程和 dominator 表格，降低单文件维护成本。

use super::*;
use crate::hprof::HprofThreadDetails;

/// HPROF 线程详情属性名列宽。
const HPROF_THREAD_PROPERTY_NAME_WIDTH: f32 = 190.0;
/// HPROF 线程详情属性行高度。
const HPROF_THREAD_PROPERTY_ROW_HEIGHT: f32 = 28.0;
/// HPROF 线程详情栈行高度。
const HPROF_THREAD_STACK_ROW_HEIGHT: f32 = 22.0;

/// HPROF 线程详情独立窗口。
///
/// 业务意图：
/// - 线程属性和堆栈信息通常比 dominator tree 行宽，独立窗口能让用户并排对照 MAT/LogClinic 结果，同时不压缩主列表。
pub(in crate::app) struct HprofThreadDetailsWindowView {
    /// 主窗口视图实体，用于同步主题。
    main_view: Entity<MainView>,
    /// 当前展示的线程详情。
    details: HprofThreadDetails,
    /// 线程堆栈虚拟列表滚动句柄。
    stack_scroll_handle: UniformListScrollHandle,
    /// 线程属性区是否展开。
    ///
    /// 业务意图：
    /// - 线程堆栈通常比属性更需要纵向空间，用户检查堆栈时可以收起属性区，把窗口主要空间留给栈帧列表。
    properties_expanded: bool,
    /// 主窗口状态变更订阅。
    _main_view_subscription: gpui::Subscription,
}

impl HprofThreadDetailsWindowView {
    /// 创建线程详情窗口根视图。
    pub(in crate::app) fn new(
        main_view: Entity<MainView>,
        details: HprofThreadDetails,
        context: &mut Context<Self>,
    ) -> Self {
        let observed_main_view = main_view.clone();
        let main_view_subscription = context.observe(&observed_main_view, |_, _, context| {
            context.notify();
        });
        Self {
            main_view,
            details,
            stack_scroll_handle: UniformListScrollHandle::new(),
            properties_expanded: true,
            _main_view_subscription: main_view_subscription,
        }
    }

    /// 替换当前展示线程。
    pub(in crate::app) fn update_details(
        &mut self,
        details: HprofThreadDetails,
        context: &mut Context<Self>,
    ) {
        self.details = details;
        self.stack_scroll_handle = UniformListScrollHandle::new();
        context.notify();
    }

    /// 切换线程属性区展开状态。
    fn toggle_properties(&mut self, context: &mut Context<Self>) {
        self.properties_expanded = !self.properties_expanded;
        context.notify();
    }

    /// 生成线程堆栈可滚动行。
    pub(in crate::app) fn thread_stack_lines(details: &HprofThreadDetails) -> Vec<String> {
        if details.stack_frames.is_empty() {
            return vec![
                details.thread_name.clone(),
                details
                    .stack_message
                    .clone()
                    .unwrap_or_else(|| "该 HPROF 未包含此线程的堆栈信息".to_string()),
            ];
        }
        let mut lines = Vec::with_capacity(details.stack_frames.len() + 1);
        lines.push(details.thread_name.clone());
        lines.extend(
            details
                .stack_frames
                .iter()
                .map(|frame| frame.display.clone()),
        );
        lines
    }

    /// 渲染线程详情标题区。
    fn render_header(&self, palette: AppThemePalette) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .truncate()
                            .child(format!("Thread {}", self.details.thread_name)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .truncate()
                            .child(format!(
                                "{} @ 0x{:x}",
                                self.details.class_name, self.details.object_id
                            )),
                    ),
            )
    }

    /// 渲染线程属性表。
    fn render_properties(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let expanded = self.properties_expanded;
        let icon = if expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        div()
            .flex()
            .flex_col()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .cursor_pointer()
                    .hover(move |header| header.bg(rgb(palette.hover)))
                    .child(MainView::render_lucide_icon(
                        Some(icon),
                        LOG_TREE_CHEVRON_WIDTH,
                        LOG_TREE_CHEVRON_SIZE,
                        palette.muted_text,
                    ))
                    .child("Thread Properties")
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseDownEvent, _window, context| {
                            view.toggle_properties(context);
                            context.stop_propagation();
                        }),
                    ),
            )
            .when(expanded, |container| {
                container.child(
                    div()
                        .mx_4()
                        .mb_4()
                        .border_1()
                        .border_color(rgb(palette.border))
                        .children(self.details.properties.iter().enumerate().map(
                            |(index, property)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .h(px(HPROF_THREAD_PROPERTY_ROW_HEIGHT))
                                    .bg(rgb(if index % 2 == 0 {
                                        palette.surface
                                    } else {
                                        palette.background
                                    }))
                                    .child(
                                        div()
                                            .w(px(HPROF_THREAD_PROPERTY_NAME_WIDTH))
                                            .flex_none()
                                            .h_full()
                                            .flex()
                                            .items_center()
                                            .px_2()
                                            .border_r_1()
                                            .border_color(rgb(palette.border))
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(palette.text))
                                            .truncate()
                                            .child(property.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .px_2()
                                            .text_xs()
                                            .text_color(rgb(palette.text))
                                            .truncate()
                                            .child(property.value.clone()),
                                    )
                            },
                        )),
                )
            })
    }

    /// 渲染线程堆栈区域。
    fn render_stack(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let stack_lines = Self::thread_stack_lines(&self.details);
        let stack_count = stack_lines.len();
        let stack_handle = self.stack_scroll_handle.clone();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .flex_1()
            .min_h(px(0.0))
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child("Thread Stack"),
            )
            .child(
                div().relative().flex_1().overflow_hidden().child(
                    uniform_list(
                        "hprof-thread-details-stack-list",
                        stack_count,
                        context.processor(
                            move |_view, range: std::ops::Range<usize>, _window, _context| {
                                range
                                    .filter_map(|index| stack_lines.get(index).cloned())
                                    .map(|line| {
                                        div()
                                            .h(px(HPROF_THREAD_STACK_ROW_HEIGHT))
                                            .flex()
                                            .items_center()
                                            .text_xs()
                                            .font_family(LOG_VIEWER_FONT_FAMILY)
                                            .text_color(rgb(palette.text))
                                            .child(line)
                                    })
                                    .collect::<Vec<_>>()
                            },
                        ),
                    )
                    .size_full()
                    .track_scroll(stack_handle),
                ),
            )
    }
}

impl Render for HprofThreadDetailsWindowView {
    /// 渲染 HPROF 线程详情窗口。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.main_view.read(context).palette();
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(palette.background))
            .child(self.render_header(palette))
            .child(self.render_properties(palette, context))
            .child(self.render_stack(palette, context))
    }
}
