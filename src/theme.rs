//! LogClinic 应用主题与调色板定义。
//!
//! 业务意图：
//! - 集中维护主题偏好、系统外观解析和基础调色板，避免主窗口、搜索窗口和设置窗口各自散落颜色规则。
//! - 配置读写仍由 `config` 模块负责，本模块只表达主题含义和运行时计算结果。
//!
//! 边界条件：
//! - 不改变已持久化的主题文本值；`light`、`dark`、`system` 必须长期兼容。

use gpui::WindowAppearance;
use lucide_icons::Icon;

use crate::highlighting::SyntaxTheme;

/// 主题设置选项。
///
/// 业务意图：
/// - 用户明确要求主题可选“明亮主题、暗色主题、跟随系统”。
/// - 用户选择会通过应用调色板立即刷新主窗口、搜索窗口和设置窗口，并写入配置文件供下次启动恢复。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThemePreference {
    /// 强制使用明亮主题。
    Light,
    /// 强制使用暗色主题。
    Dark,
    /// 跟随操作系统外观。
    System,
}

impl ThemePreference {
    /// 返回主题选项固定展示顺序。
    pub(crate) fn all() -> &'static [Self] {
        &[Self::Light, Self::Dark, Self::System]
    }

    /// 返回主题选项中文标签。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Light => "明亮主题",
            Self::Dark => "暗色主题",
            Self::System => "跟随系统",
        }
    }

    /// 返回主题选项图标。
    pub(crate) fn icon(self) -> Icon {
        match self {
            Self::Light => Icon::Sun,
            Self::Dark => Icon::Moon,
            Self::System => Icon::Monitor,
        }
    }

    /// 返回主题偏好的持久化文本。
    ///
    /// 业务意图：
    /// - 配置文件使用稳定英文值，避免中文文案调整影响已经写入的用户配置。
    pub(crate) fn as_config_value(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }
}

/// 当前实际生效的主题。
///
/// 业务意图：
/// - 用户偏好中的“跟随系统”需要结合 GPUI 当前窗口外观才能决定最终使用明亮还是暗色调色板。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EffectiveTheme {
    /// 当前实际使用明亮调色板。
    Light,
    /// 当前实际使用暗色调色板。
    Dark,
}

impl EffectiveTheme {
    /// 根据用户偏好和系统窗口外观计算实际主题。
    ///
    /// 边界条件：
    /// - 强制明亮/暗色优先于系统外观。
    /// - `VibrantLight` 和 `VibrantDark` 分别归入明亮和暗色，避免平台差异影响业务层判断。
    pub(crate) fn resolve(preference: ThemePreference, appearance: WindowAppearance) -> Self {
        match preference {
            ThemePreference::Light => Self::Light,
            ThemePreference::Dark => Self::Dark,
            ThemePreference::System => match appearance {
                WindowAppearance::Light | WindowAppearance::VibrantLight => Self::Light,
                WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::Dark,
            },
        }
    }

    /// 转换为日志语法高亮模块使用的主题枚举。
    ///
    /// 业务意图：
    /// - UI 主题和语法高亮主题属于不同模块；这里做显式映射，避免高亮模块依赖主窗口调色板。
    pub(crate) fn syntax_theme(self) -> SyntaxTheme {
        match self {
            Self::Light => SyntaxTheme::Light,
            Self::Dark => SyntaxTheme::Dark,
        }
    }
}

/// 应用基础主题调色板。
///
/// 业务意图：
/// - 当前 UI 大量直接使用颜色字面量；集中调色板可以让主窗口、搜索窗口和设置窗口共享同一套明暗色。
/// - 字段使用 `u32` RGB 值，调用处继续通过 GPUI `rgb` 转换，避免引入新的颜色类型依赖。
#[derive(Clone, Copy)]
pub(crate) struct AppThemePalette {
    /// 页面或窗口根背景色。
    pub(crate) background: u32,
    /// 面板背景色，例如工具栏、侧栏、tab 栏。
    pub(crate) panel: u32,
    /// 比面板更高一层的容器背景色，例如输入框、卡片、菜单。
    pub(crate) surface: u32,
    /// 悬浮背景色。
    pub(crate) hover: u32,
    /// 选中背景色。
    pub(crate) selected: u32,
    /// 主文本颜色。
    pub(crate) text: u32,
    /// 次级文本颜色。
    pub(crate) muted_text: u32,
    /// 边框和分割线颜色。
    pub(crate) border: u32,
    /// 品牌/交互强调色。
    pub(crate) accent: u32,
    /// 强调色悬浮态。
    pub(crate) accent_hover: u32,
    /// 强调按钮上的文本色。
    pub(crate) on_accent: u32,
    /// 输入框背景色。
    pub(crate) input: u32,
    /// 菜单背景色。
    pub(crate) menu: u32,
    /// 搜索命中或当前定位行背景色。
    pub(crate) search_highlight: u32,
    /// 错误文本颜色。
    pub(crate) error: u32,
    /// 滚动条滑块颜色。
    pub(crate) scrollbar: u32,
    /// 滚动条滑块悬浮色。
    pub(crate) scrollbar_hover: u32,
}

impl AppThemePalette {
    /// 返回指定实际主题的调色板。
    pub(crate) fn for_theme(theme: EffectiveTheme) -> Self {
        match theme {
            EffectiveTheme::Light => Self {
                // 明亮主题避免大面积纯白和高反差灰边，降低长时间查看日志时的视觉疲劳；
                // 信息密度仍由布局常量控制，这里只调整颜色，不改变任何尺寸或间距。
                background: 0xfbfcfd,
                panel: 0xf4f6f8,
                surface: 0xffffff,
                hover: 0xeef3f7,
                selected: 0xe7f2ff,
                text: 0x24292f,
                muted_text: 0x667085,
                border: 0xd7dde4,
                accent: 0x2f75d6,
                accent_hover: 0x245fb2,
                on_accent: 0xffffff,
                input: 0xfbfcfe,
                menu: 0xffffff,
                search_highlight: 0xfff1a8,
                error: 0xcf222e,
                scrollbar: 0xd4dbe3,
                scrollbar_hover: 0x98a2ad,
            },
            EffectiveTheme::Dark => Self {
                // 暗色主题使用略抬高的背景和更柔和的边框，避免纯黑界面造成眩光感；
                // 文本对比仍保持足够清晰，兼容日志正文、菜单、搜索结果和线程分析窗口。
                background: 0x101418,
                panel: 0x171d23,
                surface: 0x14191f,
                hover: 0x242b33,
                selected: 0x17324a,
                text: 0xdce3ea,
                muted_text: 0xa0a9b3,
                border: 0x343c46,
                accent: 0x68b0ff,
                accent_hover: 0x8bc4ff,
                on_accent: 0x101418,
                input: 0x0f1318,
                menu: 0x171d23,
                search_highlight: 0x4a3a05,
                error: 0xff7b72,
                scrollbar: 0x3a434d,
                scrollbar_hover: 0x747f8b,
            },
        }
    }

    /// 返回左侧资源树行 hover/选中背景色。
    ///
    /// 业务意图：
    /// - 笔记树和连接树都属于“左侧资源树”，hover 背景需要比普通按钮 hover 更明确，便于用户在密集层级列表里确认命中行。
    /// - 不直接加深全局 `hover`，避免影响工具栏按钮、菜单项和其它页面的轻量悬浮反馈。
    ///
    /// 边界条件：
    /// - 明亮主题下略深于全局 hover 但仍低于蓝色选中色，避免看起来像强选中。
    /// - 暗色主题下略抬高亮度而不是继续压暗，保证深色背景中 hover 仍可被分辨。
    pub(crate) fn resource_tree_hover(self) -> u32 {
        if self.background < 0x808080 {
            0x2a333d
        } else {
            0xe6edf3
        }
    }
}
