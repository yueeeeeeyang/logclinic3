// 设置窗口日志页签渲染。
//
// 业务意图：
// - 从设置窗口框架中拆出 快搜关键字和线程过滤设置渲染，避免页签内容继续堆在独立窗口入口文件中。
// - 本轮只移动渲染方法，保持按钮、输入元素、配置读写和焦点流转行为不变。

use super::quick_search_input::QuickSearchKeywordsInputElement;
use super::thread_filter_input::ThreadAnalysisFilterTextAreaElement;
use super::*;

impl SettingsWindowView {
    /// 渲染日志页签。
    ///
    /// 业务意图：
    /// - 日志页集中放置影响日志解析、分析和展示的偏好；当前先承载线程日志分析过滤配置。
    pub(in crate::app) fn render_log_tab(
        &self,
        quick_search_keywords_is_editing: bool,
        quick_search_keywords_focus: gpui::FocusHandle,
        thread_analysis_filter_is_editing: bool,
        focus_handle: gpui::FocusHandle,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("settings-log-tab")
            .flex()
            .flex_col()
            .size_full()
            .p_4()
            // 日志页同时承载快搜配置和 300px 的线程过滤输入区，固定窗口高度下必须允许纵向滚动，
            // 否则底部输入框和保存按钮在 macOS/Windows 的标题栏高度差异下都可能被裁剪。
            .overflow_y_scroll()
            .scrollbar_width(px(6.0))
            .bg(rgb(palette.background))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .mb_3()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(MainView::render_lucide_icon(
                        Some(Icon::FileText),
                        16.0,
                        16.0,
                        palette.muted_text,
                    ))
                    .child("日志设置"),
            )
            .child(self.render_quick_search_keywords_setting(
                quick_search_keywords_is_editing,
                quick_search_keywords_focus,
                palette,
                context,
            ))
            .child(self.render_thread_analysis_filter_setting(
                thread_analysis_filter_is_editing,
                focus_handle,
                palette,
                context,
            ))
    }

    /// 渲染快搜关键字配置项。
    ///
    /// 业务意图：
    /// - 用户维护一组英文逗号分隔的排障关键字，搜索窗口“快搜”按钮会按任一关键字命中生成结果。
    /// - 配置项默认只读，和线程过滤配置保持一致，降低误修改跨会话搜索偏好的风险。
    pub(in crate::app) fn render_quick_search_keywords_setting(
        &self,
        quick_search_keywords_is_editing: bool,
        focus_handle: gpui::FocusHandle,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let input_background = if quick_search_keywords_is_editing {
            palette.input
        } else {
            palette.panel
        };
        div()
            .id("settings-quick-search-keywords")
            .flex()
            .flex_col()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .p_4()
            .mb_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mb_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(MainView::render_lucide_icon(
                                Some(Icon::Zap),
                                18.0,
                                18.0,
                                palette.muted_text,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(palette.text))
                                            .child("快搜关键字配置"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(palette.muted_text))
                                            .child("英文逗号分隔多个关键字，快搜命中任意关键字"),
                                    ),
                            ),
                    )
                    .child(self.render_quick_search_keywords_edit_button(
                        quick_search_keywords_is_editing,
                        palette,
                        context,
                    )),
            )
            .child(
                div()
                    .id("settings-quick-search-keywords-input")
                    .relative()
                    .h(px(SEARCH_INPUT_HEIGHT))
                    .w_full()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(if quick_search_keywords_is_editing {
                        palette.accent
                    } else {
                        palette.border
                    }))
                    .bg(rgb(input_background))
                    .track_focus(&focus_handle)
                    .key_context("quick-search-keywords-input")
                    .on_key_down(context.listener(Self::handle_quick_search_keywords_key_down))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, event: &MouseDownEvent, window, context| {
                            view.handle_quick_search_keywords_mouse_down(event, window, context);
                        }),
                    )
                    .on_mouse_move(context.listener(
                        |view, event: &MouseMoveEvent, _window, context| {
                            view.handle_quick_search_keywords_mouse_move(event, context);
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.handle_quick_search_keywords_mouse_up(context);
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.handle_quick_search_keywords_mouse_up(context);
                        }),
                    )
                    .child(
                        div()
                            .id("settings-quick-search-keywords-text")
                            .absolute()
                            .left(px(10.0))
                            .right(px(10.0))
                            .top(px(5.0))
                            .bottom(px(5.0))
                            .overflow_hidden()
                            .text_sm()
                            .text_color(rgb(palette.text))
                            .child(QuickSearchKeywordsInputElement {
                                view: self.main_view.clone(),
                                focus_handle,
                                editable: quick_search_keywords_is_editing,
                                placeholder: "ERROR,Exception,Timeout",
                                palette,
                            }),
                    ),
            )
    }

    /// 渲染快搜关键字编辑/保存按钮。
    pub(in crate::app) fn render_quick_search_keywords_edit_button(
        &self,
        is_editing: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (label, icon, text_color, background) = if is_editing {
            ("保存", Icon::Save, palette.on_accent, palette.accent)
        } else {
            ("编辑", Icon::Pencil, palette.text, palette.panel)
        };
        div()
            .id("settings-quick-search-keywords-edit")
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .h(px(28.0))
            .px_3()
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(background))
            .text_xs()
            .text_color(rgb(text_color))
            .cursor_pointer()
            .hover(move |button| {
                button.bg(rgb(if is_editing {
                    palette.accent_hover
                } else {
                    palette.hover
                }))
            })
            .child(MainView::render_lucide_icon(
                Some(icon),
                13.0,
                13.0,
                text_color,
            ))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, window, context| {
                    if let Some(focus_handle) = view.toggle_quick_search_keywords_editing(context) {
                        window.focus(&focus_handle);
                    }
                }),
            )
    }

    /// 渲染线程日志分析过滤设置项。
    ///
    /// 业务意图：
    /// - 用户点击编辑后可以粘贴一个或多个完整线程堆栈，保存后的下一次线程日志分析会按这些片段过滤无效线程。
    /// - 输入区使用等宽字体和滚动容器，便于核对 Java 堆栈中的类名、方法名和锁信息。
    pub(in crate::app) fn render_thread_analysis_filter_setting(
        &self,
        thread_analysis_filter_is_editing: bool,
        focus_handle: gpui::FocusHandle,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let input_background = if thread_analysis_filter_is_editing {
            palette.input
        } else {
            palette.panel
        };
        div()
            .id("settings-thread-analysis-filter")
            .flex()
            .flex_col()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .p_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mb_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(MainView::render_lucide_icon(
                                Some(Icon::ListFilter),
                                18.0,
                                18.0,
                                palette.muted_text,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(palette.text))
                                            .child("线程日志分析过滤"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(palette.muted_text))
                                            .child("空行分隔多段堆栈，命中连续片段的线程不会显示"),
                                    ),
                            ),
                    )
                    .child(self.render_thread_analysis_filter_edit_button(
                        thread_analysis_filter_is_editing,
                        palette,
                        context,
                    )),
            )
            .child(
                div()
                    .id("settings-thread-analysis-filter-input")
                    .relative()
                    .h(px(THREAD_ANALYSIS_FILTER_TEXTAREA_HEIGHT))
                    .w_full()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(if thread_analysis_filter_is_editing {
                        palette.accent
                    } else {
                        palette.border
                    }))
                    .bg(rgb(input_background))
                    .track_focus(&focus_handle)
                    .key_context("thread-analysis-filter-input")
                    .on_key_down(context.listener(Self::handle_thread_analysis_filter_key_down))
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(|view, event: &MouseDownEvent, window, context| {
                            view.handle_thread_analysis_filter_mouse_down(event, window, context);
                        }),
                    )
                    .on_mouse_move(context.listener(
                        |view, event: &MouseMoveEvent, _window, context| {
                            view.handle_thread_analysis_filter_mouse_move(event, context);
                        },
                    ))
                    .on_mouse_up(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.handle_thread_analysis_filter_mouse_up(context);
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        context.listener(|view, _event: &MouseUpEvent, _window, context| {
                            view.handle_thread_analysis_filter_mouse_up(context);
                        }),
                    )
                    .child(
                        div()
                            .id("settings-thread-analysis-filter-scroll")
                            .size_full()
                            .px_2()
                            .py_2()
                            .overflow_y_scroll()
                            .scrollbar_width(px(6.0))
                            .text_size(px(12.0))
                            .line_height(px(THREAD_ANALYSIS_FILTER_TEXT_LINE_HEIGHT))
                            .text_color(rgb(palette.text))
                            .font_family(LOG_VIEWER_FONT_FAMILY)
                            .child(ThreadAnalysisFilterTextAreaElement {
                                view: self.main_view.clone(),
                                focus_handle,
                                editable: thread_analysis_filter_is_editing,
                                placeholder: "粘贴需要过滤的线程堆栈；多段堆栈之间用空行分隔",
                                palette,
                            }),
                    ),
            )
    }

    /// 渲染线程分析过滤编辑/保存按钮。
    pub(in crate::app) fn render_thread_analysis_filter_edit_button(
        &self,
        is_editing: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let (label, icon, text_color, background) = if is_editing {
            ("保存", Icon::Save, palette.on_accent, palette.accent)
        } else {
            ("编辑", Icon::Pencil, palette.text, palette.panel)
        };
        div()
            .id("settings-thread-analysis-filter-edit")
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .h(px(28.0))
            .px_3()
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(background))
            .text_xs()
            .text_color(rgb(text_color))
            .cursor_pointer()
            .hover(move |button| {
                button.bg(rgb(if is_editing {
                    palette.accent_hover
                } else {
                    palette.hover
                }))
            })
            .child(MainView::render_lucide_icon(
                Some(icon),
                13.0,
                13.0,
                text_color,
            ))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, window, context| {
                    if let Some(focus_handle) = view.toggle_thread_analysis_filter_editing(context)
                    {
                        window.focus(&focus_handle);
                    }
                }),
            )
    }
}
