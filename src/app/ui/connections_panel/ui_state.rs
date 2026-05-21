// 连接页 UI 状态和终端模拟器适配。
//
// 业务意图：
// - 本文件只定义 GPUI 会话状态、表单状态、tab 状态和 alacritty_terminal 渲染快照。
// - SQLite、加密和 SSH 网络任务都留在 `crate::connections` 业务域，避免 UI 状态直接承担安全和 IO 细节。

use std::{ops::Range, path::PathBuf};

use alacritty_terminal::{
    event::VoidListener,
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    term::{
        Config as AlacrittyTermConfig, Term, TermMode,
        cell::{Cell, Flags},
        test::TermSize,
    },
    vte::ansi::{Color, NamedColor, Processor, Rgb},
};
use zeroize::Zeroize;

use super::*;

/// 连接页左侧栏默认宽度。
///
/// UI 约束：
/// - 与笔记树和日志树一样左侧固定起步宽度，避免连接名称较长时压缩右侧终端首屏。
pub(in crate::app) const CONNECTIONS_TREE_DEFAULT_WIDTH: f32 = 300.0;

/// 连接页左侧栏最小宽度。
pub(in crate::app) const CONNECTIONS_TREE_MIN_WIDTH: f32 = 240.0;

/// 连接页左侧栏最大宽度。
pub(in crate::app) const CONNECTIONS_TREE_MAX_WIDTH: f32 = 460.0;

/// 连接页工具栏高度。
///
/// UI 约束：
/// - 连接页顶部栏需要和日志分析页操作栏保持同高，避免主导航切换时顶部节奏跳变。
pub(in crate::app) const CONNECTIONS_TOOLBAR_HEIGHT: f32 = TOOLBAR_HEIGHT;

/// 终端 tab 栏高度。
///
/// UI 约束：
/// - 右侧 tab 栏必须和左侧连接工具栏同高，否则空 tab 状态下左右顶部边线会错位。
pub(in crate::app) const CONNECTIONS_TAB_BAR_HEIGHT: f32 = CONNECTIONS_TOOLBAR_HEIGHT;

/// 终端内容内边距。
///
/// 实现原因：
/// - 终端文本不能紧贴 tab 栏和右侧边界，否则光标、中文宽字符和选区边缘在亮色主题下会显得拥挤。
/// - 该值同时参与鼠标格点换算，保证点击位置与实际绘制文本保持一致。
pub(in crate::app) const CONNECTION_TERMINAL_PADDING: f32 = 2.0;

/// 终端默认字体大小。
pub(in crate::app) const CONNECTION_TERMINAL_FONT_SIZE: f32 = 13.0;

/// 终端单行高度。
///
/// 实现原因：
/// - GPUI 目前这里使用自绘行映射，不直接拿到底层字体测量对象；固定行高能保证 tab 切换、滚动和光标绘制不抖动。
pub(in crate::app) const CONNECTION_TERMINAL_ROW_HEIGHT: f32 = 18.0;

/// 终端单列估算宽度。
///
/// 边界条件：
/// - 第一版按 JetBrains Mono 13px 的经验宽度估算；窗口 resize 会基于面板约束重新计算列数，
///   后续如果引入真实字体测量，应只替换这一层，不影响后端抽象。
pub(in crate::app) const CONNECTION_TERMINAL_CELL_WIDTH: f32 = 8.0;

/// 连接页事件轮询间隔。
pub(in crate::app) const CONNECTIONS_TERMINAL_POLL_MILLIS: u64 = 30;

/// 左侧连接行右键菜单宽度。
///
/// UI 约束：
/// - 菜单需要容纳“连接 / 编辑 / 删除”三项，同时不能覆盖过多连接名称区域。
pub(in crate::app) const CONNECTIONS_CONTEXT_MENU_WIDTH: f32 = 148.0;

/// 左侧连接行右键菜单单项高度。
pub(in crate::app) const CONNECTIONS_CONTEXT_MENU_ITEM_HEIGHT: f32 = 32.0;

/// 左侧连接行右键菜单纵向内边距总和。
///
/// UI 约束：
/// - 右键菜单使用 `.py_1()`，上下各 4px；坐标夹紧必须把这部分高度算进去，否则底部菜单项仍可能被窗口裁掉。
pub(in crate::app) const CONNECTIONS_CONTEXT_MENU_VERTICAL_PADDING: f32 = 8.0;

/// 连接终端区域主题色。
///
/// 业务意图：
/// - 终端 tab、正文背景和默认 ANSI 前景/背景必须跟随应用明暗主题，避免亮色主题下仍出现大面积深色块。
/// - 这里只集中定义连接页终端外壳颜色；远端程序主动输出的 256 色或 truecolor 仍按终端协议优先展示。
#[derive(Clone, Copy)]
pub(in crate::app) struct ConnectionTerminalUiColors {
    /// 终端正文背景色。
    pub(in crate::app) background: u32,
    /// 终端默认文字色。
    pub(in crate::app) foreground: u32,
    /// 终端次级文字色，例如空状态和 tab 图标。
    pub(in crate::app) muted: u32,
    /// 激活 tab 背景色。
    pub(in crate::app) active_tab_background: u32,
    /// 未激活 tab 背景色。
    pub(in crate::app) inactive_tab_background: u32,
    /// 激活 tab 文字色。
    pub(in crate::app) active_tab_text: u32,
    /// 未激活 tab 文字色。
    pub(in crate::app) inactive_tab_text: u32,
    /// tab 关闭按钮悬浮背景色。
    pub(in crate::app) close_hover: u32,
    /// 终端光标背景色。
    pub(in crate::app) cursor_background: u32,
    /// 终端光标文字色。
    pub(in crate::app) cursor_foreground: u32,
    /// 终端选区背景色。
    pub(in crate::app) selection_background: u32,
    /// 终端选区文字色。
    pub(in crate::app) selection_foreground: u32,
}

/// 返回连接终端在当前主题下的外壳颜色。
pub(in crate::app) fn connection_terminal_ui_colors(
    theme: EffectiveTheme,
    palette: AppThemePalette,
) -> ConnectionTerminalUiColors {
    match theme {
        EffectiveTheme::Light => ConnectionTerminalUiColors {
            background: palette.surface,
            foreground: palette.text,
            muted: palette.muted_text,
            active_tab_background: palette.surface,
            inactive_tab_background: palette.panel,
            active_tab_text: palette.text,
            inactive_tab_text: palette.muted_text,
            close_hover: palette.hover,
            cursor_background: palette.text,
            cursor_foreground: palette.surface,
            selection_background: palette.accent,
            selection_foreground: palette.on_accent,
        },
        EffectiveTheme::Dark => ConnectionTerminalUiColors {
            background: 0x0f172a,
            foreground: 0xe5e7eb,
            muted: 0x94a3b8,
            active_tab_background: 0x0f172a,
            inactive_tab_background: palette.panel,
            active_tab_text: 0xe5e7eb,
            inactive_tab_text: palette.text,
            close_hover: 0x334155,
            cursor_background: 0xf8fafc,
            cursor_foreground: 0x0f172a,
            selection_background: 0x2563eb,
            selection_foreground: 0xffffff,
        },
    }
}

/// 返回连接终端默认 ANSI 前景/背景颜色。
fn connection_terminal_default_colors(theme: EffectiveTheme) -> ConnectionTerminalUiColors {
    connection_terminal_ui_colors(theme, AppThemePalette::for_theme(theme))
}

/// 根据终端内容区 bounds 计算 PTY 行列和像素尺寸。
///
/// 业务意图：
/// - GPUI 布局完成后才能知道右侧终端真实大小；连接 tab 初始只能使用 80x24，首帧后必须按实际面板同步。
/// - 该计算同时服务 alacritty emulator 和 SSH remote pty，保证渲染行列、鼠标格点和远端 TUI 程序看到的窗口尺寸一致。
///
/// 边界条件：
/// - 宽高为 0 时仍返回至少 1 行 1 列，避免底层终端状态机或 SSH `window-change` 收到非法尺寸。
/// - 像素尺寸按 u16 夹紧，兼容 portable-pty 和 SSH pty 请求字段。
pub(in crate::app) fn connection_terminal_size_from_bounds(bounds: Bounds<Pixels>) -> TerminalSize {
    let width = f32::from(bounds.size.width).max(0.0);
    let height = f32::from(bounds.size.height).max(0.0);
    let cols = (width / CONNECTION_TERMINAL_CELL_WIDTH)
        .floor()
        .clamp(1.0, u16::MAX as f32) as u16;
    let rows = (height / CONNECTION_TERMINAL_ROW_HEIGHT)
        .floor()
        .clamp(1.0, u16::MAX as f32) as u16;
    let pixel_width = width.round().clamp(0.0, u16::MAX as f32) as u16;
    let pixel_height = height.round().clamp(0.0, u16::MAX as f32) as u16;

    TerminalSize::new(cols, rows, pixel_width, pixel_height)
}

/// 连接页完整工作区状态。
pub(in crate::app) struct ConnectionsWorkspaceState {
    /// 连接数据库路径；配置目录不可用时为 `None`，此时连接页只展示错误不阻断其它页面。
    pub(in crate::app) database_path: Option<PathBuf>,
    /// SSH 连接配置列表，来自独立 SQLite。
    pub(in crate::app) profiles: Vec<ConnectionProfile>,
    /// 数据库初始化或读取错误，中文展示给用户。
    pub(in crate::app) database_error: Option<String>,
    /// 左侧当前选中的连接 ID；只影响高亮和编辑/删除默认目标。
    pub(in crate::app) selected_profile_id: Option<String>,
    /// 左侧栏宽度，当前只保存进程内状态。
    pub(in crate::app) tree_width: f32,
    /// 当前是否正在拖拽调整左侧栏宽度。
    pub(in crate::app) tree_resize_drag: Option<ConnectionsTreeResizeDrag>,
    /// 下一个终端 tab ID；每次点击连接都创建新 tab，不按连接去重。
    pub(in crate::app) next_tab_id: usize,
    /// 当前激活终端 tab ID。
    pub(in crate::app) active_tab_id: Option<usize>,
    /// 已打开的终端 tab。
    pub(in crate::app) tabs: Vec<ConnectionTerminalTab>,
    /// 终端 tab 横向滚动句柄。
    pub(in crate::app) tab_bar_scroll_handle: ScrollHandle,
    /// 新建连接类型菜单是否打开。
    ///
    /// 业务意图：
    /// - 顶部新增按钮不直接假定只有 SSH，而是先打开类型菜单；当前菜单包含 SSH 和本地终端，后续可追加其它协议。
    /// - 菜单状态只属于左侧连接栏，关闭或选择后必须立即复位，避免遮挡连接列表点击。
    pub(in crate::app) create_menu_open: bool,
    /// 左侧连接行右键菜单。
    ///
    /// UI 约束：
    /// - 连接卡片本身只负责单击直连，编辑和删除等低频动作统一收进右键菜单，避免卡片内按钮互相抢占点击区域。
    /// - 菜单坐标保存为左侧连接栏内部坐标；鼠标事件进入时需要扣除主导航宽度并限制在侧栏范围内。
    pub(in crate::app) profile_context_menu: Option<ConnectionProfileContextMenu>,
    /// 新增/编辑连接弹窗状态。
    pub(in crate::app) dialog: Option<ConnectionDialogState>,
    /// 删除连接确认弹窗状态。
    pub(in crate::app) delete_confirm_dialog: Option<ConnectionDeleteConfirmDialog>,
    /// 首次信任或指纹不匹配弹窗状态。
    pub(in crate::app) host_key_dialog: Option<ConnectionHostKeyDialog>,
    /// 当前页面状态提示，保存、删除、连接失败等短消息写在这里。
    pub(in crate::app) status_message: Option<String>,
    /// 是否已经安排终端事件轮询任务，避免每个 tab 重复启动轮询循环。
    pub(in crate::app) terminal_poll_scheduled: bool,
}

impl ConnectionsWorkspaceState {
    /// 初始化连接页状态。
    ///
    /// 边界条件：
    /// - 数据库不可用时保留空列表和错误消息，主窗口仍然可以打开日志、笔记、HPROF 和 AI 页。
    pub(in crate::app) fn load_or_initialize(_context: &mut Context<MainView>) -> Self {
        let database_path = connections_database_path();
        let (profiles, database_error) = match database_path.as_ref() {
            Some(path) => match load_connection_profiles(path) {
                Ok(profiles) => (profiles, None),
                Err(error) => (Vec::new(), Some(error)),
            },
            None => (
                Vec::new(),
                Some("无法定位应用配置目录，连接配置不会被加载".to_string()),
            ),
        };
        let selected_profile_id = profiles.first().map(|profile| profile.id.clone());

        Self {
            database_path,
            profiles,
            database_error,
            selected_profile_id,
            tree_width: CONNECTIONS_TREE_DEFAULT_WIDTH,
            tree_resize_drag: None,
            next_tab_id: 1,
            active_tab_id: None,
            tabs: Vec::new(),
            tab_bar_scroll_handle: ScrollHandle::new(),
            create_menu_open: false,
            profile_context_menu: None,
            dialog: None,
            delete_confirm_dialog: None,
            host_key_dialog: None,
            status_message: None,
            terminal_poll_scheduled: false,
        }
    }

    /// 根据连接 ID 查找配置。
    pub(in crate::app) fn profile_by_id(&self, profile_id: &str) -> Option<&ConnectionProfile> {
        self.profiles
            .iter()
            .find(|profile| profile.id == profile_id)
    }

    /// 重新加载 SQLite 中的连接列表。
    pub(in crate::app) fn reload_profiles(&mut self) {
        let Some(path) = self.database_path.as_ref() else {
            self.database_error = Some("无法定位应用配置目录，连接配置不会被加载".to_string());
            self.profiles.clear();
            self.selected_profile_id = None;
            return;
        };

        match load_connection_profiles(path) {
            Ok(profiles) => {
                self.database_error = None;
                self.profiles = profiles;
                if let Some(selected) = self.selected_profile_id.as_ref()
                    && self.profiles.iter().any(|profile| &profile.id == selected)
                {
                    return;
                }
                self.selected_profile_id = self.profiles.first().map(|profile| profile.id.clone());
            }
            Err(error) => {
                self.database_error = Some(error);
            }
        }
    }

    /// 关闭指定连接打开的所有终端 tab。
    pub(in crate::app) fn close_tabs_for_profile(&mut self, profile_id: &str) {
        for tab in self.tabs.iter().filter(|tab| tab.profile_id == profile_id) {
            tab.backend.shutdown();
        }
        self.tabs.retain(|tab| tab.profile_id != profile_id);
        if let Some(active) = self.active_tab_id
            && !self.tabs.iter().any(|tab| tab.id == active)
        {
            self.active_tab_id = self.tabs.last().map(|tab| tab.id);
        }
    }

    /// 关闭单个终端 tab。
    pub(in crate::app) fn close_tab(&mut self, tab_id: usize) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.id == tab_id) {
            self.tabs[index].backend.shutdown();
            self.tabs.remove(index);
        }
        if self.active_tab_id == Some(tab_id) {
            self.active_tab_id = self.tabs.last().map(|tab| tab.id);
        }
    }

    /// 返回当前激活 tab 的可变引用。
    pub(in crate::app) fn active_tab_mut(&mut self) -> Option<&mut ConnectionTerminalTab> {
        let active_tab_id = self.active_tab_id?;
        self.tabs.iter_mut().find(|tab| tab.id == active_tab_id)
    }

    /// 返回当前激活 tab 的只读引用。
    pub(in crate::app) fn active_tab(&self) -> Option<&ConnectionTerminalTab> {
        let active_tab_id = self.active_tab_id?;
        self.tabs.iter().find(|tab| tab.id == active_tab_id)
    }
}

/// 左侧连接栏拖拽状态。
pub(in crate::app) struct ConnectionsTreeResizeDrag {
    /// 拖拽开始时鼠标 X 坐标。
    pub(in crate::app) start_x: f32,
    /// 拖拽开始时左侧栏宽度。
    pub(in crate::app) start_width: f32,
}

/// 新建连接菜单中的连接类型。
///
/// 业务意图：
/// - 新增入口从一开始按类型建模；SSH 需要持久化配置和表单，本地终端则直接启动 PTY tab。
/// - 后续扩展串口、跳板机或其它协议时，只需要追加类型和分发动作，避免重写新增菜单交互。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionCreateKind {
    /// SSH 远程连接。
    Ssh,
    /// 本机 shell 终端，不保存连接配置。
    LocalTerminal,
}

/// 新建连接菜单当前展示的连接类型顺序。
///
/// UI 约束：
/// - SSH 放在第一项，保持已有用户路径不变；本地终端作为无需配置的快捷入口紧随其后。
pub(in crate::app) const CONNECTION_CREATE_KINDS: &[ConnectionCreateKind] = &[
    ConnectionCreateKind::Ssh,
    ConnectionCreateKind::LocalTerminal,
];

impl ConnectionCreateKind {
    /// 返回菜单展示文案。
    pub(in crate::app) const fn label(self) -> &'static str {
        match self {
            Self::Ssh => "SSH 连接",
            Self::LocalTerminal => "本地终端",
        }
    }

    /// 返回菜单图标。
    pub(in crate::app) const fn icon(self) -> Icon {
        match self {
            Self::Ssh => Icon::Terminal,
            Self::LocalTerminal => Icon::SquareTerminal,
        }
    }
}

/// 左侧连接行右键菜单状态。
pub(in crate::app) struct ConnectionProfileContextMenu {
    /// 菜单目标连接 ID。
    pub(in crate::app) profile_id: String,
    /// 菜单在连接侧栏内部的横坐标。
    pub(in crate::app) x: f32,
    /// 菜单在连接侧栏内部的纵坐标。
    pub(in crate::app) y: f32,
}

/// 左侧连接行右键菜单动作。
///
/// 业务意图：
/// - 单击连接行已变为直接连接；右键菜单保留“连接”以兼容用户从菜单确认动作的习惯。
/// - 编辑和删除都需要明确指向当前右键目标，避免依赖左侧当前高亮项造成误操作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionProfileContextMenuAction {
    /// 打开一个新的 SSH 终端 tab。
    Connect,
    /// 打开编辑连接弹窗。
    Edit,
    /// 打开删除确认弹窗。
    Delete,
}

/// 新增/编辑连接弹窗状态。
pub(in crate::app) struct ConnectionDialogState {
    /// 弹窗模式。
    pub(in crate::app) mode: ConnectionDialogMode,
    /// 表单错误消息。
    pub(in crate::app) error: Option<String>,
    /// 名称输入状态。
    pub(in crate::app) name: ConnectionFormTextFieldState,
    /// 主机输入状态。
    pub(in crate::app) host: ConnectionFormTextFieldState,
    /// 端口输入状态。
    pub(in crate::app) port: ConnectionFormTextFieldState,
    /// 用户名输入状态。
    pub(in crate::app) username: ConnectionFormTextFieldState,
    /// 密码输入状态；编辑时为空表示不修改旧密码。
    pub(in crate::app) password: ConnectionFormTextFieldState,
}

impl ConnectionDialogState {
    /// 创建新增连接弹窗。
    pub(in crate::app) fn create(context: &mut Context<MainView>) -> Self {
        Self {
            mode: ConnectionDialogMode::Create,
            error: None,
            name: ConnectionFormTextFieldState::new(String::new(), context),
            host: ConnectionFormTextFieldState::new(String::new(), context),
            port: ConnectionFormTextFieldState::new(DEFAULT_SSH_PORT.to_string(), context),
            username: ConnectionFormTextFieldState::new(String::new(), context),
            password: ConnectionFormTextFieldState::new(String::new(), context),
        }
    }

    /// 创建编辑连接弹窗。
    pub(in crate::app) fn edit(
        profile: &ConnectionProfile,
        context: &mut Context<MainView>,
    ) -> Self {
        Self {
            mode: ConnectionDialogMode::Edit {
                profile_id: profile.id.clone(),
            },
            error: None,
            name: ConnectionFormTextFieldState::new(profile.name.clone(), context),
            host: ConnectionFormTextFieldState::new(profile.host.clone(), context),
            port: ConnectionFormTextFieldState::new(profile.port.to_string(), context),
            username: ConnectionFormTextFieldState::new(profile.username.clone(), context),
            password: ConnectionFormTextFieldState::new(String::new(), context),
        }
    }

    /// 生成保存校验使用的纯连接草稿。
    ///
    /// 业务意图：
    /// - UI 输入框需要保存选区、IME 和布局信息，但领域层校验只应该接收纯字符串。
    /// - 这里在保存瞬间生成短生命周期草稿，密码草稿在离开作用域时会主动清零。
    pub(in crate::app) fn to_profile_draft(&self) -> ConnectionProfileDraft {
        ConnectionProfileDraft {
            name: self.name.input.text.clone(),
            host: self.host.input.text.clone(),
            port_text: self.port.input.text.clone(),
            username: self.username.input.text.clone(),
            password: self.password.input.text.clone(),
        }
    }

    /// 返回指定字段的输入状态。
    pub(in crate::app) fn field(
        &self,
        field: ConnectionFormField,
    ) -> &ConnectionFormTextFieldState {
        match field {
            ConnectionFormField::Name => &self.name,
            ConnectionFormField::Host => &self.host,
            ConnectionFormField::Port => &self.port,
            ConnectionFormField::Username => &self.username,
            ConnectionFormField::Password => &self.password,
        }
    }

    /// 返回指定字段的可变输入状态。
    pub(in crate::app) fn field_mut(
        &mut self,
        field: ConnectionFormField,
    ) -> &mut ConnectionFormTextFieldState {
        match field {
            ConnectionFormField::Name => &mut self.name,
            ConnectionFormField::Host => &mut self.host,
            ConnectionFormField::Port => &mut self.port,
            ConnectionFormField::Username => &mut self.username,
            ConnectionFormField::Password => &mut self.password,
        }
    }
}

impl Drop for ConnectionDialogState {
    /// 弹窗销毁时清理密码明文。
    ///
    /// 安全边界：
    /// - Rust 和系统分配器无法保证所有历史副本都立即消失，但主动清零当前 UI 状态可以避免弹窗关闭后继续保留主密码副本。
    fn drop(&mut self) {
        self.password.input.text.zeroize();
    }
}

/// 连接表单单行输入状态。
///
/// 业务意图：
/// - 连接表单第一版仍是自绘 UI，但需要复用通用输入框的正常文本能力：IME、选区、剪贴板、鼠标定位和水平滚动。
/// - 每个字段独立保存布局，平台输入协议才能把候选窗口和鼠标命中映射到正确字段。
pub(in crate::app) struct ConnectionFormTextFieldState {
    /// 单行输入框通用编辑状态。
    pub(in crate::app) input: SingleLineTextInputState,
    /// GPUI 焦点句柄。
    pub(in crate::app) focus: FocusHandle,
    /// 最近一次绘制的字形布局，用于鼠标命中和 IME 候选窗口定位。
    pub(in crate::app) last_layout: Option<ShapedLine>,
    /// 最近一次绘制的输入框窗口坐标。
    pub(in crate::app) last_bounds: Option<Bounds<Pixels>>,
}

impl ConnectionFormTextFieldState {
    /// 创建连接表单输入状态。
    pub(in crate::app) fn new(text: String, context: &mut Context<MainView>) -> Self {
        Self {
            input: SingleLineTextInputState::from_text(text),
            focus: context.focus_handle(),
            last_layout: None,
            last_bounds: None,
        }
    }

    /// 记录最近一次绘制布局。
    pub(in crate::app) fn store_layout(
        &mut self,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
        horizontal_scroll_px: f32,
    ) {
        self.last_layout = Some(line);
        self.last_bounds = Some(bounds);
        self.input.horizontal_scroll_px = horizontal_scroll_px;
    }

    /// 清理布局缓存。
    pub(in crate::app) fn clear_layout(&mut self) {
        self.last_layout = None;
        self.last_bounds = None;
        self.input.horizontal_scroll_px = 0.0;
    }
}

/// 连接弹窗模式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionDialogMode {
    /// 新增连接。
    Create,
    /// 编辑已有连接。
    Edit {
        /// 正在编辑的连接 ID。
        profile_id: String,
    },
}

/// 连接表单字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionFormField {
    /// 名称。
    Name,
    /// 主机。
    Host,
    /// 端口。
    Port,
    /// 用户名。
    Username,
    /// 密码。
    Password,
}

/// 删除连接确认弹窗。
pub(in crate::app) struct ConnectionDeleteConfirmDialog {
    /// 待删除连接 ID。
    pub(in crate::app) profile_id: String,
    /// 待删除连接名称，用于确认文案。
    pub(in crate::app) profile_name: String,
}

/// SSH 主机指纹弹窗。
pub(in crate::app) struct ConnectionHostKeyDialog {
    /// 触发指纹事件的连接 ID。
    pub(in crate::app) profile_id: String,
    /// 触发指纹事件的 tab ID。
    pub(in crate::app) tab_id: usize,
    /// 服务器本次返回的指纹。
    pub(in crate::app) fingerprint: String,
    /// 如果是指纹不匹配，这里保存旧指纹；首次信任时为 `None`。
    pub(in crate::app) expected: Option<String>,
}

/// 终端 tab 状态。
pub(in crate::app) struct ConnectionTerminalTab {
    /// tab 内部 ID，每次打开连接递增。
    pub(in crate::app) id: usize,
    /// 连接配置 ID。
    pub(in crate::app) profile_id: String,
    /// tab 标题快照。
    pub(in crate::app) title: String,
    /// 后端命令和事件通道。
    pub(in crate::app) backend: TerminalBackendHandle,
    /// alacritty_terminal 模拟器状态。
    pub(in crate::app) emulator: ConnectionTerminalEmulator,
    /// tab 当前连接状态。
    pub(in crate::app) status: ConnectionTerminalStatus,
    /// 终端焦点句柄，键盘输入只在当前 tab 聚焦时写入后端。
    pub(in crate::app) focus: FocusHandle,
    /// 选区拖拽锚点；鼠标释放后清空。
    pub(in crate::app) selection_anchor: Option<Point>,
    /// xterm 鼠标上报拖拽状态；远端开启鼠标模式时使用它决定移动和释放事件是否要写回后端。
    pub(in crate::app) mouse_reporting_drag: bool,
    /// 后端事件通道是否已经结束。
    ///
    /// 业务意图：
    /// - 终端 tab 可以保留退出状态供用户查看，但后台轮询必须停止读取已经断开的通道，避免持续 30ms 刷新 UI。
    pub(in crate::app) backend_finished: bool,
}

impl ConnectionTerminalTab {
    /// 请求后端写入输入字节。
    pub(in crate::app) fn write_input(&self, bytes: Vec<u8>) {
        let _ = self
            .backend
            .command_sender
            .send(TerminalBackendCommand::Input(bytes));
    }

    /// 请求后端调整尺寸。
    #[allow(dead_code)]
    pub(in crate::app) fn resize(&mut self, size: TerminalSize) {
        self.emulator.resize(size);
        let _ = self
            .backend
            .command_sender
            .send(TerminalBackendCommand::Resize(size));
    }
}

/// 终端 tab 连接状态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionTerminalStatus {
    /// 正在连接。
    Connecting,
    /// 首次信任等待用户确认。
    WaitingHostKey,
    /// shell 已可用。
    Connected,
    /// 会话已退出。
    Exited(String),
    /// 会话失败。
    Error(String),
}

/// 终端模拟器状态。
pub(in crate::app) struct ConnectionTerminalEmulator {
    /// alacritty 终端状态机，负责 ANSI、scrollback、alternate screen、cursor 和 selection。
    term: Term<VoidListener>,
    /// vte parser，把后端字节流喂进 alacritty 终端状态机。
    parser: Processor,
    /// 当前终端尺寸，供输入、粘贴、选区和渲染层复用。
    pub(in crate::app) size: TerminalSize,
}

impl ConnectionTerminalEmulator {
    /// 创建终端模拟器。
    pub(in crate::app) fn new(size: TerminalSize) -> Self {
        let config = AlacrittyTermConfig {
            scrolling_history: TERMINAL_SCROLLBACK_LINES,
            ..AlacrittyTermConfig::default()
        };
        let term_size = TermSize::new(size.cols as usize, size.rows as usize);
        Self {
            term: Term::new(config, &term_size, VoidListener),
            parser: Processor::new(),
            size,
        }
    }

    /// 把后端输出写入终端状态机。
    pub(in crate::app) fn feed_output(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// 调整终端模拟器尺寸。
    #[allow(dead_code)]
    pub(in crate::app) fn resize(&mut self, size: TerminalSize) {
        if self.size == size {
            return;
        }
        self.size = size;
        self.term
            .resize(TermSize::new(size.cols as usize, size.rows as usize));
    }

    /// 判断 bracketed paste 是否开启。
    pub(in crate::app) fn bracketed_paste_enabled(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// 判断 xterm 鼠标上报是否开启。
    #[allow(dead_code)]
    pub(in crate::app) fn mouse_reporting_enabled(&self) -> bool {
        self.term.mode().intersects(TermMode::MOUSE_MODE)
    }

    /// 开始终端选区。
    pub(in crate::app) fn begin_selection(&mut self, point: Point) {
        self.term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
    }

    /// 更新终端选区。
    pub(in crate::app) fn update_selection(&mut self, point: Point) {
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, Side::Right);
        }
    }

    /// 清空终端选区。
    #[allow(dead_code)]
    pub(in crate::app) fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// 获取当前选区文本。
    pub(in crate::app) fn selected_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    /// 将面板内坐标估算为终端格点。
    ///
    /// 边界条件：
    /// - 鼠标可能拖到终端面板外，行列统一夹到可见区域，避免 Selection 收到越界点。
    pub(in crate::app) fn point_from_panel_offset(&self, x: f32, y: f32) -> Point {
        let column = (x / CONNECTION_TERMINAL_CELL_WIDTH)
            .floor()
            .max(0.0)
            .min((self.size.cols.saturating_sub(1)) as f32) as usize;
        let line = (y / CONNECTION_TERMINAL_ROW_HEIGHT)
            .floor()
            .max(0.0)
            .min((self.size.rows.saturating_sub(1)) as f32) as i32;
        Point::new(Line(line), Column(column))
    }

    /// 生成 GPUI 渲染用行快照。
    pub(in crate::app) fn render_lines(&self, theme: EffectiveTheme) -> Vec<TerminalRenderLine> {
        let content = self.term.renderable_content();
        let selection = content.selection;
        let cursor = content.cursor;
        let mut rows = vec![TerminalRenderLine::empty(); self.size.rows as usize];

        for indexed in content.display_iter {
            if indexed.point.line.0 < 0 {
                continue;
            }
            let row_index = indexed.point.line.0 as usize;
            if row_index >= rows.len() {
                continue;
            }
            let selected = selection
                .is_some_and(|range| range.contains_cell(&indexed, indexed.point, cursor.shape));
            let is_cursor = cursor.point == indexed.point;
            rows[row_index].push_cell(
                indexed.cell,
                selected,
                is_cursor,
                self.size.cols as usize,
                theme,
            );
        }

        rows
    }
}

/// 单行终端渲染快照。
#[derive(Clone)]
pub(in crate::app) struct TerminalRenderLine {
    /// 行文本。
    pub(in crate::app) text: String,
    /// GPUI 文本高亮范围，承载颜色、样式、选区和光标背景。
    pub(in crate::app) highlights: Vec<(Range<usize>, HighlightStyle)>,
    /// 已消费的终端列数。
    ///
    /// 实现原因：
    /// - 中文等全角字符在 alacritty grid 中占两个 cell，其中第二个 cell 是 spacer。
    /// - 渲染文本时不能把 spacer 再画成空格，否则中文后面会多出一列；但尺寸裁剪仍需要按终端列数推进。
    columns_used: usize,
}

impl TerminalRenderLine {
    /// 创建空渲染行。
    fn empty() -> Self {
        Self {
            text: String::new(),
            highlights: Vec::new(),
            columns_used: 0,
        }
    }

    /// 追加单个终端 cell。
    fn push_cell(
        &mut self,
        cell: &Cell,
        selected: bool,
        is_cursor: bool,
        columns: usize,
        theme: EffectiveTheme,
    ) {
        if self.columns_used >= columns {
            return;
        }
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            return;
        }
        if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
            self.columns_used = self.columns_used.saturating_add(1).min(columns);
            return;
        }

        let character = if cell.flags.contains(Flags::HIDDEN) {
            ' '
        } else {
            cell.c
        };
        let cell_width = if cell.flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        let start = self.text.len();
        self.text.push(character);
        if let Some(zerowidth) = cell.zerowidth() {
            for character in zerowidth {
                self.text.push(*character);
            }
        }
        let end = self.text.len();
        self.columns_used = self.columns_used.saturating_add(cell_width).min(columns);

        let style = terminal_cell_highlight_style(cell, selected, is_cursor, theme);
        if style.color.is_some()
            || style.background_color.is_some()
            || style.font_weight.is_some()
            || style.font_style.is_some()
            || style.underline.is_some()
            || style.fade_out.is_some()
        {
            self.highlights.push((start..end, style));
        }
    }
}

/// 将 alacritty 终端 cell 转为 GPUI 高亮样式。
fn terminal_cell_highlight_style(
    cell: &Cell,
    selected: bool,
    is_cursor: bool,
    theme: EffectiveTheme,
) -> HighlightStyle {
    let colors = connection_terminal_default_colors(theme);
    let mut fg = terminal_color_to_rgb(cell.fg, theme);
    let mut bg = terminal_color_to_rgb(cell.bg, theme);
    if cell.flags.contains(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if selected {
        bg = Some(colors.selection_background);
        fg = Some(colors.selection_foreground);
    }
    if is_cursor {
        bg = Some(colors.cursor_background);
        fg = Some(colors.cursor_foreground);
    }

    HighlightStyle {
        color: fg.map(|color| rgb(color).into()),
        font_weight: cell.flags.contains(Flags::BOLD).then_some(FontWeight::BOLD),
        font_style: cell
            .flags
            .contains(Flags::ITALIC)
            .then_some(FontStyle::Italic),
        background_color: bg.map(|color| rgb(color).into()),
        underline: cell
            .flags
            .intersects(Flags::ALL_UNDERLINES)
            .then_some(UnderlineStyle {
                thickness: px(1.0),
                color: fg.map(|color| rgb(color).into()),
                wavy: false,
            }),
        strikethrough: None,
        fade_out: cell.flags.contains(Flags::DIM).then_some(0.55),
    }
}

/// 将 alacritty 颜色映射成 RGB。
///
/// 实现原因：
/// - alacritty 支持命名色、256 色和 truecolor；GPUI 文本高亮使用 Hsla，先统一映射为 24 位 RGB 再交给 `rgb`。
fn terminal_color_to_rgb(color: Color, theme: EffectiveTheme) -> Option<u32> {
    match color {
        Color::Named(named) => terminal_named_color_to_rgb(named, theme),
        Color::Spec(Rgb { r, g, b }) => Some(((r as u32) << 16) | ((g as u32) << 8) | b as u32),
        Color::Indexed(index) => Some(terminal_indexed_color_to_rgb(index, theme)),
    }
}

/// 映射 ANSI 命名色。
fn terminal_named_color_to_rgb(color: NamedColor, theme: EffectiveTheme) -> Option<u32> {
    let defaults = connection_terminal_default_colors(theme);
    match color {
        NamedColor::Foreground => Some(defaults.foreground),
        NamedColor::Background => Some(defaults.background),
        NamedColor::Cursor => Some(defaults.cursor_background),
        NamedColor::Black => Some(match theme {
            EffectiveTheme::Light => 0x24292f,
            EffectiveTheme::Dark => 0x0f172a,
        }),
        NamedColor::Red => Some(match theme {
            EffectiveTheme::Light => 0xcf222e,
            EffectiveTheme::Dark => 0xef4444,
        }),
        NamedColor::Green => Some(match theme {
            EffectiveTheme::Light => 0x116329,
            EffectiveTheme::Dark => 0x22c55e,
        }),
        NamedColor::Yellow => Some(match theme {
            EffectiveTheme::Light => 0x9a6700,
            EffectiveTheme::Dark => 0xeab308,
        }),
        NamedColor::Blue => Some(match theme {
            EffectiveTheme::Light => 0x0969da,
            EffectiveTheme::Dark => 0x3b82f6,
        }),
        NamedColor::Magenta => Some(match theme {
            EffectiveTheme::Light => 0x8250df,
            EffectiveTheme::Dark => 0xd946ef,
        }),
        NamedColor::Cyan => Some(match theme {
            EffectiveTheme::Light => 0x1b7c83,
            EffectiveTheme::Dark => 0x06b6d4,
        }),
        NamedColor::White => Some(match theme {
            EffectiveTheme::Light => 0xf6f8fa,
            EffectiveTheme::Dark => 0xe5e7eb,
        }),
        NamedColor::BrightBlack => Some(match theme {
            EffectiveTheme::Light => 0x57606a,
            EffectiveTheme::Dark => 0x64748b,
        }),
        NamedColor::BrightRed => Some(match theme {
            EffectiveTheme::Light => 0xa40e26,
            EffectiveTheme::Dark => 0xf87171,
        }),
        NamedColor::BrightGreen => Some(match theme {
            EffectiveTheme::Light => 0x1a7f37,
            EffectiveTheme::Dark => 0x4ade80,
        }),
        NamedColor::BrightYellow => Some(match theme {
            EffectiveTheme::Light => 0xbf8700,
            EffectiveTheme::Dark => 0xfacc15,
        }),
        NamedColor::BrightBlue => Some(match theme {
            EffectiveTheme::Light => 0x218bff,
            EffectiveTheme::Dark => 0x60a5fa,
        }),
        NamedColor::BrightMagenta => Some(match theme {
            EffectiveTheme::Light => 0xa475f9,
            EffectiveTheme::Dark => 0xe879f9,
        }),
        NamedColor::BrightCyan => Some(match theme {
            EffectiveTheme::Light => 0x3192aa,
            EffectiveTheme::Dark => 0x22d3ee,
        }),
        NamedColor::BrightWhite => Some(match theme {
            EffectiveTheme::Light => 0xffffff,
            EffectiveTheme::Dark => 0xf8fafc,
        }),
        NamedColor::DimForeground => Some(defaults.muted),
        NamedColor::BrightForeground => Some(match theme {
            EffectiveTheme::Light => 0x000000,
            EffectiveTheme::Dark => 0xffffff,
        }),
        _ => None,
    }
}

/// 映射 xterm 256 色索引。
fn terminal_indexed_color_to_rgb(index: u8, theme: EffectiveTheme) -> u32 {
    const DARK_ANSI: [u32; 16] = [
        0x0f172a, 0xef4444, 0x22c55e, 0xeab308, 0x3b82f6, 0xd946ef, 0x06b6d4, 0xe5e7eb, 0x64748b,
        0xf87171, 0x4ade80, 0xfacc15, 0x60a5fa, 0xe879f9, 0x22d3ee, 0xf8fafc,
    ];
    const LIGHT_ANSI: [u32; 16] = [
        0x24292f, 0xcf222e, 0x116329, 0x9a6700, 0x0969da, 0x8250df, 0x1b7c83, 0xf6f8fa, 0x57606a,
        0xa40e26, 0x1a7f37, 0xbf8700, 0x218bff, 0xa475f9, 0x3192aa, 0xffffff,
    ];
    if index < 16 {
        return match theme {
            EffectiveTheme::Light => LIGHT_ANSI[index as usize],
            EffectiveTheme::Dark => DARK_ANSI[index as usize],
        };
    }

    if index < 232 {
        let value = index - 16;
        let r = value / 36;
        let g = (value % 36) / 6;
        let b = value % 6;
        let component = |part: u8| if part == 0 { 0 } else { 55 + part as u32 * 40 };
        return (component(r) << 16) | (component(g) << 8) | component(b);
    }

    let gray = 8 + (index.saturating_sub(232) as u32) * 10;
    (gray << 16) | (gray << 8) | gray
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证终端输出会进入 alacritty grid。
    #[test]
    fn 终端模拟器可以解析普通输出() {
        let mut emulator = ConnectionTerminalEmulator::new(TerminalSize::new(8, 2, 0, 0));
        emulator.feed_output(b"hello");
        let lines = emulator.render_lines(EffectiveTheme::Dark);
        assert!(lines[0].text.starts_with("hello"));
    }

    /// 验证宽字符 spacer 不会被重复绘制为空格。
    ///
    /// 业务风险：
    /// - alacritty 会用一个真实中文 cell 加一个 spacer cell 表示双列字符；如果 spacer 也渲染成空格，
    ///   中文后续列会视觉错位，看起来像本地终端和 SSH 终端字号不一致。
    #[test]
    fn 终端渲染中文宽字符不会追加多余空格() {
        let mut line = TerminalRenderLine::empty();
        let mut chinese = Cell {
            c: '中',
            ..Cell::default()
        };
        chinese.flags.insert(Flags::WIDE_CHAR);
        let mut spacer = Cell::default();
        spacer.flags.insert(Flags::WIDE_CHAR_SPACER);
        let ascii = Cell {
            c: 'A',
            ..Cell::default()
        };

        line.push_cell(&chinese, false, false, 8, EffectiveTheme::Light);
        line.push_cell(&spacer, false, false, 8, EffectiveTheme::Light);
        line.push_cell(&ascii, false, false, 8, EffectiveTheme::Light);

        assert_eq!(line.text, "中A");
        assert_eq!(line.columns_used, 3);
    }

    /// 验证终端外壳颜色跟随明暗主题切换。
    ///
    /// 业务风险：
    /// - 如果继续硬编码深色背景，亮色主题下连接页右侧会出现突兀的大面积深色终端区域。
    #[test]
    fn 连接终端颜色会适配明暗主题() {
        let light = connection_terminal_ui_colors(
            EffectiveTheme::Light,
            AppThemePalette::for_theme(EffectiveTheme::Light),
        );
        let dark = connection_terminal_ui_colors(
            EffectiveTheme::Dark,
            AppThemePalette::for_theme(EffectiveTheme::Dark),
        );

        assert_eq!(
            light.background,
            AppThemePalette::for_theme(EffectiveTheme::Light).surface
        );
        assert_ne!(light.background, dark.background);
        assert_ne!(light.foreground, dark.foreground);
    }

    /// 验证 bracketed paste 默认关闭，避免默认粘贴被错误包裹。
    #[test]
    fn 终端模拟器默认不开启括号粘贴() {
        let emulator = ConnectionTerminalEmulator::new(TerminalSize::default());
        assert!(!emulator.bracketed_paste_enabled());
    }

    /// 验证连接页左右标题栏高度一致。
    ///
    /// 业务风险：
    /// - 左侧连接工具栏和右侧终端 tab 栏分开渲染，若高度不一致，空态首屏会出现顶部边线错位。
    #[test]
    fn 连接页左右标题栏高度保持一致() {
        assert_eq!(CONNECTIONS_TAB_BAR_HEIGHT, CONNECTIONS_TOOLBAR_HEIGHT);
        assert_eq!(CONNECTIONS_TOOLBAR_HEIGHT, TOOLBAR_HEIGHT);
    }

    /// 验证新建连接菜单同时暴露 SSH 和本地终端类型。
    ///
    /// 业务风险：
    /// - 如果新增按钮再次直接绑定某一种连接，用户将无法从菜单打开无需配置的本地终端。
    #[test]
    fn 新建连接类型菜单保留_ssh_和本地终端入口() {
        assert_eq!(
            CONNECTION_CREATE_KINDS,
            &[
                ConnectionCreateKind::Ssh,
                ConnectionCreateKind::LocalTerminal
            ]
        );

        assert_eq!(ConnectionCreateKind::Ssh.label(), "SSH 连接");
        assert_eq!(
            char::from(ConnectionCreateKind::Ssh.icon()),
            char::from(Icon::Terminal)
        );
        assert_eq!(ConnectionCreateKind::LocalTerminal.label(), "本地终端");
        assert_eq!(
            char::from(ConnectionCreateKind::LocalTerminal.icon()),
            char::from(Icon::SquareTerminal)
        );
    }

    /// 验证终端尺寸会按内容区像素换算为行列。
    ///
    /// 业务风险：
    /// - 如果窗口 resize 后行列仍停在默认 80x24，远端 TUI 和本地渲染会在可视区域内错位。
    #[test]
    fn 终端尺寸会从面板_bounds_计算() {
        let size = connection_terminal_size_from_bounds(Bounds::new(
            point(px(0.0), px(0.0)),
            size(px(805.0), px(365.0)),
        ));

        assert_eq!(size.cols, 100);
        assert_eq!(size.rows, 20);
        assert_eq!(size.pixel_width, 805);
        assert_eq!(size.pixel_height, 365);
    }
}
