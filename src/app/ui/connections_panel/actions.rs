// 连接页动作和事件处理。
//
// 业务意图：
// - 本文件承接连接配置 CRUD、SSH tab 启动/关闭、终端输入、主机指纹确认和后台事件轮询。
// - 渲染函数只负责声明 UI 结构，不直接写 SQLite、不解密密码、不启动网络任务。

use std::sync::mpsc;
use std::time::Duration;

use alacritty_terminal::index::Point;

use super::*;

impl MainView {
    /// 通过鼠标按下事件切换新建连接类型菜单。
    ///
    /// 业务意图：
    /// - 新建入口是浮层触发器，使用 `mouse_down` 可与菜单项、弹窗按钮保持一致，避免 `click`
    ///   在透明遮罩或焦点切换场景下没有被合成。
    pub(in crate::app) fn toggle_connection_create_menu_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.connections.create_menu_open = !self.connections.create_menu_open;
        context.stop_propagation();
        context.notify();
    }

    /// 关闭新建连接类型菜单。
    pub(in crate::app) fn close_connection_create_menu(&mut self, context: &mut Context<Self>) {
        if self.connections.create_menu_open {
            self.connections.create_menu_open = false;
            context.notify();
        }
    }

    /// 执行新建连接类型菜单动作。
    pub(in crate::app) fn handle_connection_create_menu_action(
        &mut self,
        kind: ConnectionCreateKind,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.connections.create_menu_open = false;
        self.open_connection_create_entry(kind, window, context);
        context.stop_propagation();
        context.notify();
    }

    /// 按指定类型执行新增入口动作。
    ///
    /// 业务意图：
    /// - SSH 需要先打开配置表单，本地终端则直接打开 tab；二者都由新增菜单分发，避免按钮重新写死某一种连接。
    fn open_connection_create_entry(
        &mut self,
        kind: ConnectionCreateKind,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        match kind {
            ConnectionCreateKind::Ssh => {
                let dialog = ConnectionDialogState::create(context);
                window.focus(&dialog.name.focus);
                self.connections.create_menu_open = false;
                self.connections.dialog = Some(dialog);
            }
            ConnectionCreateKind::LocalTerminal => {
                self.open_local_terminal(window, context);
            }
        }
    }

    /// 打开编辑连接弹窗。
    pub(in crate::app) fn open_edit_connection_dialog(
        &mut self,
        profile_id: &str,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(profile) = self.connections.profile_by_id(profile_id).cloned() else {
            self.connections.status_message = Some("连接不存在，无法编辑".to_string());
            context.notify();
            return;
        };
        let dialog = ConnectionDialogState::edit(&profile, context);
        window.focus(&dialog.name.focus);
        self.connections.create_menu_open = false;
        self.connections.dialog = Some(dialog);
        context.notify();
    }

    /// 通过鼠标按下事件关闭新增/编辑连接弹窗。
    ///
    /// 业务意图：
    /// - 连接页包含终端选区、左侧拖拽分栏和弹层等多组鼠标状态，弹窗按钮如果只依赖 `on_click`，
    ///   在焦点切换或鼠标状态被父层消费时可能无法合成点击事件。
    /// - 弹窗按钮使用 `mouse_down` 直接触发业务动作，保证取消和关闭按钮在所有输入焦点下都能立即生效。
    pub(in crate::app) fn close_connection_dialog_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.connections.dialog = None;
        context.stop_propagation();
        context.notify();
    }

    /// 保存新增或编辑连接。
    pub(in crate::app) fn save_connection_dialog(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(path) = self.connections.database_path.clone() else {
            self.set_connection_dialog_error("无法定位应用配置目录，不能保存连接".to_string());
            context.stop_propagation();
            context.notify();
            return;
        };
        let Some(dialog) = self.connections.dialog.as_ref() else {
            return;
        };
        let password_required = matches!(dialog.mode, ConnectionDialogMode::Create);
        let draft = dialog.to_profile_draft();
        let normalized = match validate_connection_profile_draft(&draft, password_required) {
            Ok(normalized) => normalized,
            Err(error) => {
                self.set_connection_dialog_error(error);
                context.stop_propagation();
                context.notify();
                return;
            }
        };
        let (name, host, port, username, password) = normalized;

        let save_result = match dialog.mode.clone() {
            ConnectionDialogMode::Create => {
                let id = new_connection_entity_id("ssh");
                let Some(password) = password else {
                    self.set_connection_dialog_error("密码不能为空".to_string());
                    context.stop_propagation();
                    context.notify();
                    return;
                };
                let encrypted_password = match encrypt_connection_password(&id, password.as_str()) {
                    Ok(encrypted_password) => encrypted_password,
                    Err(error) => {
                        self.set_connection_dialog_error(error);
                        context.stop_propagation();
                        context.notify();
                        return;
                    }
                };
                let now = current_connection_time_millis();
                let profile = ConnectionProfile {
                    id,
                    name,
                    host,
                    port,
                    username,
                    encrypted_password,
                    host_key_fingerprint: None,
                    created_at_ms: now,
                    updated_at_ms: now,
                    last_connected_at_ms: None,
                };
                insert_connection_profile(&path, &profile)
            }
            ConnectionDialogMode::Edit { profile_id } => {
                let Some(existing) = self.connections.profile_by_id(&profile_id).cloned() else {
                    self.set_connection_dialog_error("连接不存在，无法保存".to_string());
                    context.stop_propagation();
                    context.notify();
                    return;
                };
                let encrypted_password = match password {
                    Some(password) => {
                        match encrypt_connection_password(&existing.id, password.as_str()) {
                            Ok(encrypted_password) => encrypted_password,
                            Err(error) => {
                                self.set_connection_dialog_error(error);
                                context.stop_propagation();
                                context.notify();
                                return;
                            }
                        }
                    }
                    None => existing.encrypted_password.clone(),
                };
                let host_key_fingerprint =
                    retained_connection_host_key_fingerprint_after_endpoint_edit(
                        &existing, &host, port,
                    );
                let profile = ConnectionProfile {
                    id: existing.id,
                    name,
                    host,
                    port,
                    username,
                    encrypted_password,
                    host_key_fingerprint,
                    created_at_ms: existing.created_at_ms,
                    updated_at_ms: current_connection_time_millis(),
                    last_connected_at_ms: existing.last_connected_at_ms,
                };
                update_connection_profile(&path, &profile)
            }
        };

        match save_result {
            Ok(()) => {
                self.connections.dialog = None;
                self.connections.reload_profiles();
                self.connections.status_message = Some("连接已保存".to_string());
            }
            Err(error) => self.set_connection_dialog_error(error),
        }
        context.stop_propagation();
        context.notify();
    }

    /// 通过鼠标按下事件保存新增或编辑连接。
    ///
    /// 业务意图：
    /// - 与取消按钮保持同一套弹窗按钮事件模型，避免部分按钮依赖 `click`、部分按钮依赖 `mouse_down`
    ///   造成交互结果不一致。
    pub(in crate::app) fn save_connection_dialog_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.save_connection_dialog(&ClickEvent::default(), window, context);
    }

    /// 在连接弹窗上设置错误消息。
    fn set_connection_dialog_error(&mut self, error: String) {
        if let Some(dialog) = self.connections.dialog.as_mut() {
            dialog.error = Some(error);
        } else {
            self.connections.status_message = Some(error);
        }
    }

    /// 打开删除连接确认弹窗。
    pub(in crate::app) fn open_delete_connection_dialog(
        &mut self,
        profile_id: &str,
        context: &mut Context<Self>,
    ) {
        let Some(profile) = self.connections.profile_by_id(profile_id) else {
            self.connections.status_message = Some("连接不存在，无法删除".to_string());
            context.notify();
            return;
        };
        self.connections.delete_confirm_dialog = Some(ConnectionDeleteConfirmDialog {
            profile_id: profile.id.clone(),
            profile_name: profile.name.clone(),
        });
        self.connections.create_menu_open = false;
        context.notify();
    }

    /// 通过鼠标按下事件关闭删除连接确认弹窗。
    pub(in crate::app) fn close_delete_connection_dialog_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.connections.delete_confirm_dialog = None;
        context.stop_propagation();
        context.notify();
    }

    /// 删除连接并关闭该连接打开的所有 tab。
    pub(in crate::app) fn confirm_delete_connection(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(dialog) = self.connections.delete_confirm_dialog.take() else {
            return;
        };
        let Some(path) = self.connections.database_path.clone() else {
            self.connections.status_message =
                Some("无法定位应用配置目录，不能删除连接".to_string());
            context.stop_propagation();
            context.notify();
            return;
        };

        match delete_connection_profile(&path, &dialog.profile_id) {
            Ok(()) => {
                self.connections.close_tabs_for_profile(&dialog.profile_id);
                self.connections.reload_profiles();
                self.connections.status_message = Some("连接已删除".to_string());
            }
            Err(error) => {
                self.connections.status_message = Some(error);
            }
        }
        context.stop_propagation();
        context.notify();
    }

    /// 通过鼠标按下事件确认删除连接。
    ///
    /// 业务意图：
    /// - 删除确认属于弹窗按钮行为，沿用 `mouse_down` 可避免透明弹层、拖拽状态或焦点切换影响确认按钮。
    pub(in crate::app) fn confirm_delete_connection_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.confirm_delete_connection(&ClickEvent::default(), window, context);
    }

    /// 重置当前编辑连接的可信主机指纹。
    pub(in crate::app) fn reset_connection_host_key_from_dialog(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(path) = self.connections.database_path.clone() else {
            self.set_connection_dialog_error("无法定位应用配置目录，不能重置信任指纹".to_string());
            context.stop_propagation();
            context.notify();
            return;
        };
        let Some(ConnectionDialogState {
            mode: ConnectionDialogMode::Edit { profile_id },
            ..
        }) = self.connections.dialog.as_ref()
        else {
            return;
        };
        let profile_id = profile_id.clone();
        match update_connection_host_key_fingerprint(&path, &profile_id, None) {
            Ok(()) => {
                if let Some(profile) = self
                    .connections
                    .profiles
                    .iter_mut()
                    .find(|profile| profile.id == profile_id)
                {
                    profile.host_key_fingerprint = None;
                }
                self.connections.status_message = Some("已重置该连接的可信主机指纹".to_string());
            }
            Err(error) => self.set_connection_dialog_error(error),
        }
        context.stop_propagation();
        context.notify();
    }

    /// 通过鼠标按下事件重置当前编辑连接的可信主机指纹。
    pub(in crate::app) fn reset_connection_host_key_from_dialog_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.reset_connection_host_key_from_dialog(&ClickEvent::default(), window, context);
    }

    /// 选择左侧连接。
    pub(in crate::app) fn select_connection_profile(
        &mut self,
        profile_id: &str,
        context: &mut Context<Self>,
    ) {
        self.connections.selected_profile_id = Some(profile_id.to_string());
        context.notify();
    }

    /// 打开一个新的 SSH 终端 tab。
    pub(in crate::app) fn open_connection_terminal(
        &mut self,
        profile_id: &str,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(profile) = self.connections.profile_by_id(profile_id).cloned() else {
            self.connections.status_message = Some("连接不存在，无法打开终端".to_string());
            context.notify();
            return;
        };
        let password = match decrypt_connection_password(&profile.id, &profile.encrypted_password) {
            Ok(password) => password,
            Err(error) => {
                self.connections.status_message = Some(error);
                context.notify();
                return;
            }
        };

        let size = TerminalSize::default();
        let backend = SshTerminalBackend::new(
            profile.clone(),
            password,
            profile.host_key_fingerprint.clone(),
            size,
        )
        .start();
        let tab_id = self.connections.next_tab_id;
        self.connections.next_tab_id = self.connections.next_tab_id.saturating_add(1);
        let focus = context.focus_handle();
        let tab = ConnectionTerminalTab {
            id: tab_id,
            profile_id: profile.id.clone(),
            title: profile.name.clone(),
            backend,
            emulator: ConnectionTerminalEmulator::new(size),
            status: ConnectionTerminalStatus::Connecting,
            focus,
            selection_anchor: None,
            mouse_reporting_drag: false,
            backend_finished: false,
        };
        self.connections.tabs.push(tab);
        self.connections.active_tab_id = Some(tab_id);
        if let Some(active) = self.connections.active_tab() {
            window.focus(&active.focus);
        }
        self.schedule_connections_terminal_poll(context);
        context.notify();
    }

    /// 打开一个新的本地终端 tab。
    ///
    /// 业务意图：
    /// - 本地终端不需要保存配置，也不需要 SSH 表单；用户从新增菜单选择后应立即获得一个独立 shell。
    /// - 仍复用 `ConnectionTerminalTab` 和统一 `TerminalBackendHandle`，让输入、resize、关闭和渲染逻辑与 SSH tab 完全一致。
    ///
    /// 跨平台约束：
    /// - 实际 shell 选择由 `LocalPtyBackend` 内部处理，macOS 走环境中的默认 shell，Windows 走 PowerShell。
    pub(in crate::app) fn open_local_terminal(
        &mut self,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let size = TerminalSize::default();
        let tab_id = self.connections.next_tab_id;
        self.connections.next_tab_id = self.connections.next_tab_id.saturating_add(1);
        let backend = LocalPtyBackend::new(size).start();
        let focus = context.focus_handle();
        let tab = ConnectionTerminalTab {
            id: tab_id,
            profile_id: format!("local-terminal-{tab_id}"),
            title: "本地终端".to_string(),
            backend,
            emulator: ConnectionTerminalEmulator::new(size),
            status: ConnectionTerminalStatus::Connecting,
            focus,
            selection_anchor: None,
            mouse_reporting_drag: false,
            backend_finished: false,
        };

        self.connections.create_menu_open = false;
        self.connections.tabs.push(tab);
        self.connections.active_tab_id = Some(tab_id);
        if let Some(active) = self.connections.active_tab() {
            window.focus(&active.focus);
        }
        self.schedule_connections_terminal_poll(context);
        context.notify();
    }

    /// 关闭指定终端 tab。
    pub(in crate::app) fn close_connection_terminal_tab(
        &mut self,
        tab_id: usize,
        context: &mut Context<Self>,
    ) {
        self.connections.close_tab(tab_id);
        context.notify();
    }

    /// 激活指定终端 tab。
    pub(in crate::app) fn activate_connection_terminal_tab(
        &mut self,
        tab_id: usize,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.connections.active_tab_id = Some(tab_id);
        if let Some(tab) = self.connections.active_tab() {
            window.focus(&tab.focus);
        }
        context.notify();
    }

    /// 根据终端内容区实际 bounds 同步终端尺寸。
    ///
    /// 业务意图：
    /// - 新建 tab 时只能先用默认 80x24，实际内容区大小要等 GPUI 布局完成后才能得到。
    /// - 只有行列或像素尺寸变化时才发送 resize，避免每帧绘制都打扰远端 shell 或触发无意义重绘。
    pub(in crate::app) fn sync_connection_terminal_size_from_bounds(
        &mut self,
        tab_id: usize,
        bounds: Bounds<Pixels>,
        context: &mut Context<Self>,
    ) {
        let next_size = connection_terminal_size_from_bounds(bounds);
        let Some(tab) = self
            .connections
            .tabs
            .iter_mut()
            .find(|tab| tab.id == tab_id)
        else {
            return;
        };
        if tab.emulator.size == next_size {
            return;
        }

        tab.resize(next_size);
        context.notify();
    }

    /// 安排终端事件轮询。
    pub(in crate::app) fn schedule_connections_terminal_poll(
        &mut self,
        context: &mut Context<Self>,
    ) {
        if self.connections.terminal_poll_scheduled {
            return;
        }
        self.connections.terminal_poll_scheduled = true;
        context
            .spawn(async move |view, app| {
                loop {
                    app.background_executor()
                        .timer(Duration::from_millis(CONNECTIONS_TERMINAL_POLL_MILLIS))
                        .await;
                    let keep_polling = view
                        .update(app, |view, context| {
                            view.drain_connections_terminal_events(context);
                            view.connections.terminal_poll_scheduled
                        })
                        .unwrap_or(false);
                    if !keep_polling {
                        break;
                    }
                }
            })
            .detach();
    }

    /// 轮询所有终端 tab 的后台事件。
    pub(in crate::app) fn drain_connections_terminal_events(
        &mut self,
        context: &mut Context<Self>,
    ) {
        let mut received_event = false;
        let database_path = self.connections.database_path.clone();
        let mut connected_profile_ids = Vec::new();
        let mut pending_host_key_dialog = None;

        for tab in &mut self.connections.tabs {
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
                        connected_profile_ids.push(tab.profile_id.clone());
                    }
                    TerminalBackendEvent::Output(bytes) => {
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
                    TerminalBackendEvent::HostKeyUnknown { fingerprint } => {
                        tab.status = ConnectionTerminalStatus::WaitingHostKey;
                        tab.backend_finished = true;
                        pending_host_key_dialog = Some(ConnectionHostKeyDialog {
                            profile_id: tab.profile_id.clone(),
                            tab_id: tab.id,
                            fingerprint,
                            expected: None,
                        });
                        break;
                    }
                    TerminalBackendEvent::HostKeyMismatch { expected, actual } => {
                        tab.status = ConnectionTerminalStatus::Error(
                            "SSH 主机指纹与已信任指纹不一致，连接已阻止".to_string(),
                        );
                        tab.backend_finished = true;
                        pending_host_key_dialog = Some(ConnectionHostKeyDialog {
                            profile_id: tab.profile_id.clone(),
                            tab_id: tab.id,
                            fingerprint: actual,
                            expected: Some(expected),
                        });
                        break;
                    }
                }
            }
        }

        if let Some(dialog) = pending_host_key_dialog {
            self.connections.host_key_dialog = Some(dialog);
        }

        if let Some(path) = database_path {
            for profile_id in connected_profile_ids {
                let connected_at_ms = current_connection_time_millis();
                let _ = update_connection_last_connected_at(&path, &profile_id, connected_at_ms);
                if let Some(profile) = self
                    .connections
                    .profiles
                    .iter_mut()
                    .find(|profile| profile.id == profile_id)
                {
                    profile.last_connected_at_ms = Some(connected_at_ms);
                }
            }
        }

        if self.connections.tabs.iter().all(|tab| tab.backend_finished) {
            self.connections.terminal_poll_scheduled = false;
        }
        if received_event {
            context.notify();
        }
    }

    /// 确认首次信任 SSH 主机指纹。
    pub(in crate::app) fn confirm_connection_host_key(
        &mut self,
        _event: &ClickEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(dialog) = self.connections.host_key_dialog.take() else {
            return;
        };
        if dialog.expected.is_some() {
            self.connections.status_message = Some(
                "主机指纹不一致时不能直接继续，请在确认服务器可信后先重置信任指纹".to_string(),
            );
            context.stop_propagation();
            context.notify();
            return;
        }
        let Some(path) = self.connections.database_path.clone() else {
            self.connections.status_message =
                Some("无法定位应用配置目录，不能保存主机指纹".to_string());
            context.stop_propagation();
            context.notify();
            return;
        };
        match update_connection_host_key_fingerprint(
            &path,
            &dialog.profile_id,
            Some(&dialog.fingerprint),
        ) {
            Ok(()) => {
                if let Some(profile) = self
                    .connections
                    .profiles
                    .iter_mut()
                    .find(|profile| profile.id == dialog.profile_id)
                {
                    profile.host_key_fingerprint = Some(dialog.fingerprint);
                }
                self.connections.close_tab(dialog.tab_id);
                self.open_connection_terminal(&dialog.profile_id, window, context);
            }
            Err(error) => {
                self.connections.status_message = Some(error);
                context.notify();
            }
        }
        context.stop_propagation();
    }

    /// 通过鼠标按下事件确认首次信任 SSH 主机指纹。
    ///
    /// 业务意图：
    /// - 主机指纹弹窗同样位于连接页终端区域上方，确认按钮必须和其它弹窗按钮一样立即消费鼠标按下事件，
    ///   防止点击继续透传到底层终端或无法合成 `click`。
    pub(in crate::app) fn confirm_connection_host_key_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.confirm_connection_host_key(&ClickEvent::default(), window, context);
    }

    /// 通过鼠标按下事件关闭 SSH 主机指纹弹窗。
    pub(in crate::app) fn close_connection_host_key_dialog_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.connections.host_key_dialog = None;
        context.stop_propagation();
        context.notify();
    }

    /// 判断连接表单当前聚焦字段。
    pub(in crate::app) fn active_connection_form_field(
        &self,
        window: &Window,
    ) -> Option<ConnectionFormField> {
        let dialog = self.connections.dialog.as_ref()?;
        if dialog.name.focus.is_focused(window) {
            Some(ConnectionFormField::Name)
        } else if dialog.host.focus.is_focused(window) {
            Some(ConnectionFormField::Host)
        } else if dialog.port.focus.is_focused(window) {
            Some(ConnectionFormField::Port)
        } else if dialog.username.focus.is_focused(window) {
            Some(ConnectionFormField::Username)
        } else if dialog.password.focus.is_focused(window) {
            Some(ConnectionFormField::Password)
        } else {
            None
        }
    }

    /// 判断连接页是否有文本输入框聚焦。
    pub(in crate::app) fn connection_text_input_focused(&self, window: &Window) -> bool {
        self.active_connection_form_field(window).is_some()
    }

    /// 判断连接页当前是否存在需要阻断底层终端输入的模态弹窗。
    ///
    /// 业务意图：
    /// - 新增/编辑、删除确认和主机指纹确认都属于连接页内模态交互；弹窗打开时，底层终端不能继续接收键盘或粘贴。
    /// - 该判断不包含左侧新增类型菜单，因为它不是模态弹窗，只负责自身区域的鼠标消费。
    pub(in crate::app) fn connection_modal_open(&self) -> bool {
        self.connections.dialog.is_some()
            || self.connections.delete_confirm_dialog.is_some()
            || self.connections.host_key_dialog.is_some()
    }

    /// 判断当前激活终端是否真正持有焦点。
    ///
    /// 业务意图：
    /// - 全局复制/粘贴快捷键只有在用户正在操作终端时才应转发到 PTY/SSH 后端。
    /// - 弹窗打开时即使焦点仍停留在旧终端 tab，也必须阻断，避免剪贴板内容被写入背后的 shell。
    pub(in crate::app) fn connection_terminal_focused_without_modal(
        &self,
        window: &Window,
    ) -> bool {
        !self.connection_modal_open()
            && self
                .connections
                .active_tab()
                .is_some_and(|tab| tab.focus.is_focused(window))
    }

    /// 返回连接表单字段的绘制快照。
    ///
    /// 业务意图：
    /// - 通用 `TextInputElement` 只认识绑定，不直接借用连接弹窗状态；这里把连接表单字段转换成组件通用快照。
    pub(in crate::app) fn connection_form_text_snapshot(
        &self,
        field: ConnectionFormField,
    ) -> Option<SingleLineTextInputSnapshot> {
        let state = self.connections.dialog.as_ref()?.field(field);
        Some(SingleLineTextInputSnapshot {
            text: state.input.text.clone(),
            selection_range: state.input.selection_range.clone(),
            marked_range: state.input.marked_range.clone(),
            horizontal_scroll_px: state.input.horizontal_scroll_px,
        })
    }

    /// 保存连接表单字段最近一次文本布局。
    pub(in crate::app) fn store_connection_form_text_layout(
        &mut self,
        field: ConnectionFormField,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
        horizontal_scroll_px: f32,
    ) {
        if let Some(dialog) = self.connections.dialog.as_mut() {
            dialog
                .field_mut(field)
                .store_layout(line, bounds, horizontal_scroll_px);
        }
    }

    /// 根据鼠标窗口坐标返回连接表单字段中的 UTF-8 字节下标。
    pub(in crate::app) fn connection_form_text_index_for_point(
        &self,
        field: ConnectionFormField,
        position: gpui::Point<Pixels>,
    ) -> usize {
        let Some(dialog) = self.connections.dialog.as_ref() else {
            return 0;
        };
        let state = dialog.field(field);
        let text = &state.input.text;
        let (Some(layout), Some(bounds)) = (state.last_layout.as_ref(), state.last_bounds.as_ref())
        else {
            return text.len();
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return text.len();
        }
        let display_index = layout
            .closest_index_for_x(position.x - bounds.left() + px(state.input.horizontal_scroll_px));
        if field == ConnectionFormField::Password {
            TextInputDisplayMode::Masked { mask_char: '*' }
                .text_index_for_display_index(text, display_index)
        } else {
            text_input_clamp_byte_index(text, display_index.min(text.len()))
        }
    }

    /// 处理连接表单按键。
    pub(in crate::app) fn handle_connection_form_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(field) = self.active_connection_form_field(window) else {
            return;
        };

        if event.keystroke.key == "escape" {
            self.connections.dialog = None;
            context.stop_propagation();
            context.notify();
            return;
        }
        if event.keystroke.key == "enter" {
            self.save_connection_dialog(&ClickEvent::default(), window, context);
            return;
        }

        let Some(dialog) = self.connections.dialog.as_mut() else {
            return;
        };
        let input = &mut dialog.field_mut(field).input;

        if Self::is_paste_keystroke(&event.keystroke) {
            if let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) {
                replace_text_input_selection(input, &sanitize_text_input_single_line_text(&text));
                dialog.error = None;
            }
            context.stop_propagation();
            context.notify();
            return;
        }
        if Self::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = text_input_selected_text(input) {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return;
        }
        if Self::is_cut_keystroke(&event.keystroke) {
            if let Some(text) = text_input_selected_text(input) {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                replace_text_input_selection(input, "");
                dialog.error = None;
            }
            context.stop_propagation();
            context.notify();
            return;
        }
        if Self::is_select_all_keystroke(&event.keystroke) {
            select_all_text_input(input);
            context.stop_propagation();
            context.notify();
            return;
        }

        let outcome = match event.keystroke.key.as_str() {
            "left" => move_text_input_left(input, event.keystroke.modifiers.shift),
            "right" => move_text_input_right(input, event.keystroke.modifiers.shift),
            "home" | "up" => move_text_input_home(input, event.keystroke.modifiers.shift),
            "end" | "down" => move_text_input_end(input, event.keystroke.modifiers.shift),
            "backspace" => backspace_text_input(input),
            "delete" => delete_text_input(input),
            _ => TextInputEditOutcome::default(),
        };
        if outcome.consumed {
            if outcome.changed {
                dialog.error = None;
            }
            context.stop_propagation();
            context.notify();
        }
    }

    /// 处理终端键盘输入。
    pub(in crate::app) fn handle_connection_terminal_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if !self.connection_terminal_focused_without_modal(window) {
            context.stop_propagation();
            return;
        }
        let Some(tab) = self.connections.active_tab_mut() else {
            return;
        };
        let Some(bytes) = terminal_input_bytes_for_keystroke(&event.keystroke) else {
            return;
        };
        tab.write_input(bytes);
        context.stop_propagation();
        context.notify();
    }

    /// 复制当前终端选区；没有选区时返回 false，让 Ctrl+C 继续作为终端中断发送。
    pub(in crate::app) fn copy_selected_connection_terminal_text(
        &mut self,
        context: &mut Context<Self>,
    ) -> bool {
        let Some(tab) = self.connections.active_tab_mut() else {
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

    /// 粘贴剪贴板文本到当前终端。
    pub(in crate::app) fn paste_clipboard_text_into_connection_terminal(
        &mut self,
        context: &mut Context<Self>,
    ) -> bool {
        let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) else {
            return false;
        };
        let Some(tab) = self.connections.active_tab_mut() else {
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

    /// 根据终端面板鼠标位置开始选区。
    pub(in crate::app) fn begin_connection_terminal_selection(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let Some(point) = self.active_connection_terminal_point_from_window_position(
            f32::from(event.position.x),
            f32::from(event.position.y),
        ) else {
            return;
        };
        let Some(tab) = self.connections.active_tab_mut() else {
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

    /// 拖拽更新终端选区。
    pub(in crate::app) fn update_connection_terminal_selection(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        let Some(point) = self.active_connection_terminal_point_from_window_position(
            f32::from(event.position.x),
            f32::from(event.position.y),
        ) else {
            return;
        };
        let Some(tab) = self.connections.active_tab_mut() else {
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

    /// 结束终端选区拖拽。
    pub(in crate::app) fn end_connection_terminal_selection(
        &mut self,
        _event: &MouseUpEvent,
        context: &mut Context<Self>,
    ) -> bool {
        let release_point = self.active_connection_terminal_point_from_window_position(
            f32::from(_event.position.x),
            f32::from(_event.position.y),
        );
        let mut consumed = false;
        if let Some(tab) = self.connections.active_tab_mut() {
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

    /// 结束连接页左侧栏宽度拖拽。
    ///
    /// 返回值用于决定是否消费鼠标释放事件，避免普通点击冒泡时阻断主导航按钮。
    fn end_connections_tree_resize(&mut self, context: &mut Context<Self>) -> bool {
        if self.connections.tree_resize_drag.take().is_some() {
            context.stop_propagation();
            context.notify();
            return true;
        }
        false
    }

    /// 结束连接页内正在进行的拖拽类交互。
    ///
    /// 业务意图：
    /// - 连接页需要在鼠标释放时清理左侧栏 resize、终端选区和 xterm 鼠标拖拽。
    /// - 只有真的存在拖拽状态时才消费事件，避免连接页透明命中区域影响左侧主导航点击。
    fn finish_connections_pointer_interaction(
        &mut self,
        event: &MouseUpEvent,
        context: &mut Context<Self>,
    ) -> bool {
        let consumed_resize = self.end_connections_tree_resize(context);
        let consumed_terminal = self.end_connection_terminal_selection(event, context);
        consumed_resize || consumed_terminal
    }

    /// 将窗口坐标换算为当前终端的 alacritty 格点。
    ///
    /// 边界条件：
    /// - GPUI 鼠标事件是窗口坐标，连接页右侧终端还要扣除主导航、连接列表、tab 栏和状态栏。
    /// - 鼠标拖到终端外时交给 `point_from_panel_offset` 夹紧，避免选区或鼠标上报生成越界坐标。
    fn active_connection_terminal_point_from_window_position(
        &self,
        x: f32,
        y: f32,
    ) -> Option<Point> {
        let tab = self.connections.active_tab()?;
        let local_x = x - MAIN_NAV_WIDTH - self.connections.tree_width;
        let local_y = y - CONNECTIONS_TAB_BAR_HEIGHT - CONNECTIONS_TERMINAL_STATUS_BAR_HEIGHT;
        Some(tab.emulator.point_from_panel_offset(local_x, local_y))
    }

    /// 开始调整连接页左侧栏宽度。
    pub(in crate::app) fn start_connections_tree_resize(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.connections.tree_resize_drag = Some(ConnectionsTreeResizeDrag {
            start_x: f32::from(event.position.x),
            start_width: self.connections.tree_width,
        });
        context.stop_propagation();
        context.notify();
    }

    /// 处理连接页鼠标移动。
    pub(in crate::app) fn handle_connections_page_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if let Some(drag) = self.connections.tree_resize_drag.as_ref() {
            let delta = f32::from(event.position.x) - drag.start_x;
            self.connections.tree_width = (drag.start_width + delta)
                .clamp(CONNECTIONS_TREE_MIN_WIDTH, CONNECTIONS_TREE_MAX_WIDTH);
            context.stop_propagation();
            context.notify();
            return;
        }
        self.update_connection_terminal_selection(event, context);
    }

    /// 处理连接页鼠标释放。
    pub(in crate::app) fn handle_connections_page_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.finish_connections_pointer_interaction(event, context);
    }
}

/// 将 GPUI 按键映射为终端输入字节。
///
/// 边界条件：
/// - `Ctrl+C` 在没有选区时由这里映射为 ETX，中断远端程序；存在选区时全局快捷键会先复制并消费。
fn terminal_input_bytes_for_keystroke(keystroke: &Keystroke) -> Option<Vec<u8>> {
    if keystroke.modifiers.control && keystroke.key.len() == 1 {
        let byte = keystroke.key.as_bytes()[0].to_ascii_lowercase();
        if byte.is_ascii_lowercase() {
            return Some(vec![byte - b'a' + 1]);
        }
    }

    let bytes = match keystroke.key.as_str() {
        "enter" => b"\r".to_vec(),
        "backspace" => vec![0x7f],
        "tab" => b"\t".to_vec(),
        "escape" => vec![0x1b],
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        _ => {
            if keystroke.modifiers.platform || keystroke.modifiers.control {
                return None;
            }
            keystroke.key_char.as_ref()?.as_bytes().to_vec()
        }
    };
    Some(bytes)
}

/// 生成 xterm SGR 鼠标上报序列。
///
/// 协议约束：
/// - alacritty 的格点是 0 基坐标，xterm 鼠标协议使用 1 基坐标，因此发送前必须同时加一。
/// - 第一版只上报左键按下、左键拖拽和释放；滚轮和其它按钮后续可以复用该函数扩展。
fn terminal_sgr_mouse_report(button_code: u8, point: Point, pressed: bool) -> Vec<u8> {
    let suffix = if pressed { 'M' } else { 'm' };
    format!(
        "\x1b[<{};{};{}{}",
        button_code,
        point.column.0 + 1,
        point.line.0 + 1,
        suffix
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::index::{Column, Line, Point};

    use super::*;

    /// 验证终端鼠标上报坐标遵循 xterm SGR 的 1 基坐标规则。
    ///
    /// 业务风险：
    /// - 坐标如果仍按 alacritty 内部 0 基格式发送，vim、less 等远端 TUI 会把点击位置错一行一列。
    #[test]
    fn 终端鼠标上报使用_sgr_一基坐标() {
        let bytes = terminal_sgr_mouse_report(0, Point::new(Line(2), Column(4)), true);
        assert_eq!(
            String::from_utf8(bytes).expect("测试鼠标序列应为 UTF-8"),
            "\x1b[<0;5;3M"
        );
    }
}
