// 设置窗口中的关于页签实现。
//
// 业务意图：
// - 该文件集中维护关于页签的产品信息、作者信息和功能说明展示，避免静态产品文案散落在设置窗口主实现中。
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
/// 关于页签展示的软件功能说明。
///
/// 业务意图：
/// - 用户要求把当前软件功能特点放到“设置-关于-功能说明”，因此这里集中保存用户可直接阅读的能力清单。
/// - 功能说明保持静态文本，不在关于页运行时扫描配置、插件或日志，避免打开设置窗口时产生额外 I/O 或网络副作用。
/// - 大日志内存占用使用代码实现推导出的典型值，实际数值会随平均行长、搜索结果、系统文件缓存和运行时状态波动。
const ABOUT_FEATURE_GUIDES: &[FeatureGuide] = &[
    FeatureGuide {
        title: "多来源日志加载",
        guide: &[
            "支持本地文件、目录、拖拽路径和系统“打开方式”入口。",
            "目录递归扫描但不跟随符号链接，避免越界读取未选择位置。",
        ],
    },
    FeatureGuide {
        title: "压缩包和嵌套压缩包浏览",
        guide: &[
            "支持 ZIP、RAR、TAR、TAR.GZ、GZ、7Z。",
            "成员路径会做安全归一化，可在安全和大小限制内展开嵌套压缩包。",
        ],
    },
    FeatureGuide {
        title: "大日志分页浏览",
        guide: &[
            "超过 30MB 的日志进入分页模式，建立行索引后按需读取可见行。",
            "典型 200B 行长下，1G/5G/10G/20G 约占 200MB/540MB/950MB/1.7GB 增量内存。",
        ],
    },
    FeatureGuide {
        title: "编码识别与手动切换",
        guide: &[
            "支持 UTF-8、UTF-8 BOM、GBK、GB18030、Big5。",
            "自动识别失败或出现替换字符时，可保留原始字节并手动切换编码重新解析。",
        ],
    },
    FeatureGuide {
        title: "日志阅读和语法高亮",
        guide: &[
            "支持多 tab、行号、横向滚动、文本选择复制、自绘滚动条和可选 minimap。",
            "普通日志、Java 线程 dump、XML、properties 会使用不同高亮策略。",
        ],
    },
    FeatureGuide {
        title: "搜索与定位",
        guide: &[
            "支持当前文件、当前目录和选中文件搜索，可使用普通文本或正则。",
            "支持大小写敏感、命中计数、上一个/下一个跳转和分页日志流式搜索。",
        ],
    },
    FeatureGuide {
        title: "Java 线程日志分析",
        guide: &[
            "可解析 Java thread dump 快照，生成线程状态时间线。",
            "支持线程名通配和完整堆栈片段过滤，并可点击定位回原始日志行。",
        ],
    },
    FeatureGuide {
        title: "HPROF 堆转储分析",
        guide: &[
            "支持 .hprof 和 .bin，解析对象图并计算 Dominator Tree。",
            "按 MAT 兼容语义展示 shallow/retained size，支持 sidecar index 缓存和线程对象详情。",
        ],
    },
    FeatureGuide {
        title: "AI 辅助排障",
        guide: &[
            "支持 OpenAI 兼容模型配置、流式 AI 对话和日志智能分析。",
            "日志只在用户主动触发时按约 500KB 分块发送，并保留证据行号便于核对。",
        ],
    },
    FeatureGuide {
        title: "排障笔记",
        guide: &[
            "笔记以真实目录和 Markdown 文件保存。",
            "支持目录树、创建、重命名、编辑、旧 SQLite 迁移和删除到系统回收站/废纸篓。",
        ],
    },
    FeatureGuide {
        title: "连接与文件管理",
        guide: &[
            "支持 SSH 连接、主机指纹校验、加密保存密码、远程终端、SFTP 和 SMB 文件管理。",
            "可列目录、预览、上传、下载、处理冲突和显示传输进度。",
        ],
    },
    FeatureGuide {
        title: "插件扩展和系统集成",
        guide: &[
            "插件可贡献主导航、日志树右键菜单、笔记树右键菜单和声明式页面。",
            "macOS/Windows 配置目录、系统打开方式和右键菜单集成按平台处理。",
        ],
    },
];

/// 关于页签中的功能说明项。
///
/// 业务意图：
/// - 使用结构体把功能标题和说明正文绑定在一起，避免两个数组下标不一致导致文案错配。
/// - 字段均为静态字符串，关于页签不需要运行期分配或读取外部配置。
struct FeatureGuide {
    /// 功能标题。
    title: &'static str,
    /// 面向用户的简短功能说明。
    ///
    /// 业务意图：
    /// - 每条说明拆成短行，避免 GPUI 文本在宽窗口或窄窗口中按单行撑破关于页卡片。
    /// - 渲染层仍会启用普通换行，确保极端窗口宽度下长术语行也不会越界。
    guide: &'static [&'static str],
}

/// 渲染设置窗口里的关于页签。
///
/// 业务意图：
/// - 关于功能已经收入口设置窗口，用户从设置窗口左侧页签访问，不再从主窗口左侧大导航打开独立窗口。
/// - 内容仍保持软件、作者和功能说明三块，明暗主题下都应保持可读。
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
                .child(render_section_title("功能说明", palette))
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

/// 渲染功能说明条目。
///
/// 业务意图：
/// - 功能条目使用轻量图标、标题和说明正文，帮助用户理解当前软件能力，同时保持关于页签的信息密度。
/// - 说明正文使用弱文本颜色，明暗主题下都不抢占功能标题的视觉层级。
fn render_feature_item(feature: &'static FeatureGuide, palette: AppThemePalette) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .gap_2()
        .min_w_0()
        .py_1()
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
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(palette.text))
                        .child(feature.title),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_xs()
                        .line_height(px(18.0))
                        .text_color(rgb(palette.muted_text))
                        .children(
                            feature
                                .guide
                                .iter()
                                .map(|line| div().min_w_0().whitespace_normal().child(*line)),
                        ),
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
