// 日志正文右键菜单辅助。
//
// 业务意图：
// - 从日志正文查看器中拆出 正文右键菜单、复制/另存为动作和保存来源定位，让正文渲染、滚动、选区和菜单职责更清晰。
// - 本轮只移动方法边界并保留原有中文注释，不改变虚拟列表、分页日志、右键菜单或复制行为。

use super::*;

impl MainView {
    /// 打开日志正文右键菜单。
    ///
    /// 业务意图：
    /// - 菜单位置使用右键点击位置，并转换成右侧日志面板内部坐标，保证单日志模式和左右分栏模式都能正确定位。
    /// - 打开正文菜单时关闭其它右侧弹层，避免多个自绘菜单重叠导致命令作用对象不清晰。
    pub(super) fn open_log_viewer_context_menu(
        &mut self,
        tab_id: usize,
        window_x: f32,
        window_y: f32,
        context: &mut Context<Self>,
    ) {
        if !self.log.open_tabs.iter().any(|tab| tab.id == tab_id) {
            return;
        }
        let panel_x = (window_x - self.right_panel_left_offset()).max(0.0);
        let panel_y = (window_y - TOOLBAR_HEIGHT).max(0.0);
        self.log.log_viewer_context_menu = Some(LogViewerContextMenu {
            tab_id,
            x: panel_x,
            y: panel_y,
        });
        self.log.tab_context_menu = None;
        self.search.search_results_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        context.notify();
    }

    /// 渲染日志正文右键菜单。
    ///
    /// 业务意图：
    /// - 正文查看器是自绘只读列表，复制和另存为需要应用自己提供菜单，不能依赖平台文本控件菜单。
    pub(super) fn render_log_viewer_context_menu(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = &self.log.log_viewer_context_menu else {
            return div().id("log-viewer-context-menu-empty").hidden();
        };
        let tab_id = menu.tab_id;
        let palette = self.palette();
        let copy_enabled = self.selected_log_text_for_tab(tab_id).is_some();

        div()
            .id("log-viewer-context-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(LOG_VIEWER_CONTEXT_MENU_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(self.render_log_viewer_context_menu_item(
                tab_id,
                LogViewerContextMenuAction::Copy,
                "复制",
                copy_enabled,
                palette,
                context,
            ))
            .child(self.render_log_viewer_context_menu_item(
                tab_id,
                LogViewerContextMenuAction::SaveAs,
                "另存为...",
                true,
                palette,
                context,
            ))
    }

    /// 渲染日志正文右键菜单单项。
    ///
    /// 业务意图：
    /// - “复制”在没有选区时仍展示但禁用，符合用户要求的“有选中内容时，可以点击”。
    pub(super) fn render_log_viewer_context_menu_item(
        &self,
        tab_id: usize,
        action: LogViewerContextMenuAction,
        label: &'static str,
        enabled: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!(
                "log-viewer-menu-{}-{}",
                tab_id, label
            )))
            .flex()
            .items_center()
            .h(px(LOG_VIEWER_CONTEXT_MENU_ITEM_HEIGHT))
            .px_3()
            .text_sm()
            .text_color(rgb(if enabled {
                palette.text
            } else {
                palette.muted_text
            }))
            .cursor_pointer()
            .when(enabled, move |item| {
                item.hover(move |item| item.bg(rgb(palette.hover)))
            })
            .when(!enabled, |item| item.opacity(0.55))
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    if enabled {
                        view.handle_log_viewer_context_menu_action(tab_id, action, context);
                    }
                    context.stop_propagation();
                }),
            )
    }

    /// 执行日志正文右键菜单命令。
    ///
    /// 业务意图：
    /// - 复制和另存为都绑定右键时的 tab，避免菜单打开后因其它事件切换 active tab 导致作用对象变化。
    pub(super) fn handle_log_viewer_context_menu_action(
        &mut self,
        tab_id: usize,
        action: LogViewerContextMenuAction,
        context: &mut Context<Self>,
    ) {
        self.log.log_viewer_context_menu = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.search.search_results_context_menu = None;
        match action {
            LogViewerContextMenuAction::Copy => {
                let _ = self.copy_selected_log_text_for_tab(tab_id, context);
            }
            LogViewerContextMenuAction::SaveAs => {
                if let Some(source) = self.log_viewer_save_source_for_tab(tab_id) {
                    self.save_log_sources_as(vec![source], context);
                }
            }
        }
        context.notify();
    }

    /// 返回日志正文右键菜单绑定 tab 的另存为来源。
    ///
    /// 业务意图：
    /// - 正文右键菜单命令应始终作用于打开菜单时所在的 tab，不能依赖当前激活 tab，避免用户切换 tab 后保存错文件。
    pub(super) fn log_viewer_save_source_for_tab(&self, tab_id: usize) -> Option<LogFileSource> {
        Self::log_viewer_save_source_for_tab_from_tabs(&self.log.open_tabs, tab_id)
    }

    /// 从打开 tab 集合中查找正文右键菜单绑定的另存为来源。
    ///
    /// 业务意图：
    /// - 拆成纯函数便于测试，避免右键菜单另存为入口因为 tab 查找错误而无声失败。
    pub(super) fn log_viewer_save_source_for_tab_from_tabs(
        tabs: &[OpenLogTab],
        tab_id: usize,
    ) -> Option<LogFileSource> {
        tabs.iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| tab.source.clone())
    }
}
