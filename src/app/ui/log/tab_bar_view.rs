// 日志 tab 栏与编码菜单渲染。
//
// 业务意图：
// - 本文件由原主窗口工作区实现机械拆分而来，只承载 GPUI 壳层内的 tab 栏、编码选择器和编码下拉菜单渲染。
// - 拆分过程保持所有状态字段、事件时序和用户可见行为不变，便于后续按视图职责继续收束。

use super::*;

impl MainView {
    pub(in crate::app) fn render_splitter_hit_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id("content-splitter-hit-overlay")
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .h_full()
            .w(px(SPLITTER_OVERLAY_HIT_WIDTH))
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(Self::start_resizing_splitter),
            )
    }

    /// 渲染已加载目录树但尚未打开文件时的右侧提示。
    ///
    /// 业务意图：
    /// - 用户完成“加载日志”后，下一步是从左侧树选择具体日志文件，右侧需要明确指引当前空态。
    pub(in crate::app) fn render_loaded_right_empty_message(&self) -> impl IntoElement {
        let palette = self.palette();
        div()
            .id("loaded-right-empty-message")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .size_full()
            .px_4()
            .bg(rgb(palette.background))
            .child(Self::render_lucide_icon(
                Some(Icon::FileSearch),
                34.0,
                30.0,
                palette.muted_text,
            ))
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child("点击左侧日志文件查看内容"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_text))
                    .child("支持普通日志文件和压缩包内日志文件。"),
            )
    }

    /// 渲染右侧 tab 栏。
    ///
    /// 业务意图：
    /// - 每个打开的日志文件对应一个 tab，用户可以在多个日志之间快速切换。
    /// - tab 上的右键菜单提供关闭当前、关闭其他和关闭所有三个命令。
    /// - tab 标题完整展示，超出可视宽度时通过横向滚动和两侧箭头访问。
    pub(in crate::app) fn render_log_tab_bar(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let tabs = self
            .log
            .open_tabs
            .iter()
            .map(|tab| {
                (
                    tab.id,
                    tab.title.clone(),
                    self.log.active_tab_id == Some(tab.id),
                )
            })
            .collect::<Vec<_>>();
        let tab_elements = tabs
            .into_iter()
            .map(|(tab_id, title, active)| self.render_log_tab(tab_id, title, active, context))
            .collect::<Vec<_>>();

        div()
            .id("log-tab-bar")
            .relative()
            .flex()
            .items_center()
            .h(px(LOG_TAB_BAR_HEIGHT))
            .flex_none()
            .overflow_hidden()
            .bg(rgb(self.palette().panel))
            .border_b_1()
            .border_color(rgb(self.palette().border))
            .child(self.render_tab_scroll_button(Icon::ChevronLeft, -LOG_TAB_SCROLL_STEP, context))
            .child(
                div()
                    .id("log-tab-scroll-viewport")
                    .flex()
                    .items_end()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_scroll()
                    .scrollbar_width(px(0.0))
                    .track_scroll(&self.log.tab_bar_scroll_handle)
                    .children(tab_elements),
            )
            .child(self.render_tab_scroll_button(Icon::ChevronRight, LOG_TAB_SCROLL_STEP, context))
    }

    /// 渲染 tab 栏横向滚动箭头。
    ///
    /// 业务意图：
    /// - 打开的日志较多时，用户可以通过左右箭头移动 tab 栏，而不需要依赖触控板横向滚动。
    /// - 按钮始终显示，符合用户“tab页签两边显示滚动箭头”的要求。
    pub(in crate::app) fn render_tab_scroll_button(
        &self,
        icon: Icon,
        delta: f32,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        div()
            .id(SharedString::from(format!("tab-scroll-{}", delta)))
            .flex()
            .items_center()
            .justify_center()
            .h_full()
            .w(px(LOG_TAB_SCROLL_BUTTON_WIDTH))
            .flex_none()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .text_color(rgb(palette.muted_text))
            .cursor_pointer()
            .hover(move |button| {
                button
                    .bg(rgb(palette.surface))
                    .text_color(rgb(palette.accent))
            })
            .when(delta < 0.0, |button| button.border_r_1())
            .when(delta > 0.0, |button| button.border_l_1())
            .child(Self::render_lucide_icon(
                Some(icon),
                14.0,
                14.0,
                palette.muted_text,
            ))
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.scroll_tab_bar(delta, context);
                    // 滚动箭头是 tab 栏内的独立按钮，不应把 click 继续传递给 tab 容器。
                    context.stop_propagation();
                }),
            )
    }

    /// 按给定像素距离横向滚动 tab 栏。
    ///
    /// 业务意图：
    /// - `ScrollHandle` 的偏移量向左滚动时为负值，因此向右箭头需要减少 x 偏移。
    /// - 滚动结果限制在 `[最大负偏移, 0]`，避免箭头点击后出现空白区域。
    pub(in crate::app) fn scroll_tab_bar(&mut self, delta: f32, context: &mut Context<Self>) {
        let current_offset = self.log.tab_bar_scroll_handle.offset();
        let max_scroll = self.log.tab_bar_scroll_handle.max_offset().width;
        let next_x = (current_offset.x - px(delta)).clamp(-max_scroll, px(0.0));
        self.log
            .tab_bar_scroll_handle
            .set_offset(point(next_x, current_offset.y));
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        context.notify();
    }

    /// 渲染单个日志 tab。
    ///
    /// 业务意图：
    /// - 左键激活 tab，右键打开 tab 操作菜单。
    /// - tab 标题完整展示，不做省略；关闭按钮放在右侧，便于快速收起单个日志。
    pub(in crate::app) fn render_log_tab(
        &self,
        tab_id: usize,
        title: String,
        active: bool,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        let background = if active {
            palette.background
        } else {
            palette.panel
        };
        div()
            .id(SharedString::from(format!("log-tab-{}", tab_id)))
            .flex()
            .items_center()
            .h(px(LOG_TAB_BAR_HEIGHT - 1.0))
            .min_w(px(96.0))
            .flex_none()
            .px_3()
            .gap_1()
            .border_r_1()
            .border_color(rgb(palette.border))
            .bg(rgb(background))
            .text_size(px(LOG_TAB_FONT_SIZE))
            .text_color(rgb(if active {
                palette.text
            } else {
                palette.muted_text
            }))
            .cursor_pointer()
            .hover(move |tab| tab.bg(rgb(palette.surface)))
            .child(Self::render_lucide_icon(
                Some(Icon::FileText),
                14.0,
                13.0,
                palette.muted_text,
            ))
            .child(div().flex_none().whitespace_nowrap().child(title))
            .child(self.render_tab_close_button(tab_id, context))
            .on_click(
                context.listener(move |view, event: &ClickEvent, _window, context| {
                    if event.standard_click() {
                        view.activate_tab(tab_id);
                        context.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.open_tab_context_menu(
                        tab_id,
                        f32::from(event.position.x),
                        f32::from(event.position.y),
                        context,
                    );
                }),
            )
    }

    /// 渲染 tab 右侧关闭按钮。
    ///
    /// 业务意图：
    /// - 每个 tab 都提供明确的关闭入口，避免用户只能通过右键菜单关闭。
    /// - 关闭后按 `close_tab` 的统一规则切换激活 tab，并清理相关弹层状态。
    pub(in crate::app) fn render_tab_close_button(
        &self,
        tab_id: usize,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        div()
            .id(SharedString::from(format!("log-tab-close-{}", tab_id)))
            .flex()
            .items_center()
            .justify_center()
            .w(px(LOG_TAB_CLOSE_BUTTON_WIDTH))
            .h(px(LOG_TAB_CLOSE_BUTTON_WIDTH))
            .flex_none()
            .rounded(px(3.0))
            .text_color(rgb(palette.muted_text))
            .hover(move |button| button.bg(rgb(palette.hover)).text_color(rgb(palette.text)))
            .child(Self::render_lucide_icon(
                Some(Icon::X),
                12.0,
                12.0,
                palette.muted_text,
            ))
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.close_tab(tab_id, context);
                    view.log.tab_context_menu = None;
                    view.log.encoding_dropdown_menu = None;
                    context.notify();
                    // 关闭按钮嵌套在 tab 元素中，关闭后不能再让父 tab 收到同一次点击。
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染当前激活 tab 的内容。
    ///
    /// 业务意图：
    /// - 激活 tab 统一显示编码工具条；正文根据读取状态显示加载、错误或日志虚拟列表。
    pub(in crate::app) fn render_active_log_tab(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(active_tab_id) = self.log.active_tab_id else {
            return div()
                .id("active-log-tab-empty")
                .flex()
                .flex_1()
                .child(self.render_loaded_right_empty_message());
        };
        let Some(tab) = self
            .log
            .open_tabs
            .iter()
            .find(|tab| tab.id == active_tab_id)
        else {
            return div()
                .id("active-log-tab-missing")
                .flex()
                .flex_1()
                .child(self.render_loaded_right_empty_message());
        };

        div()
            .id("active-log-tab")
            .flex()
            .flex_col()
            .flex_1()
            .overflow_hidden()
            .child(self.render_log_document_toolbar(tab, context))
            .child(self.render_log_tab_body(tab, context))
    }

    /// 渲染当前日志文档工具条。
    ///
    /// 业务意图：
    /// - 左侧不再放置独立编码控件，避免挤占日志正文起点上方空间。
    /// - 工具条右侧把实际编码直接渲染成编码选择器，同时展示来源、识别方式和行数。
    pub(in crate::app) fn render_log_document_toolbar(
        &self,
        tab: &OpenLogTab,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let source_kind = match &tab.source {
            LogFileSource::LocalFile { .. } => "本地文件",
            LogFileSource::ArchiveMember { .. } => "压缩包内文件",
            LogFileSource::MaterializedArchiveMember { .. } => "压缩包内文件",
            LogFileSource::NestedArchiveMember { .. } => "嵌套压缩包内文件",
        };
        let encoding_button_label = Self::log_tab_encoding_selector_label(tab);
        let mut status = match &tab.state {
            LogTabState::Ready { document } => {
                // 状态栏展示编码来源和行数；实际编码本身由右侧内联编码选择器负责显示和切换。
                let encoding_source = if document.detected_automatically() {
                    "自动识别"
                } else {
                    "手动选择"
                };
                let replacement_warning = if document.had_replacements() {
                    " · 含替换字符"
                } else {
                    ""
                };
                format!(
                    "{} · {}{}",
                    encoding_source,
                    document.status_line_label(),
                    replacement_warning
                )
            }
            LogTabState::Loading { .. } => "正在处理".to_string(),
            LogTabState::Failed { .. } => "需要选择正确编码或重新加载".to_string(),
        };
        if let Some(summary) = tab.thread_filter_summary {
            status = format!(
                "{} · 已过滤 {} 个线程片段",
                status, summary.hidden_thread_count
            );
        }
        let palette = self.palette();

        div()
            .id("log-document-toolbar")
            .flex()
            .items_center()
            .justify_between()
            .h(px(LOG_DOCUMENT_TOOLBAR_HEIGHT))
            .flex_none()
            .px_3()
            .gap_2()
            .bg(rgb(palette.panel))
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(self.render_thread_filter_toolbar_button(tab, palette, context))
            .child(
                div()
                    .id("log-document-status-group")
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(rgb(palette.muted_text))
                    .child(div().flex_none().child(source_kind))
                    .child(Self::render_status_separator(palette))
                    .child(self.render_encoding_selector(
                        tab.id,
                        encoding_button_label,
                        tab.encoding_choice,
                        matches!(tab.state, LogTabState::Ready { .. }) || tab.raw_bytes.is_some(),
                        palette,
                        context,
                    ))
                    .child(Self::render_status_separator(palette))
                    .child(div().min_w_0().truncate().child(status)),
            )
    }

    /// 渲染日志工具条左侧的线程过滤按钮。
    ///
    /// 业务意图：
    /// - Java thread dump 打开后，用户经常需要先按线程分析配置隐藏 JVM 常驻线程和单次噪声线程，再阅读剩余业务线程。
    /// - 按钮放在工具条左侧空位，和右侧编码/行数状态分离，避免改变日志正文起点或挤压状态信息。
    ///
    /// 边界条件：
    /// - 普通日志、读取失败、加载中和分页大日志都返回占位容器，不显示按钮。
    /// - 点击会消费事件，避免工具条未来叠加弹层时误传到底层日志正文。
    pub(in crate::app) fn render_thread_filter_toolbar_button(
        &self,
        tab: &OpenLogTab,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if !tab.thread_filter_available {
            return div().id("thread-filter-toolbar-placeholder").flex_none();
        }

        let tab_id = tab.id;
        let active = tab.thread_filter_original_document.is_some();
        let label = if active {
            "取消过滤"
        } else {
            "过滤线程"
        };
        let foreground = if active { palette.accent } else { palette.text };
        div()
            .id(SharedString::from(format!("thread-filter-button-{tab_id}")))
            .flex()
            .items_center()
            .gap_1()
            .h(px(ENCODING_DROPDOWN_BUTTON_HEIGHT))
            .px_2()
            .flex_none()
            .rounded(px(4.0))
            .border_1()
            .border_color(rgb(if active {
                palette.accent
            } else {
                palette.border
            }))
            .bg(rgb(if active {
                palette.selected
            } else {
                palette.input
            }))
            .text_xs()
            .text_color(rgb(foreground))
            .cursor_pointer()
            .hover(move |button| {
                button
                    .border_color(rgb(palette.accent))
                    .bg(rgb(palette.hover))
            })
            .child(Self::render_lucide_icon(
                Some(Icon::ListFilter),
                13.0,
                13.0,
                foreground,
            ))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.toggle_thread_filter_for_tab(tab_id, context);
                    context.stop_propagation();
                }),
            )
    }

    /// 返回日志工具条编码选择器应展示的文案。
    ///
    /// 业务意图：
    /// - 自动模式下按钮展示实际检测出的编码，方便用户理解当前文件最终按什么编码打开。
    /// - 手动模式下按钮必须优先展示用户选择的编码，即使底层解码出的文本编码枚举与自动检测结果相同，
    ///   也不能回退成检测结果，否则用户会误以为“编码切换选择无效”。
    ///
    /// 边界条件：
    /// - 加载中或解码失败时没有稳定的文档编码，此时直接展示当前选择，保证失败后仍能看到正在尝试的编码。
    pub(in crate::app) fn log_tab_encoding_selector_label(tab: &OpenLogTab) -> &'static str {
        match tab.encoding_choice {
            EncodingChoice::Auto => match &tab.state {
                LogTabState::Ready { document } => document.encoding_label(),
                LogTabState::Loading { .. } | LogTabState::Failed { .. } => {
                    EncodingChoice::Auto.label()
                }
            },
            EncodingChoice::Manual(encoding) => encoding.label(),
        }
    }

    /// 渲染编码切换控件。
    ///
    /// 业务意图：
    /// - 编码选项改为紧凑下拉框，避免多个编码按钮平铺占用顶部工具条。
    /// - “自动”重新执行检测流程，手动选项按指定编码重新解码原始字节。
    /// - 原始字节未读取完成前保持禁用，避免用户提前选择编码导致 UI 状态和实际解码结果不一致。
    pub(in crate::app) fn render_encoding_selector(
        &self,
        tab_id: usize,
        display_label: &'static str,
        selected_choice: EncodingChoice,
        enabled: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let select = Select::new(
            format!("encoding-select-{}", tab_id),
            display_label,
            selected_choice,
            Self::encoding_select_options(),
            Self::encoding_select_metrics(),
            palette,
        )
        .enabled(enabled);

        div()
            .id("encoding-selector")
            .flex()
            .items_center()
            .flex_none()
            .child(select.render_trigger(context.listener(
                move |view, event: &ClickEvent, _window, context| {
                    if enabled {
                        let position = event.position();
                        view.toggle_encoding_dropdown(
                            tab_id,
                            f32::from(position.x),
                            f32::from(position.y),
                            context,
                        );
                    }
                    // 编码选择按钮属于状态栏内的独立控件，点击不应继续传给日志正文或外层浮层。
                    context.stop_propagation();
                },
            )))
    }

    /// 渲染右侧状态栏中的点状分隔符。
    ///
    /// 业务意图：
    /// - 来源、编码选择器、识别方式和行数属于同一组状态信息，用轻量分隔符维持可读性。
    /// - 单独函数可以避免多个位置重复硬编码颜色和文本。
    pub(in crate::app) fn render_status_separator(palette: AppThemePalette) -> gpui::Div {
        div()
            .flex_none()
            .text_color(rgb(palette.muted_text))
            .child("·")
    }

    /// 切换编码 Select 菜单展开状态。
    ///
    /// 业务意图：
    /// - 同一个 tab 再次点击编码框会收起菜单；点击其它 tab 的编码框会切换到新的菜单。
    /// - 打开编码菜单时收起 tab 右键菜单，避免两个弹层同时争抢点击区域。
    pub(in crate::app) fn toggle_encoding_dropdown(
        &mut self,
        tab_id: usize,
        window_x: f32,
        window_y: f32,
        context: &mut Context<Self>,
    ) {
        if !self
            .log
            .open_tabs
            .iter()
            .any(|tab| tab.id == tab_id && matches!(tab.state, LogTabState::Ready { .. }))
        {
            // 编码菜单必须等内存原始字节或分页文档可用后才能打开，避免用户在加载过程中选择编码但无法立即解析。
            self.log.encoding_dropdown_menu = None;
            self.log.tab_context_menu = None;
            self.search.search_results_context_menu = None;
            self.log.log_viewer_context_menu = None;
            context.notify();
            return;
        }

        let is_same_menu_open = self
            .log
            .encoding_dropdown_menu
            .as_ref()
            .is_some_and(|menu| menu.tab_id == tab_id);
        self.log.encoding_dropdown_menu = if is_same_menu_open {
            None
        } else {
            Some(EncodingDropdownMenu {
                tab_id,
                x: self.encoding_dropdown_menu_x(window_x),
                y: Self::encoding_dropdown_menu_y(window_y),
            })
        };
        self.log.tab_context_menu = None;
        self.search.search_results_context_menu = None;
        self.log.log_viewer_context_menu = None;
        context.notify();
    }

    /// 根据点击位置计算编码菜单在右侧工作区内的横坐标。
    ///
    /// 业务意图：
    /// - 编码选择器位于右侧状态栏，状态栏内容会随来源类型、识别状态和行数变化。
    /// - GPUI 当前没有直接暴露按钮布局矩形给点击监听，因此用点击点近似还原按钮左缘，让菜单跟随按钮打开。
    ///
    /// 边界条件：
    /// - 点击按钮文字或箭头会带来几个像素的偏差，但菜单仍紧邻编码按钮，不会回到旧版左侧固定位置。
    pub(in crate::app) fn encoding_dropdown_menu_x(&self, window_x: f32) -> f32 {
        let metrics = Self::encoding_select_metrics();
        let panel_x = (window_x - self.right_panel_left_offset()).max(0.0);
        (panel_x - metrics.button_width / 2.0).max(0.0)
    }

    /// 根据点击位置计算编码菜单在右侧工作区内的纵坐标。
    ///
    /// 业务意图：
    /// - 菜单应出现在编码按钮下方；点击位置通常位于按钮中部，因此加上半个按钮高度和固定间隔。
    pub(in crate::app) fn encoding_dropdown_menu_y(window_y: f32) -> f32 {
        let metrics = Self::encoding_select_metrics();
        (window_y - TOOLBAR_HEIGHT + metrics.button_height / 2.0 + metrics.menu_gap).max(0.0)
    }

    /// 返回编码 Select 的尺寸配置。
    ///
    /// 业务意图：
    /// - 日志编码选择器是第一个迁移到通用 Select 的控件，但用户可见尺寸必须沿用旧常量。
    /// - 最大菜单高度预留 8 个选项的空间；当前手动编码只有 5 项，因此不会出现滚动条，后续增加编码时仍有上限保护。
    pub(in crate::app) fn encoding_select_metrics() -> SelectMetrics {
        SelectMetrics::new(
            ENCODING_DROPDOWN_BUTTON_WIDTH,
            ENCODING_DROPDOWN_BUTTON_HEIGHT,
            ENCODING_DROPDOWN_WIDTH,
            ENCODING_DROPDOWN_ITEM_HEIGHT,
            ENCODING_DROPDOWN_ITEM_HEIGHT * 8.0 + 8.0,
            ENCODING_DROPDOWN_MENU_GAP,
        )
    }

    /// 返回编码 Select 菜单的手动编码选项。
    ///
    /// 业务意图：
    /// - 打开日志时仍默认自动识别；下拉菜单只承载用户主动纠正编码的手动选项。
    /// - 用户要求去除菜单中的“自动”，因此这里不再把 `EncodingChoice::Auto` 作为可点击项渲染。
    /// - UI 渲染和点击处理共享同一组选项，避免菜单项和编码解析能力不一致。
    pub(in crate::app) fn encoding_choices() -> Vec<EncodingChoice> {
        LogTextEncoding::manual_options()
            .into_iter()
            .map(EncodingChoice::Manual)
            .collect()
    }

    /// 返回编码 Select 使用的通用选项结构。
    ///
    /// 业务意图：
    /// - 通用 Select 通过 `SelectOption` 统一管理稳定 id、展示文案和业务值。
    /// - id 使用手动编码数组下标，避免把中文或带空格的 label 作为元素标识，同时保持选项顺序变化可被测试捕获。
    pub(in crate::app) fn encoding_select_options() -> Vec<SelectOption<EncodingChoice>> {
        Self::encoding_choices()
            .into_iter()
            .enumerate()
            .map(|(index, choice)| {
                SelectOption::new(format!("encoding-{}", index), choice.label(), choice)
            })
            .collect()
    }

    /// 渲染编码下拉菜单。
    ///
    /// 业务意图：
    /// - 菜单作为右侧工作区内部弹层绘制，视觉上贴在当前 tab 的编码选择框下方。
    /// - 选择任一编码后复用当前 tab 保存的原始字节重新解析，并立即收起菜单。
    pub(in crate::app) fn render_encoding_dropdown_menu(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = &self.log.encoding_dropdown_menu else {
            return div().id("encoding-dropdown-menu-empty").hidden();
        };
        let Some(tab) = self.log.open_tabs.iter().find(|tab| tab.id == menu.tab_id) else {
            return div().id("encoding-dropdown-menu-missing").hidden();
        };
        let tab_id = tab.id;
        let selected_choice = tab.encoding_choice;
        let palette = self.palette();
        let select = Select::new(
            "encoding-select",
            Self::log_tab_encoding_selector_label(tab),
            selected_choice,
            Self::encoding_select_options(),
            Self::encoding_select_metrics(),
            palette,
        )
        .open_anchor(Some(SelectAnchor::new(menu.x, menu.y)));

        select.render_menu(|choice| {
            Box::new(
                context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                    view.select_tab_encoding(tab_id, choice, context);
                    context.stop_propagation();
                }),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证编码 Select 仍只展示底层支持的手动编码选项，不把“自动”加入菜单。
    #[test]
    fn 编码_select_options_覆盖所有手动编码() {
        let actual_labels = MainView::encoding_select_options()
            .into_iter()
            .map(|option| option.label)
            .collect::<Vec<_>>();
        let expected_labels = LogTextEncoding::manual_options()
            .into_iter()
            .map(|encoding| encoding.label().to_string())
            .collect::<Vec<_>>();

        assert_eq!(actual_labels, expected_labels);
    }

    /// 验证编码 Select 迁移后仍沿用原有按钮、菜单和菜单项尺寸。
    #[test]
    fn 编码_select_metrics_保持现有尺寸常量() {
        let metrics = MainView::encoding_select_metrics();

        assert_eq!(metrics.button_width, ENCODING_DROPDOWN_BUTTON_WIDTH);
        assert_eq!(metrics.button_height, ENCODING_DROPDOWN_BUTTON_HEIGHT);
        assert_eq!(metrics.menu_width, ENCODING_DROPDOWN_WIDTH);
        assert_eq!(metrics.item_height, ENCODING_DROPDOWN_ITEM_HEIGHT);
        assert_eq!(metrics.menu_gap, ENCODING_DROPDOWN_MENU_GAP);
    }
}
