// 主工作区鼠标事件与面板协调。
//
// 业务意图：
// - 本文件由原主窗口工作区实现机械拆分而来，只承载 GPUI 壳层内的 搜索结果面板、目录树滚动条、分隔条和右侧空态渲染。
// - 拆分过程保持所有状态字段、事件时序和用户可见行为不变，便于后续按功能域继续收束。

use super::*;

impl MainView {
    /// 处理根视图鼠标移动。
    ///
    /// 业务意图：
    /// - 结果面板高度调整和结果面板滚动条拖动都可能跨过右侧正文、左侧树或工具栏区域，因此放到根视图统一处理。
    /// - 搜索对话框已经改为独立窗口，不再需要主窗口接管拖动过程。
    /// - 现有左右分割线和滚动条拖动仍在内容区处理，避免扩大它们的鼠标命中范围。
    pub(in crate::app) fn handle_root_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.update_search_results_resize_drag(event, window, context);
        self.update_search_results_scrollbar_drag(event, context);
    }

    /// 处理根视图鼠标释放。
    ///
    /// 业务意图：
    /// - 结果面板 resize 和结果面板滚动条拖动都依赖鼠标释放清理临时状态。
    pub(in crate::app) fn handle_root_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let had_drag = self.search.search_results_resize_drag.take().is_some();
        let had_scrollbar_drag = self.search.search_results_scrollbar_drag.take().is_some();
        self.stop_log_text_selection(context);
        if had_drag || had_scrollbar_drag {
            context.notify();
        }
    }

    /// 开始调整搜索结果面板高度。
    pub(in crate::app) fn start_search_results_resize(&mut self, event: &MouseDownEvent) {
        let Some(panel) = &self.search.search_results_panel else {
            return;
        };
        self.search.search_results_resize_drag = Some(SearchResultsResizeDrag {
            start_y: event.position.y,
            start_height: panel.height,
        });
        self.search.search_results_scrollbar_drag = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
    }

    /// 根据鼠标拖动更新搜索结果面板高度。
    pub(in crate::app) fn update_search_results_resize_drag(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.search.search_results_resize_drag else {
            return;
        };
        if !event.dragging() {
            self.search.search_results_resize_drag = None;
            context.notify();
            return;
        }

        let Some(panel) = self.search.search_results_panel.as_mut() else {
            self.search.search_results_resize_drag = None;
            context.notify();
            return;
        };
        let content_height = (f32::from(window.viewport_size().height) - TOOLBAR_HEIGHT).max(1.0);
        let max_height =
            (content_height * SEARCH_RESULTS_PANEL_MAX_RATIO).max(SEARCH_RESULTS_PANEL_MIN_HEIGHT);
        let next_height = drag.start_height + f32::from(drag.start_y - event.position.y);
        panel.height = next_height.clamp(SEARCH_RESULTS_PANEL_MIN_HEIGHT, max_height);
        context.notify();
    }

    /// 开始拖动搜索结果面板滚动条滑块。
    ///
    /// 业务意图：
    /// - 搜索结果面板可能包含大量历史记录和命中行，需要支持直接拖动滚动条快速定位。
    /// - 拖动开始时记录鼠标在滑块内的偏移，避免滑块突然跳到鼠标中心。
    ///
    /// 边界条件：
    /// - 如果结果面板未打开、列表尚未完成测量或内容不足以滚动，则忽略本次按下。
    pub(in crate::app) fn start_search_results_scrollbar_drag(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        self.note_keyboard_scroll_region(KeyboardScrollRegion::SearchResults);
        let Some(panel) = &self.search.search_results_panel else {
            return;
        };
        let Some(metrics) = Self::search_results_scrollbar_metrics(&panel.scroll_handle) else {
            return;
        };
        if metrics.max_scroll <= px(0.0) {
            return;
        }
        let Some(viewport_top) = Self::uniform_list_viewport_axis_origin(
            &panel.scroll_handle,
            LogScrollbarAxis::Vertical,
        ) else {
            return;
        };

        self.search.search_results_scrollbar_drag = Some(SearchResultsScrollbarDrag {
            cursor_offset: event.position.y - viewport_top - metrics.thumb_start,
        });
        self.search.search_results_resize_drag = None;
        self.search.search_results_context_menu = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        self.stop_log_text_selection(context);
    }

    /// 根据鼠标移动更新搜索结果面板滚动条拖动结果。
    ///
    /// 业务意图：
    /// - 自绘滚动条拖动必须写回搜索结果虚拟列表的底层滚动偏移，才能和滚轮滚动、虚拟渲染保持一致。
    /// - 只在鼠标左键仍按下时更新；如果释放发生在其它区域，也会在下一次移动时清理拖动状态。
    pub(in crate::app) fn update_search_results_scrollbar_drag(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.search.search_results_scrollbar_drag else {
            return;
        };
        if !event.dragging() {
            self.search.search_results_scrollbar_drag = None;
            context.notify();
            return;
        }
        let Some(panel) = self.search.search_results_panel.as_ref() else {
            self.search.search_results_scrollbar_drag = None;
            context.notify();
            return;
        };
        let Some(metrics) = Self::search_results_scrollbar_metrics(&panel.scroll_handle) else {
            self.search.search_results_scrollbar_drag = None;
            context.notify();
            return;
        };
        let Some(viewport_top) = Self::uniform_list_viewport_axis_origin(
            &panel.scroll_handle,
            LogScrollbarAxis::Vertical,
        ) else {
            self.search.search_results_scrollbar_drag = None;
            context.notify();
            return;
        };

        let movable_length = (metrics.track_length - metrics.thumb_length).max(px(0.0));
        if metrics.max_scroll <= px(0.0) || movable_length <= px(0.0) {
            return;
        }

        let requested_thumb_start = event.position.y - viewport_top - drag.cursor_offset;
        let thumb_start =
            requested_thumb_start.clamp(metrics.track_start, metrics.track_start + movable_length);
        let scroll_offset =
            metrics.max_scroll * ((thumb_start - metrics.track_start) / movable_length);
        let base_scroll_handle = {
            // `UniformListScrollHandle` 包装了真正的 `ScrollHandle`；克隆后再写入，避免持有 RefCell 借用时触发嵌套借用。
            panel.scroll_handle.0.borrow().base_handle.clone()
        };
        let current_offset = base_scroll_handle.offset();
        base_scroll_handle.set_offset(point(current_offset.x, -scroll_offset));
        context.notify();
    }

    /// 根据真实加载节点类型返回目录树图标和颜色。
    ///
    /// 业务意图：
    /// - 将类型到视觉符号的映射集中管理，后续新增节点类型时不会散落在多个渲染分支里。
    pub(in crate::app) fn loaded_log_tree_icon(
        kind: LogTreeEntryKind,
        palette: AppThemePalette,
    ) -> (Icon, u32) {
        match kind {
            LogTreeEntryKind::Directory => (Icon::FolderOpen, palette.accent),
            LogTreeEntryKind::File => (Icon::FileText, palette.muted_text),
            LogTreeEntryKind::Archive => (Icon::FileArchive, 0x8250df),
            LogTreeEntryKind::Symlink => (Icon::FolderSymlink, palette.muted_text),
            LogTreeEntryKind::Error => (Icon::FileX, palette.error),
        }
    }

    /// 渲染目录树节点右侧的补充信息。
    ///
    /// 业务意图：
    /// - 元信息用于展示文件大小或错误类别，让目录树信息更便于扫描。
    /// - 没有元信息的节点仍通过统一函数返回空占位，保持行模板简单稳定。
    ///
    /// 边界条件：
    /// - 当前元信息不参与排序、过滤或诊断，只是短文本展示。
    /// - 后续若元信息可能很长，需要定义截断、悬浮提示和优先级规则。
    pub(in crate::app) fn render_log_tree_meta(
        meta: Option<&str>,
        palette: AppThemePalette,
    ) -> gpui::Div {
        let meta_element = div()
            .flex_none()
            .text_size(px(LOG_TREE_FONT_SIZE))
            .text_color(rgb(palette.muted_text))
            .child(meta.unwrap_or_default().to_string());

        if meta.is_some() {
            meta_element
        } else {
            meta_element.hidden()
        }
    }

    /// 开始拖动左右分栏分割线。
    ///
    /// 业务意图：
    /// - 用户按下分割线后进入调整模式，后续鼠标移动将改变左侧区域宽度。
    ///
    /// 边界条件：
    /// - 只响应鼠标左键，避免右键菜单或其它鼠标键误触发布局变化。
    /// - 按下瞬间不立即改变宽度，实际宽度在移动事件中按当前位置计算。
    pub(in crate::app) fn start_resizing_splitter(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.is_resizing_splitter = true;
        // 分栏拖拽条覆盖在左右内容之间，按下后必须截断，避免底层目录树或日志区同步收到鼠标事件。
        context.stop_propagation();
    }

    /// 开始拖动左侧目录树滚动条滑块。
    ///
    /// 业务意图：
    /// - 目录树节点较多时，用户需要可以直接拖动滚动条快速定位，而不是只能依赖滚轮。
    /// - 记录鼠标在滑块内的偏移，避免拖动开始时滑块位置突变。
    ///
    /// 边界条件：
    /// - 如果列表尚未完成测量或内容不足以滚动，则忽略本次按下。
    pub(in crate::app) fn start_log_tree_scrollbar_drag(&mut self, event: &MouseDownEvent) {
        self.note_keyboard_scroll_region(KeyboardScrollRegion::LogTree);
        let Some(metrics) = Self::log_tree_scrollbar_metrics(&self.log.log_tree_scroll_handle)
        else {
            return;
        };
        if metrics.max_scroll <= px(0.0) {
            return;
        }
        let Some(viewport_top) = Self::uniform_list_viewport_axis_origin(
            &self.log.log_tree_scroll_handle,
            LogScrollbarAxis::Vertical,
        ) else {
            return;
        };

        self.log.log_tree_scrollbar_drag = Some(LogTreeScrollbarDrag {
            cursor_offset: event.position.y - viewport_top - metrics.thumb_start,
        });
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
    }

    /// 处理内容区鼠标移动事件。
    ///
    /// 业务意图：
    /// - 左右分割线拖动、左侧树滚动条拖动和日志滚动条拖动都依赖窗口级鼠标移动；统一入口可以保证这些交互在同一帧内得到更新。
    /// - 该方法只分发交互，不直接渲染 UI，便于后续继续追加其它拖动类操作。
    pub(in crate::app) fn handle_content_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.resize_splitter(event, window, context);
        self.update_log_tree_scrollbar_drag(event, context);
        self.update_log_scrollbar_drag(event, context);
    }

    /// 处理内容区鼠标左键释放事件。
    ///
    /// 业务意图：
    /// - 用户释放鼠标时需要同时结束分割线拖动、左侧树滚动条拖动和日志滚动条拖动，避免后续移动继续改变布局或滚动位置。
    pub(in crate::app) fn handle_content_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.stop_resizing_splitter(event, window, context);
        self.stop_log_tree_scrollbar_drag(context);
        self.stop_log_scrollbar_drag(context);
        self.stop_log_text_selection(context);
    }

    /// 根据鼠标移动更新左侧目录树滚动条拖动结果。
    ///
    /// 业务意图：
    /// - 自绘滚动条拖动必须写回目录树虚拟列表底层滚动偏移，才能和滚轮滚动、虚拟行渲染保持一致。
    /// - 只在鼠标左键仍按下时更新；如果释放发生在其它元素上，也会在下一次移动时清理拖动状态。
    pub(in crate::app) fn update_log_tree_scrollbar_drag(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.log.log_tree_scrollbar_drag else {
            return;
        };
        if !event.dragging() {
            self.log.log_tree_scrollbar_drag = None;
            context.notify();
            return;
        }
        let Some(metrics) = Self::log_tree_scrollbar_metrics(&self.log.log_tree_scroll_handle)
        else {
            self.log.log_tree_scrollbar_drag = None;
            context.notify();
            return;
        };
        let Some(viewport_top) = Self::uniform_list_viewport_axis_origin(
            &self.log.log_tree_scroll_handle,
            LogScrollbarAxis::Vertical,
        ) else {
            self.log.log_tree_scrollbar_drag = None;
            context.notify();
            return;
        };

        let movable_length = (metrics.track_length - metrics.thumb_length).max(px(0.0));
        if metrics.max_scroll <= px(0.0) || movable_length <= px(0.0) {
            return;
        }

        let requested_thumb_start = event.position.y - viewport_top - drag.cursor_offset;
        let thumb_start =
            requested_thumb_start.clamp(metrics.track_start, metrics.track_start + movable_length);
        let scroll_offset =
            metrics.max_scroll * ((thumb_start - metrics.track_start) / movable_length);
        let base_scroll_handle = {
            // 克隆底层滚动句柄后释放 `RefCell` 借用，再写入偏移，避免测量状态借用和滚动状态更新交叉。
            self.log
                .log_tree_scroll_handle
                .0
                .borrow()
                .base_handle
                .clone()
        };
        let current_offset = base_scroll_handle.offset();
        base_scroll_handle.set_offset(point(current_offset.x, -scroll_offset));
        context.notify();
    }

    /// 结束左侧目录树滚动条拖动。
    ///
    /// 业务意图：
    /// - 鼠标释放后清理拖动状态，避免普通鼠标移动继续改变目录树滚动位置。
    pub(in crate::app) fn stop_log_tree_scrollbar_drag(&mut self, context: &mut Context<Self>) {
        if self.log.log_tree_scrollbar_drag.is_some() {
            self.log.log_tree_scrollbar_drag = None;
            context.notify();
        }
    }

    /// 拖动分割线时更新左侧内容区域宽度。
    ///
    /// 业务意图：
    /// - 将鼠标横坐标映射为左侧区域宽度，从而提供直接、可预期的分栏拖动体验。
    ///
    /// 边界条件：
    /// - 只有处于拖动状态时才更新宽度。
    /// - 宽度会被限制在左侧最小宽度和右侧最小可用空间之间，避免任一面板不可用。
    pub(in crate::app) fn resize_splitter(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if !self.is_resizing_splitter {
            return;
        }

        let log_page_x = (f32::from(event.position.x) - MAIN_NAV_WIDTH).max(0.0);
        let log_page_width = (f32::from(window.bounds().size.width) - MAIN_NAV_WIDTH).max(0.0);
        self.left_panel_width = Self::clamp_left_panel_width(log_page_x, log_page_width);
        context.notify();
    }

    /// 结束拖动左右分栏分割线。
    ///
    /// 业务意图：
    /// - 鼠标左键释放后退出调整模式，后续鼠标移动不再改变分栏宽度。
    ///
    /// 边界条件：
    /// - 当前不做宽度持久化，因此释放动作只影响内存状态。
    pub(in crate::app) fn stop_resizing_splitter(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        _context: &mut Context<Self>,
    ) {
        self.is_resizing_splitter = false;
    }

    /// 将左侧区域宽度限制在可用范围内。
    ///
    /// 业务意图：
    /// - 分割线拖动必须保留左右两栏的基本可用空间。
    /// - 独立函数便于后续为该边界逻辑补充单元测试。
    ///
    /// 边界条件：
    /// - 如果窗口极窄导致右侧最小宽度无法满足，仍优先保证左侧不低于最小宽度。
    /// - 当前只使用窗口总宽度计算，工具栏高度不影响水平分割。
    pub(in crate::app) fn clamp_left_panel_width(requested_width: f32, window_width: f32) -> f32 {
        let max_left_width = (window_width - SPLITTER_VISIBLE_WIDTH - RIGHT_PANEL_MIN_WIDTH)
            .max(LEFT_PANEL_MIN_WIDTH);
        requested_width.clamp(LEFT_PANEL_MIN_WIDTH, max_left_width)
    }

    /// 渲染未显示左侧目录树时的主内容提示。
    ///
    /// 业务意图：
    /// - 用户明确要求未加载日志时不显示左侧区域，因此主内容区需要占满窗口剩余空间。
    /// - 提示文案直接说明下一步动作，避免空白内容区让用户误以为界面卡住。
    ///
    /// 边界条件：
    /// - 加载中和加载失败也使用同一块主内容区提示，不提前显示左侧栏。
    /// - 当前提示不承载按钮，避免和顶部“加载日志”入口形成重复操作路径。
    pub(in crate::app) fn render_primary_content_message(&self) -> impl IntoElement {
        let palette = self.palette();
        if let LogTreeLoadState::Loading { message } = &self.log.load_state {
            return div()
                .id("primary-content-loading-message")
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .size_full()
                .px_4()
                .bg(rgb(palette.background))
                .child(Self::render_loading_spinner(palette.accent))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(palette.muted_text))
                        .text_center()
                        .child(message.clone()),
                );
        }

        let (icon, icon_color, title, description) = match &self.log.load_state {
            LogTreeLoadState::Empty => (
                Icon::FileText,
                palette.muted_text,
                "请先加载日志".to_string(),
                "点击日志页顶部“加载日志”，选择日志文件、目录或压缩包。".to_string(),
            ),
            LogTreeLoadState::Failed { message } => (
                Icon::FileX,
                palette.error,
                "加载日志失败".to_string(),
                message.clone(),
            ),
            LogTreeLoadState::Loaded(_) => (
                Icon::FileText,
                palette.muted_text,
                String::new(),
                String::new(),
            ),
            LogTreeLoadState::Loading { .. } => unreachable!("加载态已在上方提前返回"),
        };

        div()
            .id("primary-content-message")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .size_full()
            .px_4()
            .bg(rgb(palette.background))
            .child(Self::render_lucide_icon(Some(icon), 32.0, 28.0, icon_color))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(title),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_text))
                    .child(description),
            )
    }

    /// 渲染右侧日志工作区。
    ///
    /// 业务意图：
    /// - 加载日志后但尚未打开任何文件时，右侧给出“点击左侧日志文件查看内容”的友好提示。
    /// - 打开文件后显示 tab 栏、编码切换工具条和只读日志正文；单日志模式隐藏 tab 栏，把空间留给正文。
    /// - 搜索结果面板打开后作为底部分栏参与布局，日志正文和滚动条高度会自动扣除面板高度。
    ///
    /// 边界条件：
    /// - 当前不持久化 tab，不支持拖拽重排，也不实现复制菜单。
    pub(in crate::app) fn render_right_log_panel(
        &self,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let content = if self.log.open_tabs.is_empty() {
            div()
                .id("right-log-panel-empty")
                .flex()
                .flex_1()
                .size_full()
                .child(self.render_loaded_right_empty_message())
        } else if self.should_hide_log_tree_panel() {
            div()
                .id("right-log-panel-single-tab")
                .flex()
                .flex_col()
                .flex_1()
                .size_full()
                .child(self.render_active_log_tab(context))
        } else {
            div()
                .id("right-log-panel-tabs")
                .flex()
                .flex_col()
                .flex_1()
                .size_full()
                .child(self.render_log_tab_bar(context))
                .child(self.render_active_log_tab(context))
        };

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .child(content)
            .child(self.render_search_results_panel(context))
            .child(self.render_popup_dismiss_overlay(context))
            .child(self.render_tab_context_menu(context))
            .child(self.render_log_viewer_context_menu(context))
            .child(self.render_search_results_context_menu(context))
            .child(self.render_encoding_dropdown_menu(context))
    }
}
