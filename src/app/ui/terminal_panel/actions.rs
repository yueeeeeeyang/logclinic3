// 独立本地终端页动作。
//
// 业务意图：
// - 本文件承接“终端”主功能页的副作用：启动 PTY、关闭 tab、轮询输出、复制粘贴、右键菜单和文件管理窗口。
// - 渲染层只描述 UI，所有会改变状态或触发后台任务的逻辑都集中在这里。
//
// 线程约束：
// - `LocalPtyBackend` 通过通道和后台线程工作，不能在 PTY 线程直接读取或修改 GPUI 状态。
// - 文件管理窗口创建需要通过 `window.defer` 延后到当前实体更新结束后，避免 GPUI 实体重入读取保护。

use std::{sync::mpsc, time::Duration};

use alacritty_terminal::index::Point;

use super::*;

impl MainView {
    /// 新建一个本地终端 tab。
    ///
    /// 业务意图：
    /// - “终端”页不需要连接配置，点击新增或复制 tab 时应立即获得一个独立本机 shell。
    /// - 每个 tab 使用独立 PTY 后端，便于用户同时运行多组命令。
    pub(in crate::app) fn open_terminal_tab(
        &mut self,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let size = TerminalSize::default();
        let tab_id = self.terminal.next_tab_id;
        self.terminal.next_tab_id = self.terminal.next_tab_id.saturating_add(1);
        let backend = LocalPtyBackend::new(size).start();
        let focus = context.focus_handle();
        let title = if self.terminal.tabs.is_empty() {
            "本地终端".to_string()
        } else {
            format!("本地终端 {}", tab_id)
        };
        let tab = ConnectionTerminalTab {
            id: tab_id,
            profile_id: format!("terminal-local-{tab_id}"),
            file_target_kind: ConnectionTerminalFileTargetKind::Local,
            title,
            backend,
            emulator: ConnectionTerminalEmulator::new(size),
            status: ConnectionTerminalStatus::Connecting,
            focus,
            selection_anchor: None,
            mouse_reporting_drag: false,
            cell_width: CONNECTION_TERMINAL_CELL_WIDTH,
            content_bounds: None,
            ime: ConnectionTerminalImeState::default(),
            osc7_buffer: Vec::new(),
            last_reported_cwd: None,
            backend_finished: false,
        };

        self.terminal.tab_context_menu = None;
        self.terminal.terminal_context_menu = None;
        self.terminal.tabs.push(tab);
        self.terminal.active_tab_id = Some(tab_id);
        if let Some(active) = self.terminal.active_tab() {
            window.focus(&active.focus);
        }
        self.schedule_terminal_poll(context);
        context.stop_propagation();
        context.notify();
    }

    /// 激活指定本地终端 tab。
    pub(in crate::app) fn activate_terminal_tab(
        &mut self,
        tab_id: usize,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(tab) = self.terminal.tabs.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        self.terminal.active_tab_id = Some(tab_id);
        self.terminal.tab_context_menu = None;
        self.terminal.terminal_context_menu = None;
        window.focus(&tab.focus);
        context.stop_propagation();
        context.notify();
    }

    /// 关闭指定本地终端 tab。
    pub(in crate::app) fn close_terminal_tab(
        &mut self,
        tab_id: usize,
        context: &mut Context<Self>,
    ) {
        self.terminal.close_tab(tab_id);
        context.stop_propagation();
        context.notify();
    }

    /// 按给定像素距离横向滚动本地终端 tab 栏。
    pub(in crate::app) fn scroll_terminal_tab_bar(
        &mut self,
        delta: f32,
        context: &mut Context<Self>,
    ) {
        let current_offset = self.terminal.tab_bar_scroll_handle.offset();
        let next_x = (f32::from(current_offset.x) + delta).max(0.0);
        self.terminal
            .tab_bar_scroll_handle
            .set_offset(point(px(next_x), current_offset.y));
        context.stop_propagation();
        context.notify();
    }

    /// 打开本地终端 tab 右键菜单。
    pub(in crate::app) fn open_terminal_tab_context_menu(
        &mut self,
        tab_id: usize,
        window_x: f32,
        window_y: f32,
        context: &mut Context<Self>,
    ) {
        if !self.terminal.tabs.iter().any(|tab| tab.id == tab_id) {
            return;
        }
        let panel_x = (window_x - MAIN_NAV_WIDTH).max(0.0);
        let panel_y = window_y.max(0.0);
        self.terminal.active_tab_id = Some(tab_id);
        self.terminal.tab_context_menu = Some(TerminalTabContextMenu {
            tab_id,
            x: panel_x,
            y: panel_y,
        });
        self.terminal.terminal_context_menu = None;
        context.notify();
    }

    /// 执行本地终端 tab 右键菜单动作。
    pub(in crate::app) fn handle_terminal_tab_context_menu_action(
        &mut self,
        tab_id: usize,
        action: TerminalTabContextMenuAction,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.terminal.tab_context_menu = None;
        self.terminal.terminal_context_menu = None;
        match action {
            TerminalTabContextMenuAction::Duplicate => self.open_terminal_tab(window, context),
            TerminalTabContextMenuAction::Current => self.terminal.close_tab(tab_id),
            TerminalTabContextMenuAction::OtherTabs => self.terminal.close_other_tabs(tab_id),
            TerminalTabContextMenuAction::AllTabs => self.terminal.close_all_tabs(),
        }
        context.stop_propagation();
        context.notify();
    }

    /// 打开本地终端正文右键菜单。
    pub(in crate::app) fn open_terminal_context_menu(
        &mut self,
        tab_id: usize,
        window_x: f32,
        window_y: f32,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(tab) = self.terminal.tabs.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        let panel_x = (window_x - MAIN_NAV_WIDTH).max(0.0);
        let panel_y = window_y.max(0.0);
        self.terminal.active_tab_id = Some(tab_id);
        window.focus(&tab.focus);
        self.terminal.terminal_context_menu = Some(TerminalContextMenu {
            tab_id,
            x: panel_x,
            y: panel_y,
        });
        self.terminal.tab_context_menu = None;
        context.notify();
    }

    /// 执行本地终端正文右键菜单动作。
    pub(in crate::app) fn handle_terminal_context_menu_action(
        &mut self,
        tab_id: usize,
        action: TerminalContextMenuAction,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.terminal.terminal_context_menu = None;
        self.terminal.active_tab_id = Some(tab_id);
        match action {
            TerminalContextMenuAction::FileManager => {
                self.open_terminal_file_manager_for_tab(tab_id, window, context);
            }
            TerminalContextMenuAction::Copy => {
                let _ = self.copy_selected_terminal_text(context);
            }
            TerminalContextMenuAction::Paste => {
                let _ = self.paste_clipboard_text_into_terminal(context);
            }
        }
        context.stop_propagation();
        context.notify();
    }

    /// 打开当前本地终端的文件管理窗口。
    ///
    /// 边界条件：
    /// - 本地终端当前目录不再从终端输出猜测，文件管理统一从当前用户家目录打开，用户可在地址栏手动切换。
    fn open_terminal_file_manager_for_tab(
        &mut self,
        tab_id: usize,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(tab) = self.terminal.tabs.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        let title = format!("{} - 文件管理", tab.title);
        let initial_path = local_home_directory_text();
        let main_view = context.entity();
        window.defer(context, move |_window, app| {
            let window_options = connection_file_manager_window_options(&title, app);
            let main_view_for_window = main_view.clone();
            let main_view_for_error = main_view.clone();
            let open_result = app.open_window(window_options, move |_window, app| {
                let backend = start_connection_file_backend(ConnectionFileBackendTarget::Local);
                app.new(|context| {
                    ConnectionFileManagerWindowView::new(
                        main_view_for_window,
                        title,
                        backend,
                        initial_path,
                        false,
                        context,
                    )
                })
            });
            if open_result.is_err() {
                let _ = main_view_for_error.update(app, |view, context| {
                    view.terminal.status_message = Some("打开文件管理窗口失败".to_string());
                    context.notify();
                });
            }
        });
    }

    /// 根据终端内容区实际 bounds 同步本地 PTY 尺寸。
    pub(in crate::app) fn sync_terminal_size_from_bounds(
        &mut self,
        tab_id: usize,
        bounds: Bounds<Pixels>,
        cell_width: f32,
        context: &mut Context<Self>,
    ) {
        let Some(tab) = self.terminal.tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };
        tab.content_bounds = Some(bounds);
        tab.cell_width = cell_width.max(1.0);
        let next_size =
            connection_terminal_size_from_bounds_with_cell_width(bounds, tab.cell_width);
        if tab.emulator.size == next_size {
            return;
        }
        tab.resize(next_size);
        context.notify();
    }

    /// 安排本地终端事件轮询。
    pub(in crate::app) fn schedule_terminal_poll(&mut self, context: &mut Context<Self>) {
        if self.terminal.terminal_poll_scheduled {
            return;
        }
        self.terminal.terminal_poll_scheduled = true;
        context
            .spawn(async move |view, app| {
                loop {
                    app.background_executor()
                        .timer(Duration::from_millis(CONNECTIONS_TERMINAL_POLL_MILLIS))
                        .await;
                    let keep_polling = view
                        .update(app, |view, context| {
                            view.drain_terminal_events(context);
                            view.terminal.terminal_poll_scheduled
                        })
                        .unwrap_or(false);
                    if !keep_polling {
                        break;
                    }
                }
            })
            .detach();
    }

    /// 轮询所有本地终端 tab 的后台事件。
    pub(in crate::app) fn drain_terminal_events(&mut self, context: &mut Context<Self>) {
        let mut received_event = false;
        for tab in &mut self.terminal.tabs {
            if tab.backend_finished {
                continue;
            }
            loop {
                let event = match tab.backend.event_receiver.try_recv() {
                    Ok(event) => {
                        received_event = true;
                        event
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        received_event = true;
                        tab.backend_finished = true;
                        TerminalBackendEvent::Exited("终端后端已断开".to_string())
                    }
                };
                match event {
                    TerminalBackendEvent::Connected => {
                        tab.status = ConnectionTerminalStatus::Connected;
                    }
                    TerminalBackendEvent::Output(bytes) => {
                        if let Some(cwd) = update_osc7_cwd_from_output(&mut tab.osc7_buffer, &bytes)
                        {
                            tab.last_reported_cwd = Some(cwd);
                        }
                        tab.emulator.feed_output(&bytes);
                    }
                    TerminalBackendEvent::Exited(message) => {
                        tab.status = ConnectionTerminalStatus::Exited(message);
                        tab.backend_finished = true;
                        break;
                    }
                    TerminalBackendEvent::Error(message) => {
                        tab.status = ConnectionTerminalStatus::Error(message);
                        tab.backend_finished = true;
                        break;
                    }
                    TerminalBackendEvent::HostKeyUnknown { .. }
                    | TerminalBackendEvent::HostKeyMismatch { .. } => {
                        tab.status = ConnectionTerminalStatus::Error(
                            "本地终端收到不适用的 SSH 主机指纹事件".to_string(),
                        );
                        tab.backend_finished = true;
                        break;
                    }
                }
            }
        }
        if self.terminal.tabs.iter().all(|tab| tab.backend_finished) {
            self.terminal.terminal_poll_scheduled = false;
        }
        if received_event {
            context.notify();
        }
    }

    /// 判断独立终端页的当前 tab 是否可接收键盘输入。
    pub(in crate::app) fn terminal_focused_without_modal(&self, window: &Window) -> bool {
        self.navigation.active_main_feature == MainFeature::Terminal
            && self.terminal.tab_context_menu.is_none()
            && self.terminal.terminal_context_menu.is_none()
            && self
                .terminal
                .active_tab()
                .is_some_and(|tab| tab.focus.is_focused(window))
    }

    /// 判断当前页面是否有任意终端可接收平台输入法事件。
    pub(in crate::app) fn terminal_input_focused_without_modal(&self, window: &Window) -> bool {
        match self.navigation.active_main_feature {
            MainFeature::Terminal => self.terminal_focused_without_modal(window),
            MainFeature::Connections => self.connection_terminal_focused_without_modal(window),
            _ => false,
        }
    }

    /// 返回当前聚焦终端 tab 的只读引用。
    pub(in crate::app) fn focused_terminal_tab(
        &self,
        window: &Window,
    ) -> Option<&ConnectionTerminalTab> {
        match self.navigation.active_main_feature {
            MainFeature::Terminal => self.terminal.active_tab().filter(|tab| {
                self.terminal_focused_without_modal(window) && tab.focus.is_focused(window)
            }),
            MainFeature::Connections => self.connections.active_tab().filter(|tab| {
                self.connection_terminal_focused_without_modal(window)
                    && tab.focus.is_focused(window)
            }),
            _ => None,
        }
    }

    /// 返回当前聚焦终端 tab 的可变引用。
    pub(in crate::app) fn focused_terminal_tab_mut(
        &mut self,
        window: &Window,
    ) -> Option<&mut ConnectionTerminalTab> {
        match self.navigation.active_main_feature {
            MainFeature::Terminal => {
                if self.terminal.tab_context_menu.is_some()
                    || self.terminal.terminal_context_menu.is_some()
                {
                    return None;
                }
                let active_tab_id = self.terminal.active_tab_id?;
                let tab = self
                    .terminal
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.id == active_tab_id)?;
                tab.focus.is_focused(window).then_some(tab)
            }
            MainFeature::Connections => {
                if self.connection_modal_open() || self.connections.terminal_context_menu.is_some()
                {
                    return None;
                }
                let active_tab_id = self.connections.active_tab_id?;
                let tab = self
                    .connections
                    .tabs
                    .iter_mut()
                    .find(|tab| tab.id == active_tab_id)?;
                tab.focus.is_focused(window).then_some(tab)
            }
            _ => None,
        }
    }

    /// 处理本地终端键盘输入。
    pub(in crate::app) fn handle_terminal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if !self.terminal_focused_without_modal(window) {
            context.stop_propagation();
            return;
        }
        let Some(tab) = self.terminal.active_tab_mut() else {
            return;
        };
        let Some(bytes) = terminal_input_bytes_for_keystroke(&event.keystroke) else {
            return;
        };
        tab.write_input(bytes);
        context.stop_propagation();
        context.notify();
    }

    /// 复制当前本地终端选区；没有选区时返回 false，让 Ctrl+C 继续作为中断发送。
    pub(in crate::app) fn copy_selected_terminal_text(
        &mut self,
        context: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.terminal.active_tab_mut() else {
            return false;
        };
        let Some(text) = tab.emulator.selected_text() else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        context.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// 粘贴剪贴板文本到当前本地终端。
    pub(in crate::app) fn paste_clipboard_text_into_terminal(
        &mut self,
        context: &mut Context<Self>,
    ) -> bool {
        let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) else {
            return false;
        };
        let Some(tab) = self.terminal.active_tab_mut() else {
            return false;
        };
        let bytes = if tab.emulator.bracketed_paste_enabled() {
            format!("\x1b[200~{text}\x1b[201~").into_bytes()
        } else {
            text.into_bytes()
        };
        tab.write_input(bytes);
        true
    }

    /// 根据终端面板鼠标位置开始选区或 xterm 鼠标上报。
    pub(in crate::app) fn begin_terminal_selection(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let Some(point) = self.active_terminal_point_from_window_position(
            f32::from(event.position.x),
            f32::from(event.position.y),
        ) else {
            return;
        };
        let Some(tab) = self.terminal.active_tab_mut() else {
            return;
        };
        if tab.emulator.mouse_reporting_enabled() {
            tab.write_input(terminal_sgr_mouse_report(0, point, true));
            tab.mouse_reporting_drag = true;
            context.stop_propagation();
            context.notify();
            return;
        }
        tab.selection_anchor = Some(point);
        tab.emulator.begin_selection(point);
        context.stop_propagation();
        context.notify();
    }

    /// 拖拽更新本地终端选区。
    pub(in crate::app) fn update_terminal_selection(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        let Some(point) = self.active_terminal_point_from_window_position(
            f32::from(event.position.x),
            f32::from(event.position.y),
        ) else {
            return;
        };
        let Some(tab) = self.terminal.active_tab_mut() else {
            return;
        };
        if tab.mouse_reporting_drag {
            tab.write_input(terminal_sgr_mouse_report(32, point, true));
            context.stop_propagation();
            return;
        }
        if tab.selection_anchor.is_none() {
            return;
        }
        tab.emulator.update_selection(point);
        context.stop_propagation();
        context.notify();
    }

    /// 结束本地终端选区拖拽。
    pub(in crate::app) fn end_terminal_selection(
        &mut self,
        event: &MouseUpEvent,
        context: &mut Context<Self>,
    ) -> bool {
        let release_point = self.active_terminal_point_from_window_position(
            f32::from(event.position.x),
            f32::from(event.position.y),
        );
        let mut consumed = false;
        if let Some(tab) = self.terminal.active_tab_mut() {
            if tab.mouse_reporting_drag {
                if let Some(point) = release_point {
                    tab.write_input(terminal_sgr_mouse_report(0, point, false));
                }
                tab.mouse_reporting_drag = false;
                consumed = true;
            }
            if tab.selection_anchor.is_some() {
                consumed = true;
                tab.selection_anchor = None;
            }
        }
        if consumed {
            context.stop_propagation();
            context.notify();
        }
        consumed
    }

    /// 处理终端页鼠标移动。
    pub(in crate::app) fn handle_terminal_page_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.update_terminal_selection(event, context);
    }

    /// 处理终端页鼠标释放。
    pub(in crate::app) fn handle_terminal_page_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.end_terminal_selection(event, context) {
            context.stop_propagation();
        }
    }

    /// 将窗口坐标换算为当前本地终端的 alacritty 格点。
    fn active_terminal_point_from_window_position(&self, x: f32, y: f32) -> Option<Point> {
        let tab = self.terminal.active_tab()?;
        let bounds = tab.content_bounds?;
        let local_x = x - f32::from(bounds.left());
        let local_y = y - f32::from(bounds.top());
        Some(
            tab.emulator
                .point_from_panel_offset(local_x, local_y, tab.cell_width),
        )
    }
}
