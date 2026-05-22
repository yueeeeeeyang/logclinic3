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
        self.connections.profile_context_menu = None;
        context.stop_propagation();
        context.notify();
    }

    /// 关闭左侧连接栏所有浮层菜单。
    ///
    /// 业务意图：
    /// - 新建类型菜单和连接行右键菜单共享左侧栏浮层层级；任一菜单外部点击都应统一关闭，避免两个菜单同时残留。
    pub(in crate::app) fn close_connection_tree_menus(&mut self, context: &mut Context<Self>) {
        if self.connections.create_menu_open
            || self.connections.profile_context_menu.is_some()
            || self.connections.category_context_menu.is_some()
        {
            self.connections.create_menu_open = false;
            self.connections.profile_context_menu = None;
            self.connections.category_context_menu = None;
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
        self.connections.profile_context_menu = None;
        self.connections.category_context_menu = None;
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
                self.connections.smb_dialog = None;
            }
            ConnectionCreateKind::Smb => {
                let dialog = SmbConnectionDialogState::create(context);
                window.focus(&dialog.name.focus);
                self.connections.create_menu_open = false;
                self.connections.smb_dialog = Some(dialog);
                self.connections.dialog = None;
            }
            ConnectionCreateKind::LocalTerminal => {
                self.open_local_terminal(window, context);
            }
            ConnectionCreateKind::Category => {
                self.open_connection_category_dialog(None, window, context);
            }
        }
    }

    /// 打开编辑连接弹窗。
    pub(in crate::app) fn open_edit_connection_dialog(
        &mut self,
        profile_key: &str,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        match ConnectionProfileKey::parse(profile_key) {
            Some((ConnectionProfileKind::Ssh, profile_id)) => {
                let Some(profile) = self.connections.profile_by_id(profile_id).cloned() else {
                    self.connections.status_message = Some("连接不存在，无法编辑".to_string());
                    context.notify();
                    return;
                };
                let dialog = ConnectionDialogState::edit(&profile, context);
                window.focus(&dialog.name.focus);
                self.connections.dialog = Some(dialog);
                self.connections.smb_dialog = None;
            }
            Some((ConnectionProfileKind::Smb, profile_id)) => {
                let Some(profile) = self.connections.smb_profile_by_id(profile_id).cloned() else {
                    self.connections.status_message = Some("连接不存在，无法编辑".to_string());
                    context.notify();
                    return;
                };
                let dialog = SmbConnectionDialogState::edit(&profile, context);
                window.focus(&dialog.name.focus);
                self.connections.smb_dialog = Some(dialog);
                self.connections.dialog = None;
            }
            None => {
                self.connections.status_message = Some("连接不存在，无法编辑".to_string());
                context.notify();
                return;
            }
        }
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.category_context_menu = None;
        context.notify();
    }

    /// 打开新增或编辑连接分类弹窗。
    ///
    /// 业务意图：
    /// - 顶部新增菜单创建根分类，分类右键菜单创建子分类或重命名；三种入口统一收敛到一个弹窗状态。
    pub(in crate::app) fn open_connection_category_dialog(
        &mut self,
        parent_id: Option<String>,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if let Some(parent_id) = parent_id.as_deref()
            && self.connections.category_by_id(parent_id).is_none()
        {
            self.connections.status_message = Some("分类不存在，无法新建子分类".to_string());
            context.notify();
            return;
        }
        let dialog = ConnectionCategoryDialogState::create(parent_id, context);
        window.focus(&dialog.name.focus);
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.category_context_menu = None;
        self.connections.category_dialog = Some(dialog);
        context.notify();
    }

    /// 打开编辑连接分类弹窗。
    pub(in crate::app) fn open_edit_connection_category_dialog(
        &mut self,
        category_id: &str,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(category) = self.connections.category_by_id(category_id).cloned() else {
            self.connections.status_message = Some("分类不存在，无法编辑".to_string());
            context.notify();
            return;
        };
        let dialog = ConnectionCategoryDialogState::edit(&category, context);
        window.focus(&dialog.name.focus);
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.category_context_menu = None;
        self.connections.category_dialog = Some(dialog);
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
        self.connections.smb_dialog = None;
        context.stop_propagation();
        context.notify();
    }

    /// 通过鼠标按下事件关闭分类弹窗。
    pub(in crate::app) fn close_connection_category_dialog_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.connections.category_dialog = None;
        context.stop_propagation();
        context.notify();
    }

    /// 保存新增或编辑分类。
    pub(in crate::app) fn save_connection_category_dialog(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(path) = self.connections.database_path.clone() else {
            self.set_connection_category_dialog_error(
                "无法定位应用配置目录，不能保存分类".to_string(),
            );
            context.stop_propagation();
            context.notify();
            return;
        };
        let Some(dialog) = self.connections.category_dialog.as_ref() else {
            return;
        };
        let draft = dialog.to_category_draft();
        let name = match validate_connection_category_draft(&draft) {
            Ok(name) => name,
            Err(error) => {
                self.set_connection_category_dialog_error(error);
                context.stop_propagation();
                context.notify();
                return;
            }
        };

        let save_result = match dialog.mode.clone() {
            ConnectionCategoryDialogMode::Create { parent_id } => {
                if let Some(parent_id) = parent_id.as_deref()
                    && self.connections.category_by_id(parent_id).is_none()
                {
                    self.set_connection_category_dialog_error("父分类不存在，无法保存".to_string());
                    context.stop_propagation();
                    context.notify();
                    return;
                }
                let now = current_connection_time_millis();
                let category = ConnectionCategory {
                    id: new_connection_entity_id("cat"),
                    parent_id: parent_id.clone(),
                    name,
                    created_at_ms: now,
                    updated_at_ms: now,
                };
                if let Some(parent_id) = parent_id {
                    self.connections.expanded_category_ids.insert(parent_id);
                }
                insert_connection_category(&path, &category)
            }
            ConnectionCategoryDialogMode::Edit { category_id } => {
                let Some(existing) = self.connections.category_by_id(&category_id).cloned() else {
                    self.set_connection_category_dialog_error("分类不存在，无法保存".to_string());
                    context.stop_propagation();
                    context.notify();
                    return;
                };
                let category = ConnectionCategory {
                    id: existing.id,
                    parent_id: existing.parent_id,
                    name,
                    created_at_ms: existing.created_at_ms,
                    updated_at_ms: current_connection_time_millis(),
                };
                update_connection_category(&path, &category)
            }
        };

        match save_result {
            Ok(()) => {
                self.connections.category_dialog = None;
                self.connections.reload_tree_data();
                self.connections.status_message = Some("分类已保存".to_string());
            }
            Err(error) => self.set_connection_category_dialog_error(error),
        }
        context.stop_propagation();
        context.notify();
    }

    /// 通过鼠标按下事件保存分类弹窗。
    pub(in crate::app) fn save_connection_category_dialog_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.save_connection_category_dialog(&ClickEvent::default(), window, context);
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
        self.close_connection_dialog_category_select();
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
        if let Some(category_id) = draft.category_id.as_deref()
            && self.connections.category_by_id(category_id).is_none()
        {
            self.set_connection_dialog_error("所选分类不存在，请重新选择".to_string());
            context.stop_propagation();
            context.notify();
            return;
        }

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
                    category_id: draft.category_id.clone(),
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
                    category_id: draft.category_id.clone(),
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

    /// 切换连接表单中的分类选择器。
    pub(in crate::app) fn toggle_connection_dialog_category_select(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.connections.dialog.as_mut() {
            dialog.category_select_open = !dialog.category_select_open;
        }
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.category_context_menu = None;
        context.stop_propagation();
        context.notify();
    }

    /// 选择连接表单中的分类。
    pub(in crate::app) fn select_connection_dialog_category(
        &mut self,
        category_id: Option<String>,
        _event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.connections.dialog.as_mut() {
            dialog.category_id = category_id;
            dialog.category_select_open = false;
            dialog.error = None;
        }
        context.stop_propagation();
        context.notify();
    }

    /// 切换 SMB 连接表单中的分类选择器。
    pub(in crate::app) fn toggle_smb_connection_dialog_category_select(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.connections.smb_dialog.as_mut() {
            dialog.category_select_open = !dialog.category_select_open;
        }
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.category_context_menu = None;
        context.stop_propagation();
        context.notify();
    }

    /// 选择 SMB 连接表单中的分类。
    pub(in crate::app) fn select_smb_connection_dialog_category(
        &mut self,
        category_id: Option<String>,
        _event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.connections.smb_dialog.as_mut() {
            dialog.category_id = category_id;
            dialog.category_select_open = false;
            dialog.error = None;
        }
        context.stop_propagation();
        context.notify();
    }

    /// 通过鼠标按下事件关闭连接表单中的分类选择器。
    ///
    /// 业务意图：
    /// - 分类 Select 是连接弹窗内部的浮层；用户点击表单空白区域时应立即收起，避免菜单继续覆盖密码框、
    ///   错误提示或底部按钮。
    /// - 该动作只收起 Select，不关闭整个连接弹窗，保持和普通表单控件失焦一致的交互语义。
    pub(in crate::app) fn close_connection_dialog_category_select_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.close_connection_dialog_category_select();
        context.stop_propagation();
        context.notify();
    }

    /// 通过鼠标按下事件关闭 SMB 连接表单中的分类选择器。
    pub(in crate::app) fn close_smb_connection_dialog_category_select_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.close_smb_connection_dialog_category_select();
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
        self.close_connection_dialog_category_select();
        self.save_connection_dialog(&ClickEvent::default(), window, context);
    }

    /// 保存新增或编辑 SMB 连接。
    pub(in crate::app) fn save_smb_connection_dialog(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(path) = self.connections.database_path.clone() else {
            self.set_smb_connection_dialog_error("无法定位应用配置目录，不能保存连接".to_string());
            context.stop_propagation();
            context.notify();
            return;
        };
        self.close_smb_connection_dialog_category_select();
        let Some(dialog) = self.connections.smb_dialog.as_ref() else {
            return;
        };
        let password_required = matches!(dialog.mode, ConnectionDialogMode::Create);
        let draft = dialog.to_profile_draft();
        let normalized = match validate_smb_connection_profile_draft(&draft, password_required) {
            Ok(normalized) => normalized,
            Err(error) => {
                self.set_smb_connection_dialog_error(error);
                context.stop_propagation();
                context.notify();
                return;
            }
        };
        let (name, parsed, username, password) = normalized;
        if let Some(category_id) = draft.category_id.as_deref()
            && self.connections.category_by_id(category_id).is_none()
        {
            self.set_smb_connection_dialog_error("所选分类不存在，请重新选择".to_string());
            context.stop_propagation();
            context.notify();
            return;
        }

        let save_result = match dialog.mode.clone() {
            ConnectionDialogMode::Create => {
                let id = new_connection_entity_id("smb");
                let Some(password) = password else {
                    self.set_smb_connection_dialog_error("密码不能为空".to_string());
                    context.stop_propagation();
                    context.notify();
                    return;
                };
                let encrypted_password = match encrypt_connection_password(&id, password.as_str()) {
                    Ok(encrypted_password) => encrypted_password,
                    Err(error) => {
                        self.set_smb_connection_dialog_error(error);
                        context.stop_propagation();
                        context.notify();
                        return;
                    }
                };
                let now = current_connection_time_millis();
                let profile = SmbConnectionProfile {
                    id,
                    name,
                    host: parsed.host,
                    port: parsed.port,
                    share: parsed.share,
                    initial_path: parsed.initial_path,
                    username,
                    category_id: draft.category_id.clone(),
                    encrypted_password,
                    created_at_ms: now,
                    updated_at_ms: now,
                    last_connected_at_ms: None,
                };
                insert_smb_connection_profile(&path, &profile)
            }
            ConnectionDialogMode::Edit { profile_id } => {
                let Some(existing) = self.connections.smb_profile_by_id(&profile_id).cloned()
                else {
                    self.set_smb_connection_dialog_error("连接不存在，无法保存".to_string());
                    context.stop_propagation();
                    context.notify();
                    return;
                };
                let encrypted_password = match password {
                    Some(password) => {
                        match encrypt_connection_password(&existing.id, password.as_str()) {
                            Ok(encrypted_password) => encrypted_password,
                            Err(error) => {
                                self.set_smb_connection_dialog_error(error);
                                context.stop_propagation();
                                context.notify();
                                return;
                            }
                        }
                    }
                    None => existing.encrypted_password.clone(),
                };
                let profile = SmbConnectionProfile {
                    id: existing.id,
                    name,
                    host: parsed.host,
                    port: parsed.port,
                    share: parsed.share,
                    initial_path: parsed.initial_path,
                    username,
                    category_id: draft.category_id.clone(),
                    encrypted_password,
                    created_at_ms: existing.created_at_ms,
                    updated_at_ms: current_connection_time_millis(),
                    last_connected_at_ms: existing.last_connected_at_ms,
                };
                update_smb_connection_profile(&path, &profile)
            }
        };

        match save_result {
            Ok(()) => {
                self.connections.smb_dialog = None;
                self.connections.reload_tree_data();
                self.connections.status_message = Some("连接已保存".to_string());
            }
            Err(error) => self.set_smb_connection_dialog_error(error),
        }
        context.stop_propagation();
        context.notify();
    }

    /// 通过鼠标按下事件保存 SMB 连接。
    pub(in crate::app) fn save_smb_connection_dialog_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.close_smb_connection_dialog_category_select();
        self.save_smb_connection_dialog(&ClickEvent::default(), window, context);
    }

    /// 静默收起连接表单分类 Select。
    ///
    /// 业务意图：
    /// - 保存、外部点击和后续可能的快捷键路径都需要复用同一个状态收口，避免多个入口分别遗漏
    ///   `category_select_open`。
    pub(in crate::app) fn close_connection_dialog_category_select(&mut self) {
        if let Some(dialog) = self.connections.dialog.as_mut() {
            dialog.category_select_open = false;
        }
    }

    /// 静默收起 SMB 连接表单分类 Select。
    pub(in crate::app) fn close_smb_connection_dialog_category_select(&mut self) {
        if let Some(dialog) = self.connections.smb_dialog.as_mut() {
            dialog.category_select_open = false;
        }
    }

    /// 在连接弹窗上设置错误消息。
    fn set_connection_dialog_error(&mut self, error: String) {
        if let Some(dialog) = self.connections.dialog.as_mut() {
            dialog.error = Some(error);
        } else {
            self.connections.status_message = Some(error);
        }
    }

    /// 在 SMB 连接弹窗上设置错误消息。
    fn set_smb_connection_dialog_error(&mut self, error: String) {
        if let Some(dialog) = self.connections.smb_dialog.as_mut() {
            dialog.error = Some(error);
        } else {
            self.connections.status_message = Some(error);
        }
    }

    /// 在分类弹窗上设置错误消息。
    fn set_connection_category_dialog_error(&mut self, error: String) {
        if let Some(dialog) = self.connections.category_dialog.as_mut() {
            dialog.error = Some(error);
        } else {
            self.connections.status_message = Some(error);
        }
    }

    /// 打开删除连接确认弹窗。
    pub(in crate::app) fn open_delete_connection_dialog(
        &mut self,
        profile_key: &str,
        context: &mut Context<Self>,
    ) {
        let profile_name = match ConnectionProfileKey::parse(profile_key) {
            Some((ConnectionProfileKind::Ssh, profile_id)) => {
                let Some(profile) = self.connections.profile_by_id(profile_id) else {
                    self.connections.status_message = Some("连接不存在，无法删除".to_string());
                    context.notify();
                    return;
                };
                profile.name.clone()
            }
            Some((ConnectionProfileKind::Smb, profile_id)) => {
                let Some(profile) = self.connections.smb_profile_by_id(profile_id) else {
                    self.connections.status_message = Some("连接不存在，无法删除".to_string());
                    context.notify();
                    return;
                };
                profile.name.clone()
            }
            None => {
                self.connections.status_message = Some("连接不存在，无法删除".to_string());
                context.notify();
                return;
            }
        };
        self.connections.delete_confirm_dialog = Some(ConnectionDeleteConfirmDialog {
            profile_id: profile_key.to_string(),
            profile_name,
        });
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.category_context_menu = None;
        self.connections.terminal_context_menu = None;
        context.notify();
    }

    /// 打开删除分类确认弹窗。
    pub(in crate::app) fn open_delete_connection_category_dialog(
        &mut self,
        category_id: &str,
        context: &mut Context<Self>,
    ) {
        let Some(category) = self.connections.category_by_id(category_id) else {
            self.connections.status_message = Some("分类不存在，无法删除".to_string());
            context.notify();
            return;
        };
        self.connections.category_delete_confirm_dialog =
            Some(ConnectionCategoryDeleteConfirmDialog {
                category_id: category.id.clone(),
                category_name: category.name.clone(),
            });
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.category_context_menu = None;
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

    /// 通过鼠标按下事件关闭删除分类确认弹窗。
    pub(in crate::app) fn close_delete_connection_category_dialog_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.connections.category_delete_confirm_dialog = None;
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

        let delete_result = match ConnectionProfileKey::parse(&dialog.profile_id) {
            Some((ConnectionProfileKind::Ssh, profile_id)) => {
                delete_connection_profile(&path, profile_id).map(|()| {
                    self.connections.close_tabs_for_profile(profile_id);
                })
            }
            Some((ConnectionProfileKind::Smb, profile_id)) => {
                delete_smb_connection_profile(&path, profile_id)
            }
            None => Err("连接不存在，无法删除".to_string()),
        };
        match delete_result {
            Ok(()) => {
                self.connections.reload_tree_data();
                self.connections.status_message = Some("连接已删除".to_string());
            }
            Err(error) => {
                self.connections.status_message = Some(error);
            }
        }
        context.stop_propagation();
        context.notify();
    }

    /// 删除空分类。
    pub(in crate::app) fn confirm_delete_connection_category(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(dialog) = self.connections.category_delete_confirm_dialog.take() else {
            return;
        };
        let Some(path) = self.connections.database_path.clone() else {
            self.connections.status_message =
                Some("无法定位应用配置目录，不能删除分类".to_string());
            context.stop_propagation();
            context.notify();
            return;
        };

        match delete_connection_category(&path, &dialog.category_id) {
            Ok(()) => {
                self.connections
                    .expanded_category_ids
                    .remove(&dialog.category_id);
                self.connections.reload_tree_data();
                self.connections.status_message = Some("分类已删除".to_string());
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

    /// 通过鼠标按下事件确认删除分类。
    pub(in crate::app) fn confirm_delete_connection_category_from_mouse_down(
        &mut self,
        _event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.confirm_delete_connection_category(&ClickEvent::default(), window, context);
    }

    /// 重置当前编辑连接的可信主机指纹。
    pub(in crate::app) fn reset_connection_host_key_from_dialog(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.close_connection_dialog_category_select();
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

    /// 计算连接行右键菜单在左侧栏内的横坐标。
    ///
    /// 业务意图：
    /// - GPUI 鼠标事件给的是窗口坐标，而连接行菜单绘制在左侧连接栏内部，必须扣除固定主导航宽度。
    /// - 横坐标限制在侧栏范围内，避免靠近分割线右键时菜单溢出到终端区域。
    pub(in crate::app) fn connection_profile_context_menu_x(window_x: f32, tree_width: f32) -> f32 {
        (window_x - MAIN_NAV_WIDTH)
            .max(0.0)
            .clamp(0.0, (tree_width - CONNECTIONS_CONTEXT_MENU_WIDTH).max(0.0))
    }

    /// 计算连接行右键菜单在左侧栏内的纵坐标。
    ///
    /// 业务意图：
    /// - 菜单绘制在连接侧栏内部，纵向位置来自窗口鼠标坐标；靠近窗口底部右键时必须向上夹紧，保证“删除”等底部菜单项仍可见可点。
    /// - 菜单高度按当前三项菜单和垂直内边距计算，后续增减菜单项时需要同步该高度规则。
    pub(in crate::app) fn connection_profile_context_menu_y(
        window_y: f32,
        available_height: f32,
    ) -> f32 {
        let menu_height =
            CONNECTIONS_CONTEXT_MENU_ITEM_HEIGHT * 3.0 + CONNECTIONS_CONTEXT_MENU_VERTICAL_PADDING;
        window_y
            .max(0.0)
            .clamp(0.0, (available_height - menu_height).max(0.0))
    }

    /// 计算连接详情悬浮气泡在左侧栏内的横坐标。
    ///
    /// 业务意图：
    /// - 气泡跟随鼠标出现，但需要保持在连接侧栏内部，避免遮挡右侧终端内容。
    /// - 坐标从窗口坐标转换成侧栏局部坐标，和右键菜单使用同一套主导航宽度扣减规则。
    pub(in crate::app) fn connection_profile_tooltip_x(window_x: f32, tree_width: f32) -> f32 {
        (window_x - MAIN_NAV_WIDTH + CONNECTIONS_PROFILE_TOOLTIP_GAP)
            .max(0.0)
            .clamp(
                0.0,
                (tree_width - CONNECTIONS_PROFILE_TOOLTIP_WIDTH).max(0.0),
            )
    }

    /// 计算连接详情悬浮气泡在左侧栏内的纵坐标。
    ///
    /// 边界条件：
    /// - 靠近窗口底部悬浮时向上夹紧，保证用户和地址两行信息都可见。
    pub(in crate::app) fn connection_profile_tooltip_y(
        window_y: f32,
        available_height: f32,
    ) -> f32 {
        (window_y + CONNECTIONS_PROFILE_TOOLTIP_GAP).max(0.0).clamp(
            0.0,
            (available_height - CONNECTIONS_PROFILE_TOOLTIP_HEIGHT).max(0.0),
        )
    }

    /// 更新左侧连接行 hover 详情气泡。
    ///
    /// 业务意图：
    /// - 连接列表默认只显示名称，鼠标悬浮时再展示用户名和主机地址，降低列表信息密度。
    /// - 离开时只关闭同一个连接打开的气泡，避免快速移动到相邻连接时旧离开事件误关新气泡。
    pub(in crate::app) fn update_connection_profile_hover_tooltip(
        &mut self,
        profile_key: &str,
        is_hovered: bool,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if is_hovered {
            let pointer = window.mouse_position();
            self.connections.profile_hover_tooltip = Some(ConnectionProfileHoverTooltip {
                profile_id: profile_key.to_string(),
                x: Self::connection_profile_tooltip_x(
                    f32::from(pointer.x),
                    self.connections.tree_width,
                ),
                y: Self::connection_profile_tooltip_y(
                    f32::from(pointer.y),
                    f32::from(window.viewport_size().height),
                ),
            });
            context.notify();
            return;
        }

        let should_close = self
            .connections
            .profile_hover_tooltip
            .as_ref()
            .is_some_and(|tooltip| tooltip.profile_id == profile_key);
        if should_close {
            self.connections.profile_hover_tooltip = None;
            context.notify();
        }
    }

    /// 打开左侧连接行右键菜单。
    pub(in crate::app) fn open_connection_profile_context_menu(
        &mut self,
        profile_key: &str,
        event: &MouseDownEvent,
        window: &Window,
        context: &mut Context<Self>,
    ) {
        if !connection_profile_key_exists(
            profile_key,
            &self.connections.profiles,
            &self.connections.smb_profiles,
        ) {
            self.connections.status_message = Some("连接不存在，无法打开菜单".to_string());
            context.stop_propagation();
            context.notify();
            return;
        }
        let target_profile_id = profile_key.to_string();

        self.connections.profile_context_menu = Some(ConnectionProfileContextMenu {
            profile_id: target_profile_id,
            x: Self::connection_profile_context_menu_x(
                f32::from(event.position.x),
                self.connections.tree_width,
            ),
            y: Self::connection_profile_context_menu_y(
                f32::from(event.position.y),
                f32::from(window.viewport_size().height),
            ),
        });
        self.connections.profile_hover_tooltip = None;
        self.connections.create_menu_open = false;
        self.connections.category_context_menu = None;
        context.stop_propagation();
        context.notify();
    }

    /// 打开左侧分类行右键菜单。
    pub(in crate::app) fn open_connection_category_context_menu(
        &mut self,
        category_id: &str,
        event: &MouseDownEvent,
        window: &Window,
        context: &mut Context<Self>,
    ) {
        let Some(category) = self.connections.category_by_id(category_id) else {
            self.connections.status_message = Some("分类不存在，无法打开菜单".to_string());
            context.stop_propagation();
            context.notify();
            return;
        };
        self.connections.category_context_menu = Some(ConnectionCategoryContextMenu {
            category_id: category.id.clone(),
            x: Self::connection_profile_context_menu_x(
                f32::from(event.position.x),
                self.connections.tree_width,
            ),
            y: Self::connection_profile_context_menu_y(
                f32::from(event.position.y),
                f32::from(window.viewport_size().height),
            ),
        });
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.profile_hover_tooltip = None;
        context.stop_propagation();
        context.notify();
    }

    /// 执行左侧连接行右键菜单动作。
    pub(in crate::app) fn handle_connection_profile_context_menu_action(
        &mut self,
        action: ConnectionProfileContextMenuAction,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(menu) = self.connections.profile_context_menu.take() else {
            context.stop_propagation();
            context.notify();
            return;
        };
        let profile_id = menu.profile_id;
        self.connections.create_menu_open = false;
        self.connections.category_context_menu = None;

        match action {
            ConnectionProfileContextMenuAction::Connect => {
                self.open_connection_entry(&profile_id, window, context);
            }
            ConnectionProfileContextMenuAction::Edit => {
                self.open_edit_connection_dialog(&profile_id, window, context);
            }
            ConnectionProfileContextMenuAction::Delete => {
                self.open_delete_connection_dialog(&profile_id, context);
            }
        }
        context.stop_propagation();
        context.notify();
    }

    /// 执行左侧分类行右键菜单动作。
    pub(in crate::app) fn handle_connection_category_context_menu_action(
        &mut self,
        action: ConnectionCategoryContextMenuAction,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(menu) = self.connections.category_context_menu.take() else {
            context.stop_propagation();
            context.notify();
            return;
        };
        let category_id = menu.category_id;
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;

        match action {
            ConnectionCategoryContextMenuAction::CreateChild => {
                self.connections
                    .expanded_category_ids
                    .insert(category_id.clone());
                self.open_connection_category_dialog(Some(category_id), window, context);
            }
            ConnectionCategoryContextMenuAction::Edit => {
                self.open_edit_connection_category_dialog(&category_id, window, context);
            }
            ConnectionCategoryContextMenuAction::Delete => {
                self.open_delete_connection_category_dialog(&category_id, context);
            }
        }
        context.stop_propagation();
        context.notify();
    }

    /// 打开一个新的 SSH 终端 tab。
    pub(in crate::app) fn open_connection_entry(
        &mut self,
        profile_key: &str,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        match ConnectionProfileKey::parse(profile_key) {
            Some((ConnectionProfileKind::Ssh, profile_id)) => {
                self.open_connection_terminal(profile_id, window, context);
            }
            Some((ConnectionProfileKind::Smb, profile_id)) => {
                self.open_smb_file_manager(profile_id, window, context);
            }
            None => {
                self.connections.status_message = Some("连接不存在，无法打开".to_string());
                context.notify();
            }
        }
    }

    /// 打开 SMB 连接对应的文件管理窗口。
    ///
    /// 业务意图：
    /// - SMB 第一版只提供文件管理，不创建终端 tab；左侧点击连接时直接打开独立窗口。
    /// - 文件管理窗口拿到当前连接快照和短生命周期明文密码，后续删除保存的连接不影响已经打开的窗口。
    pub(in crate::app) fn open_smb_file_manager(
        &mut self,
        profile_id: &str,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(profile) = self.connections.smb_profile_by_id(profile_id).cloned() else {
            self.connections.status_message = Some("连接不存在，无法打开文件管理".to_string());
            context.notify();
            return;
        };
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.profile_hover_tooltip = None;
        self.connections.category_context_menu = None;
        self.connections.tab_context_menu = None;
        self.connections.terminal_context_menu = None;
        let password = match decrypt_connection_password(&profile.id, &profile.encrypted_password) {
            Ok(password) => password,
            Err(error) => {
                self.connections.status_message = Some(error);
                context.notify();
                return;
            }
        };
        let title = format!("{} - 文件管理", profile.name);
        let initial_path = profile.initial_path.clone();
        let backend_target = ConnectionFileBackendTarget::Smb {
            profile,
            password: zeroize::Zeroizing::new(password),
        };
        let main_view = context.entity();
        window.defer(context, move |_window, app| {
            let window_options = connection_file_manager_window_options(&title, app);
            let main_view_for_window = main_view.clone();
            let main_view_for_error = main_view.clone();
            let open_result = app.open_window(window_options, move |_window, app| {
                let backend = start_connection_file_backend(backend_target);
                app.new(|context| {
                    ConnectionFileManagerWindowView::new(
                        main_view_for_window,
                        title,
                        backend,
                        initial_path,
                        true,
                        context,
                    )
                })
            });
            if open_result.is_err() {
                let _ = main_view_for_error.update(app, |view, context| {
                    view.connections.status_message = Some("打开文件管理窗口失败".to_string());
                    context.notify();
                });
            }
        });
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
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.profile_hover_tooltip = None;
        self.connections.category_context_menu = None;
        self.connections.tab_context_menu = None;
        self.connections.terminal_context_menu = None;
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
            file_target_kind: ConnectionTerminalFileTargetKind::Ssh,
            title: profile.name.clone(),
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
            file_target_kind: ConnectionTerminalFileTargetKind::Local,
            title: "本地终端".to_string(),
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

        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.profile_hover_tooltip = None;
        self.connections.category_context_menu = None;
        self.connections.tab_context_menu = None;
        self.connections.terminal_context_menu = None;
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

    /// 按给定像素距离横向滚动连接终端 tab 栏。
    pub(in crate::app) fn scroll_connection_tab_bar(
        &mut self,
        delta: f32,
        context: &mut Context<Self>,
    ) {
        let current_offset = self.connections.tab_bar_scroll_handle.offset();
        let max_scroll = self.connections.tab_bar_scroll_handle.max_offset().width;
        let next_x = (current_offset.x - px(delta)).clamp(-max_scroll, px(0.0));
        self.connections
            .tab_bar_scroll_handle
            .set_offset(point(next_x, current_offset.y));
        self.connections.tab_context_menu = None;
        self.connections.terminal_context_menu = None;
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
        self.connections.tab_context_menu = None;
        self.connections.terminal_context_menu = None;
        if let Some(tab) = self.connections.active_tab() {
            window.focus(&tab.focus);
        }
        context.notify();
    }

    /// 打开连接终端 tab 右键菜单。
    pub(in crate::app) fn open_connection_tab_context_menu(
        &mut self,
        tab_id: usize,
        window_x: f32,
        window_y: f32,
        context: &mut Context<Self>,
    ) {
        if !self.connections.tabs.iter().any(|tab| tab.id == tab_id) {
            return;
        }
        let panel_x = (window_x - MAIN_NAV_WIDTH - self.connections.tree_width).max(0.0);
        let panel_y = window_y.max(0.0);
        self.connections.active_tab_id = Some(tab_id);
        self.connections.tab_context_menu = Some(ConnectionTabContextMenu {
            tab_id,
            x: panel_x,
            y: panel_y,
        });
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.category_context_menu = None;
        context.notify();
    }

    /// 执行连接终端 tab 右键菜单动作。
    pub(in crate::app) fn handle_connection_tab_context_menu_action(
        &mut self,
        tab_id: usize,
        action: ConnectionTabContextMenuAction,
        context: &mut Context<Self>,
    ) {
        self.connections.tab_context_menu = None;
        self.connections.terminal_context_menu = None;
        match action {
            ConnectionTabContextMenuAction::Current => self.connections.close_tab(tab_id),
            ConnectionTabContextMenuAction::OtherTabs => self.connections.close_other_tabs(tab_id),
            ConnectionTabContextMenuAction::AllTabs => self.connections.close_all_tabs(),
        }
        context.notify();
    }

    /// 打开终端正文右键菜单。
    pub(in crate::app) fn open_connection_terminal_context_menu(
        &mut self,
        tab_id: usize,
        window_x: f32,
        window_y: f32,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(tab) = self.connections.tabs.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        let panel_x = (window_x - MAIN_NAV_WIDTH - self.connections.tree_width).max(0.0);
        let panel_y = window_y.max(0.0);
        self.connections.active_tab_id = Some(tab_id);
        window.focus(&tab.focus);
        self.connections.terminal_context_menu = Some(ConnectionTerminalContextMenu {
            tab_id,
            x: panel_x,
            y: panel_y,
        });
        self.connections.tab_context_menu = None;
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.category_context_menu = None;
        self.connections.profile_hover_tooltip = None;
        context.notify();
    }

    /// 执行终端正文右键菜单动作。
    pub(in crate::app) fn handle_connection_terminal_context_menu_action(
        &mut self,
        tab_id: usize,
        action: ConnectionTerminalContextMenuAction,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.connections.terminal_context_menu = None;
        self.connections.active_tab_id = Some(tab_id);
        match action {
            ConnectionTerminalContextMenuAction::FileManager => {
                self.open_connection_file_manager_for_tab(tab_id, window, context);
            }
            ConnectionTerminalContextMenuAction::Copy => {
                let _ = self.copy_selected_connection_terminal_text(context);
            }
            ConnectionTerminalContextMenuAction::Paste => {
                let _ = self.paste_clipboard_text_into_connection_terminal(context);
            }
        }
        context.stop_propagation();
        context.notify();
    }

    /// 打开指定终端 tab 的文件管理窗口。
    ///
    /// 业务意图：
    /// - 文件管理窗口使用独立后端：SSH 重新建立 SFTP 会话，本地终端走本机文件系统。
    /// - 当前阶段不再依赖终端当前目录，统一从当前用户家目录打开，避免 OSC 7 缺失时进入错误目录。
    fn open_connection_file_manager_for_tab(
        &mut self,
        tab_id: usize,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(tab) = self.connections.tabs.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        let initial_path = if tab.file_target_kind == ConnectionTerminalFileTargetKind::Local {
            local_home_directory_text()
        } else {
            ssh_default_directory_text()
        };
        let remote = tab.file_target_kind == ConnectionTerminalFileTargetKind::Ssh;
        let title = format!("{} - 文件管理", tab.title);
        let backend_target = match tab.file_target_kind {
            ConnectionTerminalFileTargetKind::Local => ConnectionFileBackendTarget::Local,
            ConnectionTerminalFileTargetKind::Ssh => {
                let Some(profile) = self.connections.profile_by_id(&tab.profile_id).cloned() else {
                    self.connections.status_message =
                        Some("连接配置不存在，无法打开文件管理".to_string());
                    return;
                };
                let password =
                    match decrypt_connection_password(&profile.id, &profile.encrypted_password) {
                        Ok(password) => password,
                        Err(error) => {
                            self.connections.status_message = Some(error);
                            return;
                        }
                    };
                ConnectionFileBackendTarget::Ssh {
                    trusted_fingerprint: profile.host_key_fingerprint.clone(),
                    profile,
                    password: zeroize::Zeroizing::new(password),
                }
            }
        };
        let main_view = context.entity();
        window.defer(context, move |_window, app| {
            // GPUI 不允许在 `MainView` 正处于更新租借时创建并渲染会读取 `MainView` 的新窗口。
            // 因此文件管理窗口必须延迟到当前事件更新结束后再打开，避免触发实体重入读取保护。
            let window_options = connection_file_manager_window_options(&title, app);
            let main_view_for_window = main_view.clone();
            let main_view_for_error = main_view.clone();
            let open_result = app.open_window(window_options, move |_window, app| {
                let backend = start_connection_file_backend(backend_target);
                app.new(|context| {
                    ConnectionFileManagerWindowView::new(
                        main_view_for_window,
                        title,
                        backend,
                        initial_path,
                        remote,
                        context,
                    )
                })
            });
            if open_result.is_err() {
                let _ = main_view_for_error.update(app, |view, context| {
                    view.connections.status_message = Some("打开文件管理窗口失败".to_string());
                    context.notify();
                });
            }
        });
    }

    /// 展开或收起连接分类。
    pub(in crate::app) fn toggle_connection_category(
        &mut self,
        category_id: &str,
        context: &mut Context<Self>,
    ) {
        if self.connections.expanded_category_ids.contains(category_id) {
            self.connections.expanded_category_ids.remove(category_id);
        } else {
            self.connections
                .expanded_category_ids
                .insert(category_id.to_string());
        }
        self.connections.create_menu_open = false;
        self.connections.profile_context_menu = None;
        self.connections.category_context_menu = None;
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
        cell_width: f32,
        context: &mut Context<Self>,
    ) {
        let Some(tab) = self
            .connections
            .tabs
            .iter_mut()
            .find(|tab| tab.id == tab_id)
        else {
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

    /// 判断 SMB 连接表单当前聚焦字段。
    pub(in crate::app) fn active_smb_connection_form_field(
        &self,
        window: &Window,
    ) -> Option<SmbConnectionFormField> {
        let dialog = self.connections.smb_dialog.as_ref()?;
        if dialog.name.focus.is_focused(window) {
            Some(SmbConnectionFormField::Name)
        } else if dialog.address.focus.is_focused(window) {
            Some(SmbConnectionFormField::Address)
        } else if dialog.username.focus.is_focused(window) {
            Some(SmbConnectionFormField::Username)
        } else if dialog.password.focus.is_focused(window) {
            Some(SmbConnectionFormField::Password)
        } else {
            None
        }
    }

    /// 判断连接分类弹窗名称输入框是否聚焦。
    pub(in crate::app) fn connection_category_name_focused(&self, window: &Window) -> bool {
        self.connections
            .category_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.name.focus.is_focused(window))
    }

    /// 判断连接页是否有文本输入框聚焦。
    pub(in crate::app) fn connection_text_input_focused(&self, window: &Window) -> bool {
        self.connections.tree_search.focus.is_focused(window)
            || self.active_connection_form_field(window).is_some()
            || self.active_smb_connection_form_field(window).is_some()
            || self.connection_category_name_focused(window)
    }

    /// 返回连接树搜索框绘制快照。
    pub(in crate::app) fn connection_tree_search_text_snapshot(
        &self,
    ) -> Option<SingleLineTextInputSnapshot> {
        Some(SingleLineTextInputSnapshot {
            text: self.connections.tree_search.input.text.clone(),
            selection_range: self.connections.tree_search.input.selection_range.clone(),
            marked_range: self.connections.tree_search.input.marked_range.clone(),
            horizontal_scroll_px: self.connections.tree_search.input.horizontal_scroll_px,
        })
    }

    /// 保存连接树搜索框最近一次文本布局。
    pub(in crate::app) fn store_connection_tree_search_text_layout(
        &mut self,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
        horizontal_scroll_px: f32,
    ) {
        self.connections
            .tree_search
            .store_layout(line, bounds, horizontal_scroll_px);
    }

    /// 根据鼠标窗口坐标返回连接树搜索框中的 UTF-8 字节下标。
    pub(in crate::app) fn connection_tree_search_text_index_for_point(
        &self,
        position: gpui::Point<Pixels>,
    ) -> usize {
        let state = &self.connections.tree_search;
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
        text_input_clamp_byte_index(text, display_index.min(text.len()))
    }

    /// 判断连接页当前是否存在需要阻断底层终端输入的模态弹窗。
    ///
    /// 业务意图：
    /// - 新增/编辑、删除确认和主机指纹确认都属于连接页内模态交互；弹窗打开时，底层终端不能继续接收键盘或粘贴。
    /// - 该判断不包含左侧新增类型菜单，因为它不是模态弹窗，只负责自身区域的鼠标消费。
    pub(in crate::app) fn connection_modal_open(&self) -> bool {
        self.connections.dialog.is_some()
            || self.connections.smb_dialog.is_some()
            || self.connections.category_dialog.is_some()
            || self.connections.delete_confirm_dialog.is_some()
            || self.connections.category_delete_confirm_dialog.is_some()
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
            // 终端右键菜单打开时焦点仍可能停留在终端正文；此时键盘和粘贴不能继续写入背后的 shell。
            && self.connections.terminal_context_menu.is_none()
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

    /// 返回 SMB 连接表单字段的绘制快照。
    pub(in crate::app) fn smb_connection_form_text_snapshot(
        &self,
        field: SmbConnectionFormField,
    ) -> Option<SingleLineTextInputSnapshot> {
        let state = self.connections.smb_dialog.as_ref()?.field(field);
        Some(SingleLineTextInputSnapshot {
            text: state.input.text.clone(),
            selection_range: state.input.selection_range.clone(),
            marked_range: state.input.marked_range.clone(),
            horizontal_scroll_px: state.input.horizontal_scroll_px,
        })
    }

    /// 返回连接分类名称输入框绘制快照。
    pub(in crate::app) fn connection_category_text_snapshot(
        &self,
    ) -> Option<SingleLineTextInputSnapshot> {
        let state = &self.connections.category_dialog.as_ref()?.name;
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

    /// 保存 SMB 连接表单字段最近一次文本布局。
    pub(in crate::app) fn store_smb_connection_form_text_layout(
        &mut self,
        field: SmbConnectionFormField,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
        horizontal_scroll_px: f32,
    ) {
        if let Some(dialog) = self.connections.smb_dialog.as_mut() {
            dialog
                .field_mut(field)
                .store_layout(line, bounds, horizontal_scroll_px);
        }
    }

    /// 保存连接分类名称输入框最近一次文本布局。
    pub(in crate::app) fn store_connection_category_text_layout(
        &mut self,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
        horizontal_scroll_px: f32,
    ) {
        if let Some(dialog) = self.connections.category_dialog.as_mut() {
            dialog.name.store_layout(line, bounds, horizontal_scroll_px);
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

    /// 根据鼠标窗口坐标返回 SMB 连接表单字段中的 UTF-8 字节下标。
    pub(in crate::app) fn smb_connection_form_text_index_for_point(
        &self,
        field: SmbConnectionFormField,
        position: gpui::Point<Pixels>,
    ) -> usize {
        let Some(dialog) = self.connections.smb_dialog.as_ref() else {
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
        if field == SmbConnectionFormField::Password {
            TextInputDisplayMode::Masked { mask_char: '*' }
                .text_index_for_display_index(text, display_index)
        } else {
            text_input_clamp_byte_index(text, display_index.min(text.len()))
        }
    }

    /// 根据鼠标窗口坐标返回连接分类名称输入框中的 UTF-8 字节下标。
    pub(in crate::app) fn connection_category_text_index_for_point(
        &self,
        position: gpui::Point<Pixels>,
    ) -> usize {
        let Some(dialog) = self.connections.category_dialog.as_ref() else {
            return 0;
        };
        let state = &dialog.name;
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
        text_input_clamp_byte_index(text, display_index.min(text.len()))
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

    /// 处理 SMB 连接表单按键。
    pub(in crate::app) fn handle_smb_connection_form_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(field) = self.active_smb_connection_form_field(window) else {
            return;
        };

        if event.keystroke.key == "escape" {
            self.connections.smb_dialog = None;
            context.stop_propagation();
            context.notify();
            return;
        }
        if event.keystroke.key == "enter" {
            self.save_smb_connection_dialog(&ClickEvent::default(), window, context);
            return;
        }

        let Some(dialog) = self.connections.smb_dialog.as_mut() else {
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

    /// 处理连接树搜索框按键。
    ///
    /// 业务意图：
    /// - 普通字符和中文 IME 由 `EntityInputHandler` 提交；这里只处理剪贴板、方向键、删除和清空。
    /// - 搜索框只过滤左侧连接树，不启动网络连接、不修改 SQLite。
    pub(in crate::app) fn handle_connection_tree_search_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.handle_connection_single_line_input_key_down(
            TextInputBinding::ConnectionTreeSearch,
            event,
            context,
        ) {
            return;
        }

        if event.keystroke.key == "escape" && !self.connections.tree_search.input.text.is_empty() {
            self.connections.tree_search.input = SingleLineTextInputState::empty();
            self.connections.tree_search.clear_layout();
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
        }
    }

    /// 处理连接分类名称输入框按键。
    pub(in crate::app) fn handle_connection_category_name_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if event.keystroke.key == "escape" {
            self.connections.category_dialog = None;
            context.stop_propagation();
            context.notify();
            return;
        }
        if event.keystroke.key == "enter" {
            self.save_connection_category_dialog(&ClickEvent::default(), window, context);
            return;
        }

        if self.handle_connection_single_line_input_key_down(
            TextInputBinding::ConnectionCategoryName,
            event,
            context,
        ) && let Some(dialog) = self.connections.category_dialog.as_mut()
        {
            dialog.error = None;
        }
    }

    /// 处理连接页通用单行输入框基础编辑按键。
    fn handle_connection_single_line_input_key_down(
        &mut self,
        binding: TextInputBinding,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) -> bool {
        if Self::is_paste_keystroke(&event.keystroke) {
            if let Some(text) = context.read_from_clipboard().and_then(|item| item.text())
                && let Some(input) = self.connection_single_line_input_mut(binding)
            {
                replace_text_input_selection(input, &sanitize_text_input_single_line_text(&text));
                self.after_connection_text_input_changed(binding);
            }
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return true;
        }

        let Some(input) = self.connection_single_line_input_mut(binding) else {
            return false;
        };
        if Self::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = text_input_selected_text(input) {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return true;
        }
        if Self::is_cut_keystroke(&event.keystroke) {
            if let Some(text) = text_input_selected_text(input) {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                replace_text_input_selection(input, "");
                self.after_connection_text_input_changed(binding);
            }
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return true;
        }
        if Self::is_select_all_keystroke(&event.keystroke) {
            select_all_text_input(input);
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return true;
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
                self.after_connection_text_input_changed(binding);
            }
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return true;
        }
        false
    }

    /// 返回连接页通用单行输入框可变状态。
    fn connection_single_line_input_mut(
        &mut self,
        binding: TextInputBinding,
    ) -> Option<&mut SingleLineTextInputState> {
        match binding {
            TextInputBinding::ConnectionTreeSearch => Some(&mut self.connections.tree_search.input),
            TextInputBinding::ConnectionCategoryName => self
                .connections
                .category_dialog
                .as_mut()
                .map(|dialog| &mut dialog.name.input),
            _ => None,
        }
    }

    /// 连接页单行输入框文本变更后的本地副作用。
    fn after_connection_text_input_changed(&mut self, binding: TextInputBinding) {
        match binding {
            TextInputBinding::ConnectionTreeSearch => {
                self.connections.tree_search.clear_layout();
                self.connections.create_menu_open = false;
                self.connections.profile_context_menu = None;
                self.connections.category_context_menu = None;
            }
            TextInputBinding::ConnectionCategoryName => {
                if let Some(dialog) = self.connections.category_dialog.as_mut() {
                    dialog.name.clear_layout();
                    dialog.error = None;
                }
            }
            _ => {}
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
    /// - GPUI 鼠标事件是窗口坐标，连接页右侧终端还要扣除主导航、连接列表、tab 栏和终端内边距。
    /// - 鼠标拖到终端外时交给 `point_from_panel_offset` 夹紧，避免选区或鼠标上报生成越界坐标。
    fn active_connection_terminal_point_from_window_position(
        &self,
        x: f32,
        y: f32,
    ) -> Option<Point> {
        let tab = self.connections.active_tab()?;
        let bounds = tab.content_bounds?;
        let local_x = x - f32::from(bounds.left());
        let local_y = y - f32::from(bounds.top());
        Some(
            tab.emulator
                .point_from_panel_offset(local_x, local_y, tab.cell_width),
        )
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
/// - 普通可打印字符由 `EntityInputHandler` 处理，避免注册 IME 后 ASCII 字符在 keydown 和平台输入提交两条路径重复写入。
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
        _ => return None,
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
