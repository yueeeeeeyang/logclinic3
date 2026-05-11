// 日志 tab 右键菜单协调。
//
// 业务意图：
// - 本文件由原主窗口工作区实现机械拆分而来，只承载 GPUI 壳层内的 tab 右键菜单渲染、关闭动作和分页资源清理。
// - 拆分过程保持所有状态字段、事件时序和用户可见行为不变，便于后续按视图职责继续收束。

use super::*;

impl MainView {
    pub(in crate::app) fn open_tab_context_menu(
        &mut self,
        tab_id: usize,
        window_x: f32,
        window_y: f32,
        context: &mut Context<Self>,
    ) {
        // GPUI 鼠标事件给出的是窗口内容坐标，而右键菜单作为右侧面板内部的绝对定位元素渲染。
        // 这里把坐标转换到右侧面板局部坐标，避免菜单因为左侧目录树和顶部工具栏的偏移而显示到错误位置。
        let panel_x = (window_x - self.right_panel_left_offset()).max(0.0);
        let panel_y = (window_y - TOOLBAR_HEIGHT).max(0.0);
        self.activate_tab(tab_id);
        self.log.tab_context_menu = Some(TabContextMenu {
            tab_id,
            x: panel_x,
            y: panel_y,
        });
        self.search.search_results_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        context.notify();
    }

    /// 渲染 tab 右键菜单。
    ///
    /// 业务意图：
    /// - 自绘菜单保证 macOS 和 Windows 的 tab 关闭命令行为一致，不依赖平台窗口系统菜单。
    pub(in crate::app) fn render_tab_context_menu(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = &self.log.tab_context_menu else {
            return div().id("tab-context-menu-empty").hidden();
        };
        let tab_id = menu.tab_id;
        let palette = self.palette();

        div()
            .id("tab-context-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(TAB_CONTEXT_MENU_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // tab 菜单的上下留白也属于菜单命中区域，必须阻止事件继续触发底层 tab。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 菜单上右键不应重新定位底层 tab 的上下文菜单。
                    context.stop_propagation();
                }),
            )
            .child(self.render_tab_context_menu_item(
                tab_id,
                TabContextMenuAction::Current,
                "关闭当前",
                palette,
                context,
            ))
            .child(self.render_tab_context_menu_item(
                tab_id,
                TabContextMenuAction::OtherTabs,
                "关闭其他",
                palette,
                context,
            ))
            .child(self.render_tab_context_menu_item(
                tab_id,
                TabContextMenuAction::AllTabs,
                "关闭所有",
                palette,
                context,
            ))
    }

    /// 渲染 tab 右键菜单单项。
    ///
    /// 业务意图：
    /// - 菜单项在鼠标按下时立即执行关闭命令并收起菜单，和左侧目录树右键菜单保持一致。
    /// - 自绘弹层同时存在透明关闭遮罩，使用 `mouse_down` 可以避免 `click` 在菜单收起或鼠标轻微移动后丢失。
    pub(in crate::app) fn render_tab_context_menu_item(
        &self,
        tab_id: usize,
        action: TabContextMenuAction,
        label: &'static str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("tab-menu-{}-{}", tab_id, label)))
            .flex()
            .items_center()
            .h(px(TAB_CONTEXT_MENU_ITEM_HEIGHT))
            .px_3()
            .text_sm()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |item| item.bg(rgb(palette.hover)))
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                    view.handle_tab_context_menu_action(tab_id, action, context);
                    context.stop_propagation();
                }),
            )
    }

    /// 执行 tab 右键菜单命令。
    ///
    /// 业务意图：
    /// - 三个关闭命令集中处理，保证关闭后 active tab 和菜单状态一致。
    pub(in crate::app) fn handle_tab_context_menu_action(
        &mut self,
        tab_id: usize,
        action: TabContextMenuAction,
        context: &mut Context<Self>,
    ) {
        // 先收起所有右侧弹层，再执行关闭动作，避免“关闭所有”后旧菜单仍参与下一帧命中测试。
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.search.search_results_context_menu = None;
        self.log.log_viewer_context_menu = None;
        match action {
            TabContextMenuAction::Current => self.close_tab(tab_id),
            TabContextMenuAction::OtherTabs => self.close_other_tabs(tab_id),
            TabContextMenuAction::AllTabs => self.close_all_tabs(),
        }
        context.notify();
    }

    /// 关闭指定 tab。
    ///
    /// 边界条件：
    /// - 如果关闭的是当前激活 tab，则优先激活当前位置后面的 tab，否则激活前一个 tab。
    pub(in crate::app) fn close_tab(&mut self, tab_id: usize) {
        let Some(index) = self.log.open_tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };
        let closed_tab = self.log.open_tabs.remove(index);
        Self::cleanup_tab_paged_resources(&closed_tab);

        if self.log.active_tab_id == Some(tab_id) {
            self.log.active_tab_id = self
                .log
                .open_tabs
                .get(index)
                .or_else(|| {
                    index
                        .checked_sub(1)
                        .and_then(|previous| self.log.open_tabs.get(previous))
                })
                .map(|tab| tab.id);
            self.clear_search_current_file_match_count();
        }
        if let Some(active_tab_id) = self.log.active_tab_id {
            self.scroll_tab_bar_to_tab(active_tab_id);
        }
        if self
            .log
            .encoding_dropdown_menu
            .as_ref()
            .is_some_and(|menu| menu.tab_id == tab_id)
        {
            self.log.encoding_dropdown_menu = None;
        }
        if self
            .log
            .log_scrollbar_drag
            .is_some_and(|drag| drag.tab_id == tab_id)
        {
            self.log.log_scrollbar_drag = None;
        }
        if self
            .log
            .log_viewer_context_menu
            .as_ref()
            .is_some_and(|menu| menu.tab_id == tab_id)
        {
            self.log.log_viewer_context_menu = None;
        }
    }

    /// 关闭指定 tab 之外的所有 tab。
    ///
    /// 业务意图：
    /// - 保留右键点击的 tab，并把它设为当前激活 tab。
    pub(in crate::app) fn close_other_tabs(&mut self, tab_id: usize) {
        let mut retained = Vec::new();
        for tab in self.log.open_tabs.drain(..) {
            if tab.id == tab_id {
                retained.push(tab);
            } else {
                Self::cleanup_tab_paged_resources(&tab);
            }
        }
        self.log.open_tabs = retained;
        self.log.active_tab_id = self.log.open_tabs.first().map(|tab| tab.id);
        self.clear_search_current_file_match_count();
        self.log
            .tab_bar_scroll_handle
            .set_offset(point(px(0.0), px(0.0)));
        if self
            .log
            .encoding_dropdown_menu
            .as_ref()
            .is_some_and(|menu| menu.tab_id != tab_id)
        {
            self.log.encoding_dropdown_menu = None;
        }
        if self
            .log
            .log_scrollbar_drag
            .is_some_and(|drag| drag.tab_id != tab_id)
        {
            self.log.log_scrollbar_drag = None;
        }
        if self
            .log
            .log_viewer_context_menu
            .as_ref()
            .is_some_and(|menu| menu.tab_id != tab_id)
        {
            self.log.log_viewer_context_menu = None;
        }
    }

    /// 关闭所有日志 tab。
    ///
    /// 业务意图：
    /// - 清空右侧工作区后回到“点击左侧日志文件查看内容”的友好提示。
    pub(in crate::app) fn close_all_tabs(&mut self) {
        for tab in &self.log.open_tabs {
            Self::cleanup_tab_paged_resources(tab);
        }
        self.log.open_tabs.clear();
        self.log.active_tab_id = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.search.search_results_context_menu = None;
        self.log.log_viewer_context_menu = None;
        self.log.log_scrollbar_drag = None;
        self.log.tab_bar_scroll_handle = ScrollHandle::new();
        self.clear_search_current_file_match_count();
    }

    /// 清理 tab 关联的分页临时文件。
    ///
    /// 业务意图：
    /// - 压缩包内超大日志会物化到应用临时目录，tab 关闭后应立即释放磁盘空间。
    /// - 普通本地大文件没有 `materialized_temp_path`，不会被误删。
    pub(in crate::app) fn cleanup_tab_paged_resources(tab: &OpenLogTab) {
        if let LogTabState::Ready { document } = &tab.state
            && let LogTabDocument::Paged(document) = document.as_ref()
            && let Some(temp_path) = document.materialized_temp_path.as_deref()
        {
            cleanup_materialized_file(temp_path);
        }
    }
}
