// 设置窗口中的关于页签实现。
//
// 业务意图：
// - 该文件集中维护关于页签的产品信息、作者信息和特色功能展示，避免静态产品文案散落在设置窗口主实现中。
// - 关于页签只展示静态信息和编译期版本，不读取日志、不访问磁盘、不请求网络。
//
// 边界条件：
// - 版本号来自 Cargo 编译期环境变量，避免 UI 文案和包版本产生重复维护。
// - 作者信息当前按项目提交身份展示为 yueyang，并显式展示联系邮箱，方便用户反馈日志打开、编码和压缩包兼容问题。
// - 后续如 Cargo.toml 增加 authors，可统一改为读取包元数据，但仍建议保留可见邮箱入口。

use super::*;

/// 关于页签展示的软件名称。
const ABOUT_APP_NAME: &str = "LogClinic";
/// 关于页签展示的软件版本。
const ABOUT_APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 关于页签展示的作者信息。
const ABOUT_AUTHOR: &str = "yueyang";
/// 关于页签展示的作者邮箱。
const ABOUT_AUTHOR_EMAIL: &str = "yueyang.cao@163.com";
/// 关于页签展示的特色功能和对应使用教程。
///
/// 业务意图：
/// - 用户要求“使用教程”服务于已有特色功能，而不是新增一个独立帮助系统。
/// - 教程文案保持短句，避免关于页签承担完整帮助文档职责；后续如果加入在线文档入口，可在这里扩展链接或按钮。
const ABOUT_FEATURE_GUIDES: &[FeatureGuide] = &[
    FeatureGuide {
        title: "加载任意日志、目录、压缩包",
        guide: "点击“加载日志”或直接拖入文件、目录、ZIP/RAR/7Z/TAR.GZ/GZ 压缩包，左侧目录树会自动展开可读取日志。",
    },
    FeatureGuide {
        title: "大日志正常打开",
        guide: "打开超过 30MB 的大文件时会使用分页浏览，先显示可见区域，滚动和搜索时再按需读取内容。",
    },
    FeatureGuide {
        title: "线程日志分析功能",
        guide: "在左侧树选择线程日志后右键点击“线程日志分析”，可在独立窗口查看线程状态时间线和快照详情。",
    },
];

/// 关于页签中的特色功能教程项。
///
/// 业务意图：
/// - 使用结构体把功能标题和教程说明绑定在一起，避免两个数组下标不一致导致文案错配。
/// - 字段均为静态字符串，关于页签不需要运行期分配或读取外部配置。
struct FeatureGuide {
    /// 特色功能标题。
    title: &'static str,
    /// 面向用户的简短使用教程。
    guide: &'static str,
}

/// 渲染设置窗口里的关于页签。
///
/// 业务意图：
/// - 关于功能已经收入口设置窗口，用户从设置窗口左侧页签访问，不再从主窗口左侧大导航打开独立窗口。
/// - 内容仍保持软件、作者和特色功能三块，明暗主题下都应保持可读。
pub(in crate::app) fn render_about_settings_tab(
    palette: AppThemePalette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id("settings-about-tab")
        .flex()
        .flex_col()
        .size_full()
        .p_4()
        .gap_4()
        .overflow_y_scroll()
        .scrollbar_width(px(6.0))
        .bg(rgb(palette.background))
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(MainView::render_lucide_icon(
                    Some(Icon::Info),
                    24.0,
                    24.0,
                    palette.accent,
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(palette.text))
                                .child(ABOUT_APP_NAME),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(palette.muted_text))
                                .child(format!("版本 {}", ABOUT_APP_VERSION)),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded(px(8.0))
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .child(render_section_title("作者信息", palette))
                .child(render_author_item(
                    "作者",
                    ABOUT_AUTHOR,
                    Icon::User,
                    palette,
                ))
                .child(render_author_item(
                    "邮箱",
                    ABOUT_AUTHOR_EMAIL,
                    Icon::Mail,
                    palette,
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded(px(8.0))
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .child(render_section_title("特色功能", palette))
                .children(
                    ABOUT_FEATURE_GUIDES
                        .iter()
                        .map(|feature| render_feature_item(feature, palette)),
                ),
        )
}

/// 渲染一个信息区块标题。
///
/// 业务意图：
/// - 关于页签信息分为软件、作者和特色功能三块；统一标题样式可以减少视觉噪声。
fn render_section_title(title: &'static str, palette: AppThemePalette) -> gpui::Div {
    div()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(palette.text))
        .child(title)
}

/// 渲染特色功能和对应使用教程。
///
/// 业务意图：
/// - 功能条目使用轻量图标、标题和一行教程说明，帮助用户理解入口位置，同时保持关于页签的信息密度。
/// - 教程说明使用弱文本颜色，明暗主题下都不抢占功能标题的视觉层级。
fn render_feature_item(feature: &'static FeatureGuide, palette: AppThemePalette) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .gap_2()
        .child(MainView::render_lucide_icon(
            Some(Icon::Sparkles),
            TOOLBAR_BUTTON_ICON_WIDTH,
            TOOLBAR_BUTTON_ICON_SIZE,
            palette.accent,
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(palette.text))
                        .child(feature.title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(palette.muted_text))
                        .child(feature.guide),
                ),
        )
}

/// 渲染作者信息条目。
///
/// 业务意图：
/// - 作者信息现在包含姓名和邮箱两类短文本；统一条目结构可以保证明暗主题下的间距和可读性一致。
/// - 邮箱只作为静态文本展示，不自动打开邮件客户端，避免不同平台默认邮件应用缺失时产生额外错误。
fn render_author_item(
    label: &'static str,
    value: &'static str,
    icon: Icon,
    palette: AppThemePalette,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .text_color(rgb(palette.text))
        .child(MainView::render_lucide_icon(
            Some(icon),
            TOOLBAR_BUTTON_ICON_WIDTH,
            TOOLBAR_BUTTON_ICON_SIZE,
            palette.muted_text,
        ))
        .child(
            div()
                .w(px(44.0))
                .text_color(rgb(palette.muted_text))
                .child(label),
        )
        .child(value)
}
