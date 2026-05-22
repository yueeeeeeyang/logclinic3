// 连接页 UI 状态和终端模拟器适配。
//
// 业务意图：
// - 本文件只定义 GPUI 会话状态、表单状态、tab 状态和 alacritty_terminal 渲染快照。
// - SQLite、加密和 SSH 网络任务都留在 `crate::connections` 业务域，避免 UI 状态直接承担安全和 IO 细节。

use std::{collections::HashSet, ops::Range, path::PathBuf};

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
pub(in crate::app) const CONNECTION_TERMINAL_PADDING: f32 = 4.0;

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

/// 左侧连接详情悬浮气泡宽度。
///
/// UI 约束：
/// - 连接卡片本身只显示名称，主机和用户信息放到 hover 气泡；固定宽度可以避免长地址导致侧栏布局抖动。
pub(in crate::app) const CONNECTIONS_PROFILE_TOOLTIP_WIDTH: f32 = 224.0;

/// 左侧连接详情悬浮气泡预估高度。
///
/// 边界条件：
/// - GPUI hover 回调阶段无法读取气泡真实布局高度，坐标夹紧使用该值避免靠近窗口底部时被裁切。
pub(in crate::app) const CONNECTIONS_PROFILE_TOOLTIP_HEIGHT: f32 = 72.0;

/// 左侧连接详情悬浮气泡和鼠标点之间的间距。
pub(in crate::app) const CONNECTIONS_PROFILE_TOOLTIP_GAP: f32 = 12.0;

/// 左侧连接树搜索框高度。
///
/// UI 约束：
/// - 搜索框放在连接页标题栏下方，需要保持紧凑，避免挤压分类树首屏。
pub(in crate::app) const CONNECTIONS_TREE_SEARCH_HEIGHT: f32 = 32.0;

/// 左侧连接树单层缩进。
///
/// UI 约束：
/// - 分类和连接共用树形列表，缩进必须稳定，避免搜索和展开/收起时行内容横向跳动。
pub(in crate::app) const CONNECTIONS_TREE_DEPTH_INDENT: f32 = 16.0;

/// 左侧连接树节点名称字号。
///
/// UI 约束：
/// - 分类节点和连接节点必须使用同一个显式字号，不能依赖父级继承，避免不同图标组合或选中态导致视觉大小不一致。
pub(in crate::app) const CONNECTIONS_TREE_NODE_TEXT_SIZE: f32 = 13.0;

/// 左侧连接树节点名称行高。
///
/// UI 约束：
/// - 分类名和连接名共用行高，保证中文、英文和数字混排时基线稳定。
pub(in crate::app) const CONNECTIONS_TREE_NODE_LINE_HEIGHT: f32 = 18.0;

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

/// 根据指定终端单元格宽度计算 PTY 行列和像素尺寸。
///
/// 业务意图：
/// - GPUI 布局完成后才能知道右侧终端真实大小；连接 tab 初始只能使用 80x24，首帧后必须按实际面板同步。
/// - 该计算同时服务 alacritty emulator 和 SSH remote pty，保证渲染行列、鼠标格点和远端 TUI 程序看到的窗口尺寸一致。
/// - 终端渲染字体可能被平台字体回退、字号缩放或不同系统字体实现影响，固定 8px 列宽会让鼠标选区逐列累积偏差。
/// - 渲染层测量到真实等宽字符宽度后，通过该函数同时驱动本地/远程 PTY resize 和鼠标格点换算。
///
/// 边界条件：
/// - 测量失败或异常小时回退到默认列宽，避免除以 0 导致终端尺寸非法。
pub(in crate::app) fn connection_terminal_size_from_bounds_with_cell_width(
    bounds: Bounds<Pixels>,
    cell_width: f32,
) -> TerminalSize {
    let width = f32::from(bounds.size.width).max(0.0);
    let height = f32::from(bounds.size.height).max(0.0);
    let cell_width = cell_width.max(1.0);
    let cols = (width / cell_width).floor().clamp(1.0, u16::MAX as f32) as u16;
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
    /// SMB 连接配置列表，来自独立 SQLite；SMB 不创建终端 tab，点击后直接打开文件管理窗口。
    pub(in crate::app) smb_profiles: Vec<SmbConnectionProfile>,
    /// 连接分类列表，来自独立 SQLite；根层是虚拟节点，不包含在该列表中。
    pub(in crate::app) categories: Vec<ConnectionCategory>,
    /// 数据库初始化或读取错误，中文展示给用户。
    pub(in crate::app) database_error: Option<String>,
    /// 左侧连接树已展开分类 ID 集合；搜索模式会临时展开匹配祖先但不写入该集合。
    pub(in crate::app) expanded_category_ids: HashSet<String>,
    /// 左侧连接名称搜索框状态；只过滤连接名称，不匹配分类名称。
    pub(in crate::app) tree_search: ConnectionTreeSearchState,
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
    /// - 顶部新增按钮不直接假定只有 SSH，而是先打开类型菜单；当前菜单包含 SSH、SMB 和分类入口，后续可追加其它协议。
    /// - 菜单状态只属于左侧连接栏，关闭或选择后必须立即复位，避免遮挡连接列表点击。
    pub(in crate::app) create_menu_open: bool,
    /// 左侧连接行右键菜单。
    ///
    /// UI 约束：
    /// - 连接卡片本身只负责单击直连，编辑和删除等低频动作统一收进右键菜单，避免卡片内按钮互相抢占点击区域。
    /// - 菜单坐标保存为左侧连接栏内部坐标；鼠标事件进入时需要扣除主导航宽度并限制在侧栏范围内。
    pub(in crate::app) profile_context_menu: Option<ConnectionProfileContextMenu>,
    /// 左侧连接行悬浮详情气泡。
    ///
    /// UI 约束：
    /// - 连接卡片只保留名称，用户和主机信息在 hover 时展示，避免列表在窄侧栏里显得拥挤。
    /// - 气泡是纯展示状态，不持久化；连接树刷新或菜单打开后可以直接丢弃。
    pub(in crate::app) profile_hover_tooltip: Option<ConnectionProfileHoverTooltip>,
    /// 左侧分类行右键菜单。
    ///
    /// UI 约束：
    /// - 分类菜单和连接菜单共享左侧浮层层级，任一菜单打开时必须关闭另一种菜单，避免点击目标不明确。
    pub(in crate::app) category_context_menu: Option<ConnectionCategoryContextMenu>,
    /// 终端 tab 右键菜单。
    ///
    /// UI 约束：
    /// - 菜单绘制在右侧终端工作区内部，坐标使用工作区局部坐标，避免受左侧栏宽度变化影响。
    pub(in crate::app) tab_context_menu: Option<ConnectionTabContextMenu>,
    /// 终端正文右键菜单。
    ///
    /// UI 约束：
    /// - 菜单只在终端正文区域打开，坐标同样使用右侧工作区局部坐标。
    /// - 打开时必须关闭 tab 菜单和左侧菜单，避免同一次右键产生多个浮层竞争输入。
    pub(in crate::app) terminal_context_menu: Option<ConnectionTerminalContextMenu>,
    /// 新增/编辑连接弹窗状态。
    pub(in crate::app) dialog: Option<ConnectionDialogState>,
    /// 新增/编辑 SMB 连接弹窗状态。
    pub(in crate::app) smb_dialog: Option<SmbConnectionDialogState>,
    /// 新增/编辑分类弹窗状态。
    pub(in crate::app) category_dialog: Option<ConnectionCategoryDialogState>,
    /// 删除连接确认弹窗状态。
    pub(in crate::app) delete_confirm_dialog: Option<ConnectionDeleteConfirmDialog>,
    /// 删除分类确认弹窗状态。
    pub(in crate::app) category_delete_confirm_dialog:
        Option<ConnectionCategoryDeleteConfirmDialog>,
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
    pub(in crate::app) fn load_or_initialize(context: &mut Context<MainView>) -> Self {
        let database_path = connections_database_path();
        let (profiles, smb_profiles, categories, database_error) = match database_path.as_ref() {
            Some(path) => match (
                load_connection_profiles(path),
                load_smb_connection_profiles(path),
                load_connection_categories(path),
            ) {
                (Ok(profiles), Ok(smb_profiles), Ok(categories)) => {
                    (profiles, smb_profiles, categories, None)
                }
                (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                    (Vec::new(), Vec::new(), Vec::new(), Some(error))
                }
            },
            None => (
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Some("无法定位应用配置目录，连接配置不会被加载".to_string()),
            ),
        };
        Self {
            database_path,
            profiles,
            smb_profiles,
            categories,
            database_error,
            expanded_category_ids: HashSet::new(),
            tree_search: ConnectionTreeSearchState::new(context),
            tree_width: CONNECTIONS_TREE_DEFAULT_WIDTH,
            tree_resize_drag: None,
            next_tab_id: 1,
            active_tab_id: None,
            tabs: Vec::new(),
            tab_bar_scroll_handle: ScrollHandle::new(),
            create_menu_open: false,
            profile_context_menu: None,
            profile_hover_tooltip: None,
            category_context_menu: None,
            tab_context_menu: None,
            terminal_context_menu: None,
            dialog: None,
            smb_dialog: None,
            category_dialog: None,
            delete_confirm_dialog: None,
            category_delete_confirm_dialog: None,
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

    /// 根据 SMB 连接 ID 查找配置。
    pub(in crate::app) fn smb_profile_by_id(
        &self,
        profile_id: &str,
    ) -> Option<&SmbConnectionProfile> {
        self.smb_profiles
            .iter()
            .find(|profile| profile.id == profile_id)
    }

    /// 根据分类 ID 查找分类。
    pub(in crate::app) fn category_by_id(&self, category_id: &str) -> Option<&ConnectionCategory> {
        self.categories
            .iter()
            .find(|category| category.id == category_id)
    }

    /// 生成连接 hover 气泡展示的用户文案。
    ///
    /// 业务意图：
    /// - 连接卡片主区域只显示名称；悬浮信息统一由这里格式化，避免列表渲染和测试各自拼接。
    pub(in crate::app) fn profile_tooltip_user_label(profile: &ConnectionProfile) -> String {
        format!("用户：{}", profile.username)
    }

    /// 生成连接 hover 气泡展示的地址文案。
    ///
    /// 边界条件：
    /// - host 可能是 IP、域名或内网别名；这里只展示用户保存的原始主机和端口，不做解析或脱敏。
    pub(in crate::app) fn profile_tooltip_address_label(profile: &ConnectionProfile) -> String {
        format!("地址：{}:{}", profile.host, profile.port)
    }

    /// 生成 SMB 连接 hover 气泡展示的用户文案。
    pub(in crate::app) fn smb_profile_tooltip_user_label(profile: &SmbConnectionProfile) -> String {
        format!("用户：{}", profile.username)
    }

    /// 生成 SMB 连接 hover 气泡展示的地址文案。
    pub(in crate::app) fn smb_profile_tooltip_address_label(
        profile: &SmbConnectionProfile,
    ) -> String {
        format!("地址：{}:{}/{}", profile.host, profile.port, profile.share)
    }

    /// 重新加载 SQLite 中的连接和分类列表。
    pub(in crate::app) fn reload_tree_data(&mut self) {
        let Some(path) = self.database_path.as_ref() else {
            self.database_error = Some("无法定位应用配置目录，连接配置不会被加载".to_string());
            self.profiles.clear();
            self.smb_profiles.clear();
            self.categories.clear();
            return;
        };

        match (
            load_connection_profiles(path),
            load_smb_connection_profiles(path),
            load_connection_categories(path),
        ) {
            (Ok(profiles), Ok(smb_profiles), Ok(categories)) => {
                self.database_error = None;
                self.profiles = profiles;
                self.smb_profiles = smb_profiles;
                self.categories = categories;
                self.expanded_category_ids.retain(|category_id| {
                    self.categories
                        .iter()
                        .any(|category| &category.id == category_id)
                });
            }
            (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                self.database_error = Some(error);
            }
        }
    }

    /// 兼容旧调用点的连接列表刷新入口。
    pub(in crate::app) fn reload_profiles(&mut self) {
        self.reload_tree_data();
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
            if self.active_tab_id == Some(tab_id) {
                self.active_tab_id = self
                    .tabs
                    .get(index)
                    .or_else(|| {
                        index
                            .checked_sub(1)
                            .and_then(|previous| self.tabs.get(previous))
                    })
                    .map(|tab| tab.id);
            }
        }
        self.tab_context_menu = None;
        self.terminal_context_menu = None;
    }

    /// 关闭指定 tab 之外的所有终端 tab。
    pub(in crate::app) fn close_other_tabs(&mut self, tab_id: usize) {
        for tab in self.tabs.iter().filter(|tab| tab.id != tab_id) {
            tab.backend.shutdown();
        }
        self.tabs.retain(|tab| tab.id == tab_id);
        self.active_tab_id = self.tabs.first().map(|tab| tab.id);
        self.tab_context_menu = None;
        self.terminal_context_menu = None;
        self.tab_bar_scroll_handle
            .set_offset(point(px(0.0), px(0.0)));
    }

    /// 关闭全部终端 tab。
    pub(in crate::app) fn close_all_tabs(&mut self) {
        for tab in &self.tabs {
            tab.backend.shutdown();
        }
        self.tabs.clear();
        self.active_tab_id = None;
        self.tab_context_menu = None;
        self.terminal_context_menu = None;
        self.tab_bar_scroll_handle
            .set_offset(point(px(0.0), px(0.0)));
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

/// 左侧连接树搜索输入状态。
///
/// 业务意图：
/// - 搜索框只影响连接树当前展示，不写入 SQLite，不影响分类展开持久状态。
/// - 使用通用单行输入框状态，让中文 IME、复制粘贴、拖拽选区和水平滚动与其它输入框保持一致。
pub(in crate::app) struct ConnectionTreeSearchState {
    /// 单行输入框通用编辑状态。
    pub(in crate::app) input: SingleLineTextInputState,
    /// 搜索框焦点句柄。
    pub(in crate::app) focus: FocusHandle,
    /// 最近一次绘制的字形布局，用于鼠标命中和 IME 候选窗口定位。
    pub(in crate::app) last_layout: Option<ShapedLine>,
    /// 最近一次绘制的输入框窗口坐标。
    pub(in crate::app) last_bounds: Option<Bounds<Pixels>>,
}

impl ConnectionTreeSearchState {
    /// 创建连接树搜索输入状态。
    pub(in crate::app) fn new(context: &mut Context<MainView>) -> Self {
        Self {
            input: SingleLineTextInputState::empty(),
            focus: context.focus_handle(),
            last_layout: None,
            last_bounds: None,
        }
    }

    /// 判断当前是否存在有效搜索关键字。
    pub(in crate::app) fn query(&self) -> String {
        self.input.text.trim().to_lowercase()
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

/// 左侧连接树中的连接类型。
///
/// 业务意图：
/// - SSH 点击后打开终端 tab，SMB 点击后打开文件管理窗口；树行、右键菜单和选中态必须携带协议类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionProfileKind {
    /// SSH 连接。
    Ssh,
    /// SMB 文件共享连接。
    Smb,
}

/// 左侧连接树连接 key。
///
/// 业务意图：
/// - 不同协议写入不同 SQLite 表，裸 ID 不能单独作为 UI 动作目标；key 使用稳定文本便于 existing GPUI 状态直接保存。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct ConnectionProfileKey;

impl ConnectionProfileKey {
    /// 构造 SSH key。
    pub(in crate::app) fn ssh(id: &str) -> String {
        format!("ssh:{id}")
    }

    /// 构造 SMB key。
    pub(in crate::app) fn smb(id: &str) -> String {
        format!("smb:{id}")
    }

    /// 解析连接 key。
    pub(in crate::app) fn parse(key: &str) -> Option<(ConnectionProfileKind, &str)> {
        if let Some(id) = key.strip_prefix("ssh:") {
            Some((ConnectionProfileKind::Ssh, id))
        } else {
            key.strip_prefix("smb:")
                .map(|id| (ConnectionProfileKind::Smb, id))
        }
    }
}

/// 判断连接 key 是否仍然存在。
pub(super) fn connection_profile_key_exists(
    key: &str,
    profiles: &[ConnectionProfile],
    smb_profiles: &[SmbConnectionProfile],
) -> bool {
    match ConnectionProfileKey::parse(key) {
        Some((ConnectionProfileKind::Ssh, id)) => profiles.iter().any(|profile| profile.id == id),
        Some((ConnectionProfileKind::Smb, id)) => {
            smb_profiles.iter().any(|profile| profile.id == id)
        }
        None => false,
    }
}

/// 新建连接菜单中的连接类型。
///
/// 业务意图：
/// - 新增入口从一开始按类型建模；连接页只管理需要保存配置的远程/文件共享连接和分类。
/// - 本地终端已经拆到独立“终端”导航页，不再作为连接类型出现，避免把本机 shell 生命周期混入连接树。
/// - 后续扩展串口、跳板机或其它协议时，只需要追加类型和分发动作，避免重写新增菜单交互。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionCreateKind {
    /// SSH 远程连接。
    Ssh,
    /// SMB 文件共享连接。
    Smb,
    /// 新建连接分类。
    Category,
}

/// 新建连接菜单当前展示的连接类型顺序。
///
/// UI 约束：
/// - SSH 放在第一项，保持已有用户路径不变；SMB 紧随其后，分类入口放在末尾。
pub(in crate::app) const CONNECTION_CREATE_KINDS: &[ConnectionCreateKind] = &[
    ConnectionCreateKind::Ssh,
    ConnectionCreateKind::Smb,
    ConnectionCreateKind::Category,
];

impl ConnectionCreateKind {
    /// 返回菜单展示文案。
    pub(in crate::app) const fn label(self) -> &'static str {
        match self {
            Self::Ssh => "SSH 连接",
            Self::Smb => "SMB 连接",
            Self::Category => "新建分类",
        }
    }

    /// 返回菜单图标。
    pub(in crate::app) const fn icon(self) -> Icon {
        match self {
            Self::Ssh => Icon::Terminal,
            Self::Smb => Icon::HardDrive,
            Self::Category => Icon::FolderPlus,
        }
    }
}

/// 左侧连接行右键菜单状态。
pub(in crate::app) struct ConnectionProfileContextMenu {
    /// 菜单目标连接 key。
    pub(in crate::app) profile_id: String,
    /// 菜单在连接侧栏内部的横坐标。
    pub(in crate::app) x: f32,
    /// 菜单在连接侧栏内部的纵坐标。
    pub(in crate::app) y: f32,
}

/// 左侧连接详情悬浮气泡状态。
///
/// 业务意图：
/// - 用户需要快速确认连接的用户名和主机地址，但这些信息不应长期占用连接列表空间。
/// - 坐标保存在左侧栏局部坐标中，渲染时可以直接作为侧栏绝对定位浮层使用。
pub(in crate::app) struct ConnectionProfileHoverTooltip {
    /// 悬浮目标连接 key。
    pub(in crate::app) profile_id: String,
    /// 气泡在连接侧栏内部的横坐标。
    pub(in crate::app) x: f32,
    /// 气泡在连接侧栏内部的纵坐标。
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

/// 左侧分类行右键菜单状态。
pub(in crate::app) struct ConnectionCategoryContextMenu {
    /// 菜单目标分类 ID。
    pub(in crate::app) category_id: String,
    /// 菜单在连接侧栏内部的横坐标。
    pub(in crate::app) x: f32,
    /// 菜单在连接侧栏内部的纵坐标。
    pub(in crate::app) y: f32,
}

/// 左侧分类行右键菜单动作。
///
/// 业务意图：
/// - 分类行主点击用于展开/收起，管理动作统一放到右键菜单，避免和连接行的单击直连语义冲突。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionCategoryContextMenuAction {
    /// 在当前分类下新建子分类。
    CreateChild,
    /// 重命名分类。
    Edit,
    /// 删除空分类。
    Delete,
}

/// 连接终端 tab 右键菜单状态。
pub(in crate::app) struct ConnectionTabContextMenu {
    /// 菜单目标 tab ID。
    pub(in crate::app) tab_id: usize,
    /// 菜单在右侧终端工作区内部的横坐标。
    pub(in crate::app) x: f32,
    /// 菜单在右侧终端工作区内部的纵坐标。
    pub(in crate::app) y: f32,
}

/// 连接终端 tab 右键菜单动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionTabContextMenuAction {
    /// 关闭右键点击的 tab。
    Current,
    /// 关闭右键点击 tab 之外的其它 tab。
    OtherTabs,
    /// 关闭全部连接终端 tab。
    AllTabs,
}

/// 终端正文右键菜单状态。
pub(in crate::app) struct ConnectionTerminalContextMenu {
    /// 菜单目标 tab ID。
    pub(in crate::app) tab_id: usize,
    /// 菜单在右侧终端工作区内部的横坐标。
    pub(in crate::app) x: f32,
    /// 菜单在右侧终端工作区内部的纵坐标。
    pub(in crate::app) y: f32,
}

/// 终端正文右键菜单动作。
///
/// 业务意图：
/// - 终端正文的右键菜单承载和当前终端内容相关的动作；复制/粘贴复用现有终端选区和 bracketed paste 逻辑，
///   文件管理则从当前 tab 的 OSC 7 目录打开独立窗口。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionTerminalContextMenuAction {
    /// 打开文件管理窗口。
    FileManager,
    /// 复制当前终端选区。
    Copy,
    /// 粘贴系统剪贴板内容。
    Paste,
}

/// 终端 tab 对应的文件管理目标类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionTerminalFileTargetKind {
    /// 本地终端，文件管理走本机文件系统。
    Local,
    /// SSH 终端，文件管理走独立 SFTP 会话。
    Ssh,
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
    /// 当前选择的分类 ID；`None` 表示根层无分类。
    pub(in crate::app) category_id: Option<String>,
    /// 分类 Select 是否展开。
    pub(in crate::app) category_select_open: bool,
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
            category_id: None,
            category_select_open: false,
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
            category_id: profile.category_id.clone(),
            category_select_open: false,
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
            category_id: self.category_id.clone(),
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

/// 新增/编辑 SMB 连接弹窗状态。
pub(in crate::app) struct SmbConnectionDialogState {
    /// 弹窗模式。
    pub(in crate::app) mode: ConnectionDialogMode,
    /// 表单错误消息。
    pub(in crate::app) error: Option<String>,
    /// 名称输入状态。
    pub(in crate::app) name: ConnectionFormTextFieldState,
    /// SMB 地址输入状态。
    pub(in crate::app) address: ConnectionFormTextFieldState,
    /// 用户名输入状态。
    pub(in crate::app) username: ConnectionFormTextFieldState,
    /// 当前选择的分类 ID；`None` 表示根层无分类。
    pub(in crate::app) category_id: Option<String>,
    /// 分类 Select 是否展开。
    pub(in crate::app) category_select_open: bool,
    /// 密码输入状态；编辑时为空表示不修改旧密码。
    pub(in crate::app) password: ConnectionFormTextFieldState,
}

impl SmbConnectionDialogState {
    /// 创建新增 SMB 连接弹窗。
    pub(in crate::app) fn create(context: &mut Context<MainView>) -> Self {
        Self {
            mode: ConnectionDialogMode::Create,
            error: None,
            name: ConnectionFormTextFieldState::new(String::new(), context),
            address: ConnectionFormTextFieldState::new(String::new(), context),
            username: ConnectionFormTextFieldState::new(String::new(), context),
            category_id: None,
            category_select_open: false,
            password: ConnectionFormTextFieldState::new(String::new(), context),
        }
    }

    /// 创建编辑 SMB 连接弹窗。
    pub(in crate::app) fn edit(
        profile: &SmbConnectionProfile,
        context: &mut Context<MainView>,
    ) -> Self {
        let address = ParsedSmbAddress {
            host: profile.host.clone(),
            port: profile.port,
            share: profile.share.clone(),
            initial_path: profile.initial_path.clone(),
        }
        .to_address_text();
        Self {
            mode: ConnectionDialogMode::Edit {
                profile_id: profile.id.clone(),
            },
            error: None,
            name: ConnectionFormTextFieldState::new(profile.name.clone(), context),
            address: ConnectionFormTextFieldState::new(address, context),
            username: ConnectionFormTextFieldState::new(profile.username.clone(), context),
            category_id: profile.category_id.clone(),
            category_select_open: false,
            password: ConnectionFormTextFieldState::new(String::new(), context),
        }
    }

    /// 生成保存校验使用的纯 SMB 连接草稿。
    pub(in crate::app) fn to_profile_draft(&self) -> SmbConnectionProfileDraft {
        SmbConnectionProfileDraft {
            name: self.name.input.text.clone(),
            address: self.address.input.text.clone(),
            username: self.username.input.text.clone(),
            category_id: self.category_id.clone(),
            password: self.password.input.text.clone(),
        }
    }

    /// 返回指定 SMB 字段的输入状态。
    pub(in crate::app) fn field(
        &self,
        field: SmbConnectionFormField,
    ) -> &ConnectionFormTextFieldState {
        match field {
            SmbConnectionFormField::Name => &self.name,
            SmbConnectionFormField::Address => &self.address,
            SmbConnectionFormField::Username => &self.username,
            SmbConnectionFormField::Password => &self.password,
        }
    }

    /// 返回指定 SMB 字段的可变输入状态。
    pub(in crate::app) fn field_mut(
        &mut self,
        field: SmbConnectionFormField,
    ) -> &mut ConnectionFormTextFieldState {
        match field {
            SmbConnectionFormField::Name => &mut self.name,
            SmbConnectionFormField::Address => &mut self.address,
            SmbConnectionFormField::Username => &mut self.username,
            SmbConnectionFormField::Password => &mut self.password,
        }
    }
}

impl Drop for SmbConnectionDialogState {
    /// 弹窗销毁时清理 SMB 密码明文。
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

/// SMB 连接表单字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum SmbConnectionFormField {
    /// 名称。
    Name,
    /// 地址。
    Address,
    /// 用户名。
    Username,
    /// 密码。
    Password,
}

/// 新增/编辑分类弹窗状态。
pub(in crate::app) struct ConnectionCategoryDialogState {
    /// 弹窗模式。
    pub(in crate::app) mode: ConnectionCategoryDialogMode,
    /// 表单错误消息。
    pub(in crate::app) error: Option<String>,
    /// 分类名称输入状态。
    pub(in crate::app) name: ConnectionFormTextFieldState,
}

impl ConnectionCategoryDialogState {
    /// 创建新增分类弹窗。
    pub(in crate::app) fn create(
        parent_id: Option<String>,
        context: &mut Context<MainView>,
    ) -> Self {
        Self {
            mode: ConnectionCategoryDialogMode::Create { parent_id },
            error: None,
            name: ConnectionFormTextFieldState::new(String::new(), context),
        }
    }

    /// 创建编辑分类弹窗。
    pub(in crate::app) fn edit(
        category: &ConnectionCategory,
        context: &mut Context<MainView>,
    ) -> Self {
        Self {
            mode: ConnectionCategoryDialogMode::Edit {
                category_id: category.id.clone(),
            },
            error: None,
            name: ConnectionFormTextFieldState::new(category.name.clone(), context),
        }
    }

    /// 生成保存校验使用的纯分类草稿。
    pub(in crate::app) fn to_category_draft(&self) -> ConnectionCategoryDraft {
        ConnectionCategoryDraft {
            name: self.name.input.text.clone(),
        }
    }
}

/// 分类弹窗模式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionCategoryDialogMode {
    /// 新增分类，`parent_id=None` 表示根层分类。
    Create {
        /// 父分类 ID。
        parent_id: Option<String>,
    },
    /// 编辑已有分类名称。
    Edit {
        /// 正在编辑的分类 ID。
        category_id: String,
    },
}

/// 删除连接确认弹窗。
pub(in crate::app) struct ConnectionDeleteConfirmDialog {
    /// 待删除连接 key。
    pub(in crate::app) profile_id: String,
    /// 待删除连接名称，用于确认文案。
    pub(in crate::app) profile_name: String,
}

/// 删除分类确认弹窗。
pub(in crate::app) struct ConnectionCategoryDeleteConfirmDialog {
    /// 待删除分类 ID。
    pub(in crate::app) category_id: String,
    /// 待删除分类名称，用于确认文案。
    pub(in crate::app) category_name: String,
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

/// 左侧连接树派生行。
///
/// 业务意图：
/// - 分类展开、搜索过滤和连接排序都属于状态推导，先生成结构化行再交给 GPUI 渲染，便于纯单元测试覆盖。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionTreeRow {
    /// 分类行。
    Category {
        /// 分类快照。
        category: ConnectionCategory,
        /// 树形缩进层级。
        depth: usize,
        /// 当前是否展开；搜索模式下匹配祖先会被临时视为展开。
        expanded: bool,
    },
    /// 连接行。
    Profile {
        /// 连接快照。
        profile: ConnectionTreeProfile,
        /// 树形缩进层级。
        depth: usize,
    },
}

/// 左侧连接树中的连接快照。
///
/// 业务意图：
/// - 树构建和渲染需要统一处理 SSH/SMB 名称、分类、排序和搜索，同时保留具体协议类型用于点击动作分发。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum ConnectionTreeProfile {
    /// SSH 连接。
    Ssh(ConnectionProfile),
    /// SMB 连接。
    Smb(SmbConnectionProfile),
}

impl ConnectionTreeProfile {
    /// 返回连接 key。
    pub(in crate::app) fn key(&self) -> String {
        match self {
            Self::Ssh(profile) => ConnectionProfileKey::ssh(&profile.id),
            Self::Smb(profile) => ConnectionProfileKey::smb(&profile.id),
        }
    }

    /// 返回裸连接 ID。
    pub(in crate::app) fn id(&self) -> &str {
        match self {
            Self::Ssh(profile) => &profile.id,
            Self::Smb(profile) => &profile.id,
        }
    }

    /// 返回连接名称。
    pub(in crate::app) fn name(&self) -> &str {
        match self {
            Self::Ssh(profile) => &profile.name,
            Self::Smb(profile) => &profile.name,
        }
    }

    /// 返回连接所属分类。
    fn category_id(&self) -> Option<&str> {
        match self {
            Self::Ssh(profile) => profile.category_id.as_deref(),
            Self::Smb(profile) => profile.category_id.as_deref(),
        }
    }

    /// 返回更新时间，供同一分类下混合协议排序。
    fn updated_at_ms(&self) -> i64 {
        match self {
            Self::Ssh(profile) => profile.updated_at_ms,
            Self::Smb(profile) => profile.updated_at_ms,
        }
    }

    /// 返回连接图标。
    pub(in crate::app) fn icon(&self) -> Icon {
        match self {
            Self::Ssh(_) => Icon::Terminal,
            Self::Smb(_) => Icon::HardDrive,
        }
    }
}

/// 根据分类、连接、展开状态和搜索词生成左侧连接树行。
///
/// 业务意图：
/// - 搜索只匹配连接名称；匹配连接的祖先分类需要保留，帮助用户理解结果所在路径。
/// - 普通模式按分类创建时间稳定排序，同一分类下连接沿用存储层的更新时间倒序。
///
/// 边界条件：
/// - 数据库中如果存在父分类缺失的孤儿分类，按根层分类展示，避免用户看不到可恢复的数据。
/// - 分类父级如果意外形成循环，递归会通过 visited 集合停止，避免 UI 渲染卡死。
pub(in crate::app) fn build_connection_tree_rows(
    categories: &[ConnectionCategory],
    profiles: &[ConnectionProfile],
    smb_profiles: &[SmbConnectionProfile],
    expanded_category_ids: &HashSet<String>,
    search_query: &str,
) -> Vec<ConnectionTreeRow> {
    let normalized_query = search_query.trim().to_lowercase();
    let searching = !normalized_query.is_empty();
    let mut all_profiles = profiles
        .iter()
        .cloned()
        .map(ConnectionTreeProfile::Ssh)
        .chain(smb_profiles.iter().cloned().map(ConnectionTreeProfile::Smb))
        .collect::<Vec<_>>();
    all_profiles.sort_by(|left, right| {
        right
            .updated_at_ms()
            .cmp(&left.updated_at_ms())
            .then_with(|| left.name().cmp(right.name()))
    });
    let mut rows = Vec::new();
    let mut visited = HashSet::new();

    build_connection_tree_rows_for_parent(
        None,
        0,
        categories,
        &all_profiles,
        expanded_category_ids,
        searching,
        &normalized_query,
        &mut visited,
        &mut rows,
    );
    rows
}

fn build_connection_tree_rows_for_parent(
    parent_id: Option<&str>,
    depth: usize,
    categories: &[ConnectionCategory],
    profiles: &[ConnectionTreeProfile],
    expanded_category_ids: &HashSet<String>,
    searching: bool,
    normalized_query: &str,
    visited: &mut HashSet<String>,
    rows: &mut Vec<ConnectionTreeRow>,
) {
    for category in categories.iter().filter(|category| {
        connection_category_visible_under_parent(category, parent_id, categories)
    }) {
        if !visited.insert(category.id.clone()) {
            continue;
        }
        let has_match = !searching
            || connection_category_has_matching_profile(
                categories,
                profiles,
                &category.id,
                normalized_query,
                &mut HashSet::new(),
            );
        if has_match {
            let expanded = searching || expanded_category_ids.contains(&category.id);
            rows.push(ConnectionTreeRow::Category {
                category: category.clone(),
                depth,
                expanded,
            });
            if expanded {
                build_connection_tree_rows_for_parent(
                    Some(&category.id),
                    depth + 1,
                    categories,
                    profiles,
                    expanded_category_ids,
                    searching,
                    normalized_query,
                    visited,
                    rows,
                );
            }
        }
    }

    for profile in profiles.iter().filter(|profile| {
        connection_profile_visible_under_parent(profile, parent_id, categories)
            && (!searching || profile.name().to_lowercase().contains(normalized_query))
    }) {
        rows.push(ConnectionTreeRow::Profile {
            profile: profile.clone(),
            depth,
        });
    }
}

fn connection_category_visible_under_parent(
    category: &ConnectionCategory,
    parent_id: Option<&str>,
    categories: &[ConnectionCategory],
) -> bool {
    match (category.parent_id.as_deref(), parent_id) {
        (None, None) => true,
        (Some(parent), Some(expected)) => parent == expected,
        (Some(parent), None) => !categories.iter().any(|category| category.id == parent),
        _ => false,
    }
}

fn connection_profile_visible_under_parent(
    profile: &ConnectionTreeProfile,
    parent_id: Option<&str>,
    categories: &[ConnectionCategory],
) -> bool {
    match (profile.category_id(), parent_id) {
        (None, None) => true,
        (Some(category_id), Some(expected)) => category_id == expected,
        (Some(category_id), None) => !categories.iter().any(|category| category.id == category_id),
        _ => false,
    }
}

fn connection_category_has_matching_profile(
    categories: &[ConnectionCategory],
    profiles: &[ConnectionTreeProfile],
    category_id: &str,
    normalized_query: &str,
    visited: &mut HashSet<String>,
) -> bool {
    if !visited.insert(category_id.to_string()) {
        return false;
    }
    profiles.iter().any(|profile| {
        profile.category_id() == Some(category_id)
            && profile.name().to_lowercase().contains(normalized_query)
    }) || categories.iter().any(|category| {
        category.parent_id.as_deref() == Some(category_id)
            && connection_category_has_matching_profile(
                categories,
                profiles,
                &category.id,
                normalized_query,
                visited,
            )
    })
}

/// 终端 tab 状态。
pub(in crate::app) struct ConnectionTerminalTab {
    /// tab 内部 ID，每次打开连接递增。
    pub(in crate::app) id: usize,
    /// 连接配置 ID。
    pub(in crate::app) profile_id: String,
    /// 文件管理目标类型。
    ///
    /// 业务意图：
    /// - SSH 连接页和独立本地终端页共用同一套终端 tab 渲染；文件管理打开时需要明确选择 SFTP 或本机文件系统。
    pub(in crate::app) file_target_kind: ConnectionTerminalFileTargetKind,
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
    /// 当前 tab 渲染使用的真实终端列宽。
    ///
    /// 业务意图：
    /// - GPUI 文本系统会根据平台字体实际测量字符宽度，不能长期使用经验常量做鼠标选区和 pty resize。
    /// - 独立本地终端和 SSH 终端共用该值，避免不同后端打开后选区位置一前一后。
    pub(in crate::app) cell_width: f32,
    /// 终端内容区最近一次 GPUI 实际 bounds。
    ///
    /// 业务意图：
    /// - 鼠标选区、xterm 鼠标上报和 IME 候选窗口都必须使用真实内容区坐标，而不能用左侧栏宽度、
    ///   tab 栏高度等常量反推，否则在 resize、边框和平台标题栏差异下会出现点击位置错位。
    pub(in crate::app) content_bounds: Option<Bounds<Pixels>>,
    /// 终端中文输入法组合状态。
    ///
    /// 业务意图：
    /// - IME 组合文本不能直接写入 PTY/SSH；只有平台提交最终文本时才发送 UTF-8 字节到后端。
    /// - 该状态只服务当前 tab 的候选窗口定位和组合文本预览，终端真实内容仍由后端回显驱动。
    pub(in crate::app) ime: ConnectionTerminalImeState,
    /// OSC 7 解析缓存。
    ///
    /// 业务意图：
    /// - 当前目录上报序列可能跨后端输出 chunk；这里保留尚未闭合的片段，只用于解析 OSC 7，不进入终端显示。
    pub(in crate::app) osc7_buffer: Vec<u8>,
    /// 最近一次由 OSC 7 上报的当前目录。
    ///
    /// 边界条件：
    /// - 不是所有 shell 都启用 OSC 7；为空时文件管理窗口回退到本地 home 或 SSH `~`，并允许用户在路径栏手动切换。
    pub(in crate::app) last_reported_cwd: Option<String>,
    /// 后端事件通道是否已经结束。
    ///
    /// 业务意图：
    /// - 终端 tab 可以保留退出状态供用户查看，但后台轮询必须停止读取已经断开的通道，避免持续 30ms 刷新 UI。
    pub(in crate::app) backend_finished: bool,
}

/// 终端 IME 组合状态。
///
/// 业务意图：
/// - 自绘终端不是传统文本框，但平台输入法仍需要“当前文本、选区、marked range、候选窗口位置”
///   这组最小协议来完成中文输入。
/// - 这里只保存尚未提交到 shell 的组合文本；最终提交后立即清空，避免下次输入把旧拼音重复发送。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct ConnectionTerminalImeState {
    /// 当前组合文本，通常是拼音或输入法临时候选内容。
    pub(in crate::app) text: String,
    /// 平台标记的组合文本范围，按 UTF-8 字节下标保存。
    pub(in crate::app) marked_range: Option<Range<usize>>,
    /// 组合文本内部选择范围，按 UTF-8 字节下标保存。
    pub(in crate::app) selection_range: Range<usize>,
}

impl Default for ConnectionTerminalImeState {
    fn default() -> Self {
        Self {
            text: String::new(),
            marked_range: None,
            selection_range: 0..0,
        }
    }
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

    /// 清空终端 IME 组合状态。
    pub(in crate::app) fn clear_ime(&mut self) {
        self.ime = ConnectionTerminalImeState::default();
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
    pub(in crate::app) fn point_from_panel_offset(&self, x: f32, y: f32, cell_width: f32) -> Point {
        let cell_width = cell_width.max(1.0);
        let column = (x / cell_width)
            .floor()
            .max(0.0)
            .min((self.size.cols.saturating_sub(1)) as f32) as usize;
        let line = (y / CONNECTION_TERMINAL_ROW_HEIGHT)
            .floor()
            .max(0.0)
            .min((self.size.rows.saturating_sub(1)) as f32) as i32;
        Point::new(Line(line), Column(column))
    }

    /// 返回当前终端光标格点。
    ///
    /// 业务意图：
    /// - IME 候选窗口需要跟随 shell 光标，而不是跟随整个终端面板左上角。
    /// - 这里直接读取 alacritty 当前 grid cursor，保证远端程序移动光标后候选框仍出现在正确行列附近。
    pub(in crate::app) fn cursor_point(&self) -> Point {
        self.term.grid().cursor.point
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

    /// 构造连接树测试分类。
    fn test_category(id: &str, parent_id: Option<&str>, created_at_ms: i64) -> ConnectionCategory {
        ConnectionCategory {
            id: id.to_string(),
            parent_id: parent_id.map(ToString::to_string),
            name: id.to_string(),
            created_at_ms,
            updated_at_ms: created_at_ms,
        }
    }

    /// 构造连接树测试连接。
    fn test_profile(id: &str, name: &str, category_id: Option<&str>) -> ConnectionProfile {
        ConnectionProfile {
            id: id.to_string(),
            name: name.to_string(),
            host: "127.0.0.1".to_string(),
            port: 22,
            username: "root".to_string(),
            category_id: category_id.map(ToString::to_string),
            encrypted_password: "v1:nonce:cipher".to_string(),
            host_key_fingerprint: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            last_connected_at_ms: None,
        }
    }

    /// 构造连接树测试 SMB 连接。
    fn test_smb_profile(id: &str, name: &str, category_id: Option<&str>) -> SmbConnectionProfile {
        SmbConnectionProfile {
            id: id.to_string(),
            name: name.to_string(),
            host: "fileserver".to_string(),
            port: DEFAULT_SMB_PORT,
            share: "logs".to_string(),
            initial_path: "/".to_string(),
            username: "root".to_string(),
            category_id: category_id.map(ToString::to_string),
            encrypted_password: "v1:nonce:cipher".to_string(),
            created_at_ms: 1,
            updated_at_ms: 2,
            last_connected_at_ms: None,
        }
    }

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

    /// 验证新建连接菜单只暴露连接配置和分类入口。
    ///
    /// 业务风险：
    /// - 本地终端已经拆到独立主导航页，连接菜单若继续出现本地终端，会让用户误以为它仍属于连接树。
    #[test]
    fn 新建连接类型菜单保留_ssh_smb_和分类入口() {
        assert_eq!(
            CONNECTION_CREATE_KINDS,
            &[
                ConnectionCreateKind::Ssh,
                ConnectionCreateKind::Smb,
                ConnectionCreateKind::Category
            ]
        );

        assert_eq!(ConnectionCreateKind::Ssh.label(), "SSH 连接");
        assert_eq!(
            char::from(ConnectionCreateKind::Ssh.icon()),
            char::from(Icon::Terminal)
        );
        assert_eq!(ConnectionCreateKind::Smb.label(), "SMB 连接");
        assert_eq!(
            char::from(ConnectionCreateKind::Smb.icon()),
            char::from(Icon::HardDrive)
        );
        assert_eq!(ConnectionCreateKind::Category.label(), "新建分类");
        assert_eq!(
            char::from(ConnectionCreateKind::Category.icon()),
            char::from(Icon::FolderPlus)
        );
    }

    /// 验证连接树普通模式按展开状态显示分类和连接。
    #[test]
    fn 连接树普通模式遵循分类展开状态() {
        let categories = vec![
            test_category("生产", None, 1),
            test_category("华东", Some("生产"), 2),
        ];
        let profiles = vec![
            test_profile("conn-root", "根连接", None),
            test_profile("conn-prod", "生产连接", Some("生产")),
            test_profile("conn-east", "华东连接", Some("华东")),
        ];
        let expanded = HashSet::from(["生产".to_string()]);

        let rows = build_connection_tree_rows(&categories, &profiles, &[], &expanded, "");
        let labels = rows
            .iter()
            .map(|row| match row {
                ConnectionTreeRow::Category {
                    category, depth, ..
                } => {
                    format!("C{depth}:{}", category.name)
                }
                ConnectionTreeRow::Profile { profile, depth } => {
                    format!("P{depth}:{}", profile.name())
                }
            })
            .collect::<Vec<_>>();

        assert_eq!(
            labels,
            vec!["C0:生产", "C1:华东", "P1:生产连接", "P0:根连接"]
        );
    }

    /// 验证搜索只匹配连接名称，并保留匹配连接的祖先分类。
    #[test]
    fn 连接树搜索显示匹配连接及祖先分类() {
        let categories = vec![
            test_category("生产", None, 1),
            test_category("华东", Some("生产"), 2),
            test_category("测试", None, 3),
        ];
        let profiles = vec![
            test_profile("conn-east", "Redis 主库", Some("华东")),
            test_profile("conn-test", "普通连接", Some("测试")),
        ];
        let rows =
            build_connection_tree_rows(&categories, &profiles, &[], &HashSet::new(), "redis");
        let labels = rows
            .iter()
            .map(|row| match row {
                ConnectionTreeRow::Category {
                    category,
                    depth,
                    expanded,
                } => {
                    format!("C{depth}:{}:{expanded}", category.name)
                }
                ConnectionTreeRow::Profile { profile, depth } => {
                    format!("P{depth}:{}", profile.name())
                }
            })
            .collect::<Vec<_>>();

        assert_eq!(
            labels,
            vec!["C0:生产:true", "C1:华东:true", "P2:Redis 主库"]
        );
    }

    /// 验证搜索不会因为分类名称匹配而显示无匹配连接的分类。
    #[test]
    fn 连接树搜索不匹配分类名称() {
        let categories = vec![test_category("Redis 分类", None, 1)];
        let profiles = vec![test_profile("conn-1", "普通连接", Some("Redis 分类"))];
        let rows =
            build_connection_tree_rows(&categories, &profiles, &[], &HashSet::new(), "redis");

        assert!(rows.is_empty());
    }

    /// 验证连接树会混合展示 SSH 与 SMB 连接，并继续只按连接名称搜索。
    #[test]
    fn 连接树混合展示_ssh_和_smb_连接() {
        let categories = vec![test_category("文件服务", None, 1)];
        let profiles = vec![test_profile("ssh-1", "SSH 主机", None)];
        let smb_profiles = vec![test_smb_profile("smb-1", "SMB 共享", Some("文件服务"))];
        let expanded = HashSet::from(["文件服务".to_string()]);

        let rows =
            build_connection_tree_rows(&categories, &profiles, &smb_profiles, &expanded, "smb");
        let labels = rows
            .iter()
            .map(|row| match row {
                ConnectionTreeRow::Category {
                    category, depth, ..
                } => format!("C{depth}:{}", category.name),
                ConnectionTreeRow::Profile { profile, depth } => {
                    format!("P{depth}:{}", profile.name())
                }
            })
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["C0:文件服务", "P1:SMB 共享"]);
    }

    /// 验证连接详情气泡只负责展示用户和地址信息。
    ///
    /// 业务风险：
    /// - 连接卡片主区域已收敛为名称，如果气泡格式不稳定，用户悬浮后仍无法确认具体目标主机。
    #[test]
    fn 连接详情气泡格式化用户和地址() {
        let profile = test_profile("conn-1", "火山云 Server0", None);

        assert_eq!(
            ConnectionsWorkspaceState::profile_tooltip_user_label(&profile),
            "用户：root"
        );
        assert_eq!(
            ConnectionsWorkspaceState::profile_tooltip_address_label(&profile),
            "地址：127.0.0.1:22"
        );
    }

    /// 验证终端尺寸会按内容区像素换算为行列。
    ///
    /// 业务风险：
    /// - 如果窗口 resize 后行列仍停在默认 80x24，远端 TUI 和本地渲染会在可视区域内错位。
    #[test]
    fn 终端尺寸会从面板_bounds_计算() {
        let size = connection_terminal_size_from_bounds_with_cell_width(
            Bounds::new(point(px(0.0), px(0.0)), size(px(805.0), px(365.0))),
            CONNECTION_TERMINAL_CELL_WIDTH,
        );

        assert_eq!(size.cols, 100);
        assert_eq!(size.rows, 20);
        assert_eq!(size.pixel_width, 805);
        assert_eq!(size.pixel_height, 365);
    }

    /// 验证终端内容区内边距按产品要求保持 4px。
    ///
    /// 业务风险：
    /// - 终端边距同时影响文本绘制、IME 候选窗口和鼠标格点换算，值不一致会再次造成点击选区错位。
    #[test]
    fn 终端内容区内边距为四像素() {
        assert_eq!(CONNECTION_TERMINAL_PADDING, 4.0);
    }

    /// 验证终端尺寸计算可以使用渲染层测量到的真实列宽。
    ///
    /// 业务风险：
    /// - 如果本地/SSH 终端继续共用经验列宽，字体实际宽度偏差会同时影响 pty 列数和鼠标选区。
    #[test]
    fn 终端尺寸支持真实列宽() {
        let size = connection_terminal_size_from_bounds_with_cell_width(
            Bounds::new(point(px(0.0), px(0.0)), size(px(90.0), px(36.0))),
            9.0,
        );

        assert_eq!(size.cols, 10);
        assert_eq!(size.rows, 2);
    }

    /// 验证鼠标坐标换算使用当前 tab 的真实列宽。
    ///
    /// 业务风险：
    /// - 拖拽选区时如果仍用固定 8px 列宽，终端内容越靠右，选区和鼠标位置偏差越大。
    #[test]
    fn 终端鼠标格点使用真实列宽() {
        let emulator = ConnectionTerminalEmulator::new(TerminalSize::new(10, 2, 0, 0));

        assert_eq!(emulator.point_from_panel_offset(17.9, 0.0, 9.0).column.0, 1);
        assert_eq!(emulator.point_from_panel_offset(18.1, 0.0, 9.0).column.0, 2);
    }
}
