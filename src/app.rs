//! LogClinic 桌面客户端的应用装配与主视图协调模块。
//!
//! 业务意图：
//! - 该模块承接从历史 `main.rs` 中迁出的主窗口装配、根状态协调和全局事件分发。
//! - 功能域较大的 UI 代码已经拆到 `app_impl` 子模块，由正常 `mod` 边界约束跨域访问。
//! - 这里仍保留跨域协调逻辑，例如窗口句柄、后台任务回调、主视图状态汇总和全局快捷键。
//!
//! 跨平台约束：
//! - macOS 和 Windows 都需要从同一个应用装配入口启动主窗口，平台打开文件和窗口尺寸差异由专门函数处理。
//! - 窗口尺寸使用 GPUI 的逻辑像素表达，由 GPUI 负责映射到具体平台窗口系统。
//! - 当前只持久化主窗口宽高、主题偏好和日志字号，不持久化窗口位置，避免跨显示器恢复造成窗口不可见。

use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{BTreeSet, HashMap, HashSet},
    env, fs, io,
    ops::{Deref, DerefMut, Range},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt as _, AnyWindowHandle, App, AppContext, Application, AsyncApp, Bounds,
    ClickEvent, ClipboardItem, Context, DisplayId, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, ExternalPaths, FontWeight, GlobalElementId, InteractiveElement,
    IntoElement, KeyBinding, KeyDownEvent, Keystroke, LayoutId, ListAlignment,
    ListHorizontalSizingBehavior, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, ParentElement, PathPromptOptions, Pixels, Point, Render, ScrollHandle,
    ScrollStrategy, ScrollWheelEvent, ShapedLine, SharedString, StatefulInteractiveElement, Style,
    Styled as _, StyledText, TextRun, TitlebarOptions, UTF16Selection, UnderlineStyle,
    UniformListScrollHandle, Window, WindowAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, actions, div, fill, list, point, px, relative, rgb, size, uniform_list,
};
use lucide_icons::{Icon, LUCIDE_FONT_BYTES};
use serde::{Deserialize, Serialize};

use crate::archive_materializer::{
    cleanup_materialized_file, cleanup_stale_large_log_cache, materialize_source_for_paging,
};
use crate::highlighting::{SyntaxTheme, highlight_line};
use crate::large_log::{LargeLogOpenResult, LogTabDocument, open_log_source_for_tab};
use crate::launch::{log_source_paths_from_launch_arguments, log_source_paths_from_open_urls};
use crate::log_content::{
    EncodingChoice, LogContentError, LogTextEncoding, decode_log_bytes, read_log_source_bytes,
};
use crate::log_loader::{
    LoadedLogTree, LogFileSource, LogTreeEntryKind, LogTreeRow as LoadedLogTreeRow,
    load_log_sources,
};
use crate::paged_document;
use crate::search::{
    SearchFileError, SearchMatchMode, SearchOptions, SearchProgress, SearchResultItem, SearchScope,
    collect_current_directory_sources, count_query_occurrences, search_lines,
    source_location_label,
};
use crate::stream_search::{count_query_occurrences_paged, search_paged_document};
use crate::theme::{AppThemePalette, EffectiveTheme, ThemePreference};

actions!(logclinic, [OpenSearchDialog]);

/// 设置窗口关于页签功能域。
#[path = "app_impl/about_window.rs"]
mod about_window;
/// AI 对话页面、持久化和流式请求功能域。
#[path = "app_impl/ai_chat/mod.rs"]
mod ai_chat;
/// 应用配置路径、解析和持久化功能域。
#[path = "app_impl/config.rs"]
mod config;
/// HPROF dump 分析独立窗口功能域。
#[path = "app_impl/hprof_analysis.rs"]
mod hprof_analysis;
/// 左侧日志目录树功能域。
#[path = "app_impl/log_tree_methods.rs"]
mod log_tree_methods;
/// 日志正文查看器和 tab 正文功能域。
#[path = "app_impl/log_viewer_methods.rs"]
mod log_viewer_methods;
/// 主窗口搜索结果面板功能域。
#[path = "app_impl/search_results_methods.rs"]
mod search_results_methods;
/// 搜索独立窗口功能域。
#[path = "app_impl/search_window.rs"]
mod search_window;
/// 设置独立窗口功能域。
#[path = "app_impl/settings_window.rs"]
mod settings_window;
/// 主窗口拆分后的纯状态容器。
#[path = "app_impl/state.rs"]
mod state;
/// 应用层纯状态测试。
#[cfg(test)]
#[path = "app_impl/tests.rs"]
mod tests;
/// 线程日志分析独立窗口功能域。
#[path = "app_impl/thread_analysis.rs"]
mod thread_analysis;

use ai_chat::*;
use config::*;
use hprof_analysis::HprofAnalysisView;
use search_window::SearchDialogWindowView;
use settings_window::SettingsWindowView;
use state::*;
use thread_analysis::{
    SearchResultsResizeDrag, SearchTarget, ThreadAnalysisFilterRule, ThreadSnapshot,
    ThreadStateKind, ThreadStateSample, ThreadStateSamplePending, ThreadTimelineCell,
};
use thread_analysis::{ThreadAnalysisData, ThreadAnalysisWindowView};

/// 应用主窗口的标题。
///
/// 业务意图：
/// - 第一阶段只验证窗口创建，因此标题使用稳定的产品名 `LogClinic`。
/// - 后续如果需要显示当前打开文件名或工作区状态，应在明确标题格式规则后再扩展。
const MAIN_WINDOW_TITLE: &str = "LogClinic";

/// 主窗口的默认宽度。
///
/// 业务约束：
/// - 用户明确要求大屏默认窗口宽度固定为 1600px，以便日志 tab 和正文区域在首次打开时有更多横向空间。
/// - 这里使用浮点字面量是为了匹配 GPUI `px` 的尺寸 API，避免在调用处反复转换。
const MAIN_WINDOW_WIDTH: f32 = 1600.0;

/// 主窗口的默认高度。
///
/// 边界说明：
/// - 用户明确要求大屏默认窗口高度固定为 900px，保证日志正文首屏拥有稳定的可视行数。
/// - 当前不设置最小尺寸和最大尺寸，因为本阶段只要求默认大小和用户手动调整后的尺寸记忆。
const MAIN_WINDOW_HEIGHT: f32 = 900.0;

/// 小屏电脑的主显示器宽度阈值。
///
/// 业务意图：
/// - 用户明确要求按逻辑像素宽度 `< 1440px` 判定小屏电脑。
/// - 这里使用 GPUI 暴露的显示器逻辑像素，由框架负责处理 macOS Retina 和 Windows 缩放比例差异。
const SMALL_SCREEN_MAXIMIZED_WIDTH_THRESHOLD: f32 = 1440.0;

/// 历史窗口宽度接近显示器宽度时判定为“最大化残留”的比例阈值。
///
/// 业务意图：
/// - 旧版本会在最大化关闭时保存接近屏幕宽度的窗口尺寸，导致大屏下次启动看起来仍是最大化。
/// - 只使用宽度判定是因为 macOS 标题栏、菜单栏和 Windows 任务栏会让高度存在平台差异；宽度是更稳定的最大化信号。
///
/// 边界条件：
/// - 阈值保留少量平台装饰误差，避免最大化窗口因为边框、缩放或系统保留区域略小于显示器宽度而漏判。
const MAXIMIZED_RESTORED_WIDTH_RATIO: f32 = 0.96;

/// 主窗口尺寸偏好文件名。
///
/// 业务意图：
/// - 当前只保存主窗口宽高，不保存位置、最大化状态或其他设置，因此使用独立小文本文件即可。
/// - 如果后续接入完整设置系统，应迁移到统一配置文件并保留兼容读取逻辑。
const MAIN_WINDOW_SIZE_FILE_NAME: &str = "window-size.txt";

/// 主题偏好文件名。
///
/// 业务意图：
/// - 主题属于用户明确设置，必须和窗口大小一样跨启动保留。
/// - 文件内容保持为简单英文枚举值，避免仅为单个配置新增 JSON/TOML 依赖。
const THEME_PREFERENCE_FILE_NAME: &str = "theme-preference.txt";

/// 日志显示字号偏好文件名。
///
/// 业务意图：
/// - 日志字号是用户明确调整的阅读偏好，需要像主题一样跨启动恢复。
/// - 文件只保存一个像素值，继续使用简单文本格式，避免为单项设置引入完整配置依赖。
const LOG_VIEWER_FONT_SIZE_FILE_NAME: &str = "log-viewer-font-size.txt";

/// 线程日志分析过滤配置文件名。
///
/// 业务意图：
/// - 用户会在设置窗口中粘贴需要过滤的线程堆栈，配置必须跨重启保留，避免每次排查都重新维护无效线程列表。
/// - 文件保存原始多行文本而不是结构化格式，方便用户直接打开配置文件排查或批量替换。
const THREAD_ANALYSIS_FILTER_FILE_NAME: &str = "thread-analysis-filter.txt";

/// 快搜关键字配置文件名。
///
/// 业务意图：
/// - 快搜关键字是用户面向排障场景维护的常用搜索词集合，需要跨应用重启保留。
/// - 文件保存英文逗号分隔的单行文本，保持可手工编辑，同时避免为一个简单列表引入结构化配置依赖。
const QUICK_SEARCH_KEYWORDS_FILE_NAME: &str = "quick-search-keywords.txt";

/// 模型配置文件名。
///
/// 业务意图：
/// - 模型配置包含多个 OpenAI 兼容接口档案和默认模型选择，需要跨应用重启恢复。
/// - 文件使用 JSON 而不是多个文本文件，便于一次性保存列表、默认 ID 和 API Key 等结构化字段。
///
/// 安全边界：
/// - 用户已确认第一版 API Key 明文保存在应用配置目录；UI 默认掩码显示，代码中避免把 Key 写入错误文案。
const MODEL_CONFIGS_FILE_NAME: &str = "model-configs.json";

/// 测试模型接口的超时时间。
///
/// 业务意图：
/// - 测试按钮只用于快速确认配置是否可用，不能因为网络不可达或本地服务无响应长期占用后台线程。
/// - 20 秒是需求确认的默认超时，既兼容本地模型冷启动，也能让 UI 尽快反馈失败。
const MODEL_TEST_TIMEOUT_SECONDS: u64 = 20;

/// OpenAI 兼容 Chat Completions 路径。
///
/// 业务意图：
/// - `base_url` 约定为 API 根路径，例如 `https://api.openai.com/v1`，测试时统一拼接该相对路径。
/// - 单独定义后测试和真实请求共用一套拼接规则，避免尾斜杠处理不一致。
const MODEL_TEST_CHAT_COMPLETIONS_PATH: &str = "chat/completions";

/// 快搜默认关键字配置。
///
/// 业务意图：
/// - 首次使用快搜时内置常见导入、导出、转换和水印相关排障词，用户不需要先进入设置维护才能使用快搜。
///
/// 边界条件：
/// - 该默认值只在配置文件不存在时使用；如果用户保存空配置，会写入空文件，后续启动必须尊重用户显式选择。
/// - 关键字只使用英文逗号分隔，和快搜配置解析规则保持一致。
const DEFAULT_QUICK_SEARCH_KEYWORDS_TEXT: &str = "excel,import,export,wbi,convertFile,waterMark";

/// 线程日志分析默认过滤配置。
///
/// 业务意图：
/// - Resin 的网络 accept 和 keepalive 线程在大量 thread dump 中经常长期存在，通常不代表业务阻塞根因。
/// - 首次使用线程分析时默认过滤这些稳定噪声线程，减少时间线中的无效线程；用户仍可在设置页清空或改写配置文件。
///
/// 边界条件：
/// - 该默认值只在配置文件不存在时使用；如果用户点击“清空”，会写入空文件，后续启动必须尊重用户显式选择。
/// - 文本使用 LF 作为内置换行，粘贴或保存路径仍会通过统一规范化函数处理 CRLF。
const DEFAULT_THREAD_ANALYSIS_FILTER_TEXT: &str = concat!(
    "java.lang.Thread.State: RUNNABLE\n",
    "\tat java.net.PlainSocketImpl.socketAccept(Native Method)\n",
    "\tat java.net.AbstractPlainSocketImpl.accept(AbstractPlainSocketImpl.java:409)\n",
    "\tat java.net.ServerSocket.implAccept(ServerSocket.java:545)\n",
    "\tat java.net.ServerSocket.accept(ServerSocket.java:513)\n",
    "\tat com.caucho.vfs.QServerSocketWrapper.accept(QServerSocketWrapper.java:105)\n",
    "\tat com.caucho.network.listen.TcpPort.accept(TcpPort.java:1380)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.accept(TcpSocketLink.java:1039)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.handleAcceptTaskImpl(TcpSocketLink.java:989)\n",
    "\tat com.caucho.network.listen.ConnectionTask.runThread(ConnectionTask.java:117)\n",
    "\tat com.caucho.network.listen.ConnectionTask.run(ConnectionTask.java:93)\n",
    "\tat com.caucho.network.listen.SocketLinkThreadLauncher.handleTasks(SocketLinkThreadLauncher.java:175)\n",
    "\tat com.caucho.network.listen.TcpSocketAcceptThread.run(TcpSocketAcceptThread.java:61)\n",
    "\tat com.caucho.env.thread2.ResinThread2.runTasks(ResinThread2.java:173)\n",
    "\tat com.caucho.env.thread2.ResinThread2.run(ResinThread2.java:118)\n",
    "\n",
    "java.lang.Thread.State: RUNNABLE\n",
    "\tat java.net.SocketInputStream.socketRead0(Native Method)\n",
    "\tat java.net.SocketInputStream.socketRead(SocketInputStream.java:116)\n",
    "\tat java.net.SocketInputStream.read(SocketInputStream.java:171)\n",
    "\tat java.net.SocketInputStream.read(SocketInputStream.java:141)\n",
    "\tat com.caucho.vfs.SocketStream.read(SocketStream.java:187)\n",
    "\tat com.caucho.vfs.SocketStream.readTimeout(SocketStream.java:239)\n",
    "\tat com.caucho.vfs.ReadStream.fillWithTimeout(ReadStream.java:1147)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.threadKeepalive(TcpSocketLink.java:1482)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.processKeepalive(TcpSocketLink.java:1460)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.handleRequestsImpl(TcpSocketLink.java:1300)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.handleRequests(TcpSocketLink.java:1215)\n",
    "\tat com.caucho.network.listen.TcpSocketLink.handleAcceptTaskImpl(TcpSocketLink.java:1011)\n",
    "\tat com.caucho.network.listen.ConnectionTask.runThread(ConnectionTask.java:117)\n",
    "\tat com.caucho.network.listen.ConnectionTask.run(ConnectionTask.java:93)\n",
    "\tat com.caucho.network.listen.SocketLinkThreadLauncher.handleTasks(SocketLinkThreadLauncher.java:175)\n",
    "\tat com.caucho.network.listen.TcpSocketAcceptThread.run(TcpSocketAcceptThread.java:61)\n",
    "\tat com.caucho.env.thread2.ResinThread2.runTasks(ResinThread2.java:173)\n",
    "\tat com.caucho.env.thread2.ResinThread2.run(ResinThread2.java:118)"
);

/// 左侧目录树右键菜单宽度。
///
/// 业务意图：
/// - 菜单承载“另存为”和“线程日志分析”两个文件操作，宽度需要足够展示中文命令且不挤压左侧树。
const LOG_TREE_CONTEXT_MENU_WIDTH: f32 = 176.0;

/// 左侧目录树右键菜单单项高度。
///
/// 业务意图：
/// - 与 tab 右键菜单保持相同操作密度，保证 macOS 和 Windows 鼠标命中体验一致。
const LOG_TREE_CONTEXT_MENU_ITEM_HEIGHT: f32 = 34.0;

/// 日志正文右键菜单宽度。
///
/// 业务意图：
/// - 菜单目前承载“复制”和“另存为”两个正文相关操作，宽度需要兼顾中文文案和鼠标命中面积。
const LOG_VIEWER_CONTEXT_MENU_WIDTH: f32 = 152.0;

/// 日志正文右键菜单单项高度。
///
/// 业务意图：
/// - 与左侧树和 tab 右键菜单保持一致密度，避免同一应用内菜单命中体验不一致。
const LOG_VIEWER_CONTEXT_MENU_ITEM_HEIGHT: f32 = 34.0;

/// 线程日志分析窗口默认宽度。
///
/// 业务意图：
/// - 时间线需要同时展示线程名和多个快照列，因此使用比设置窗口更宽的独立窗口。
const THREAD_ANALYSIS_WINDOW_WIDTH: f32 = 1040.0;

/// 线程日志分析窗口默认高度。
///
/// 业务意图：
/// - Java thread dump 往往包含大量线程，较高窗口可以减少初次打开后的滚动成本。
const THREAD_ANALYSIS_WINDOW_HEIGHT: f32 = 720.0;

/// 线程分析图中线程名列宽度。
///
/// 业务意图：
/// - Java 线程名经常包含业务前缀、线程池编号和连接信息，保留固定宽度便于和右侧时间线对齐。
const THREAD_ANALYSIS_NAME_COLUMN_WIDTH: f32 = 260.0;

/// 线程分析图中单个时间快照列宽度。
///
/// 业务意图：
/// - 横轴不再展示时间文本后，列宽只需要容纳正方形状态色块和少量间距，避免大量快照时横向滚动过长。
const THREAD_ANALYSIS_SNAPSHOT_COLUMN_WIDTH: f32 = 24.0;

/// 线程分析图中状态色块边长。
///
/// 业务意图：
/// - 用户要求色块高度保持不变且宽度与高度一致，因此使用固定 18px 正方形。
const THREAD_ANALYSIS_STATE_BLOCK_SIZE: f32 = 18.0;

/// 线程分析图中最近一次点击跳转色块的强调色。
///
/// 业务意图：
/// - 点击跳转后的色块需要和 Java 线程状态色区分开，帮助用户回到分析窗口时快速确认刚才定位过哪一段日志。
/// - 这里使用玫红色，避开当前状态色中的绿色、红色、橙色、青色、紫色和灰色；明暗主题下都保持可辨识。
const THREAD_ANALYSIS_JUMPED_CELL_COLOR: u32 = 0xec4899;

/// 线程分析色块悬浮气泡宽度。
///
/// 业务意图：
/// - 气泡需要容纳完整线程名和最多 5 行日志预览；固定宽度便于根据窗口边界计算弹出方向。
const THREAD_ANALYSIS_POPUP_WIDTH: f32 = 520.0;

/// 线程分析色块悬浮气泡预估高度。
///
/// 业务意图：
/// - GPUI 在悬浮事件阶段尚未布局气泡，不能读取真实高度；这里按三行信息和五行预览估算，
///   用于选择向上或向下弹出，避免靠近窗口底部时被遮挡。
const THREAD_ANALYSIS_POPUP_ESTIMATED_HEIGHT: f32 = 190.0;

/// 线程分析色块悬浮气泡与鼠标悬浮点的间距。
///
/// 业务意图：
/// - 保留少量间距，避免气泡刚出现就盖住当前悬浮的状态色块。
const THREAD_ANALYSIS_POPUP_OFFSET: f32 = 12.0;

/// 线程分析色块悬浮气泡与窗口边缘的最小间距。
///
/// 业务意图：
/// - 气泡贴边会影响阴影和边框识别，保留边距也能减少被系统标题栏或窗口边框裁切的风险。
const THREAD_ANALYSIS_POPUP_MARGIN: f32 = 8.0;

/// 判断历史尺寸是否像旧版本保存下来的最大化窗口宽度。
///
/// 业务意图：
/// - 大屏下无历史尺寸应默认使用 1600x900；如果旧配置保存了最大化宽度，继续尊重它会让窗口看起来仍然最大化。
/// - 这里只过滤接近当前主显示器宽度的历史值，普通用户手动调整过的窗口尺寸仍继续恢复。
///
/// 边界条件：
/// - 显示器宽度读取失败时不能判断是否最大化残留，保守保留历史尺寸。
/// - 小屏仍保留“历史尺寸优先”规则，避免用户在小屏上手动调整出的宽窗口被误判为最大化残留。
fn saved_size_looks_maximized(
    saved_size: MainWindowSizePreference,
    primary_display_width: Option<f32>,
) -> bool {
    let Some(display_width) = primary_display_width else {
        return false;
    };
    if !display_width.is_finite() || display_width <= 0.0 {
        return false;
    }
    if display_width < SMALL_SCREEN_MAXIMIZED_WIDTH_THRESHOLD {
        return false;
    }
    saved_size.width >= display_width * MAXIMIZED_RESTORED_WIDTH_RATIO
}

/// 主窗口首次启动时的尺寸策略。
///
/// 业务意图：
/// - 该枚举把“是否有历史尺寸”和“小屏默认最大化”的产品规则拆成纯数据，便于单元测试覆盖。
/// - 真正转换为 GPUI `WindowBounds` 时再依赖 `App`，避免测试环境必须启动真实窗口系统。
#[derive(Clone, Copy, Debug, PartialEq)]
enum MainWindowStartupDecision {
    /// 使用用户上次保存的宽高，窗口启动时仍重新居中。
    Remembered(MainWindowSizePreference),
    /// 使用固定 1600x900 居中窗口，适用于大屏或无法读取显示器信息的场景。
    DefaultWindowed,
    /// 使用系统最大化窗口，适用于首次在小屏电脑启动。
    DefaultMaximized,
}

/// 根据历史宽高和主显示器宽度决定主窗口启动策略。
///
/// 业务意图：
/// - 历史宽高代表用户明确调整过窗口大小，优先级高于小屏默认最大化。
/// - 没有历史宽高时，才按主显示器逻辑像素宽度判断是否最大化。
///
/// 边界条件：
/// - 显示器宽度读取失败时按大屏窗口化处理，避免在图形环境信息不完整时强行最大化。
fn decide_main_window_startup(
    saved_size: Option<MainWindowSizePreference>,
    primary_display_width: Option<f32>,
) -> MainWindowStartupDecision {
    if let Some(saved_size) = saved_size
        && !saved_size_looks_maximized(saved_size, primary_display_width)
    {
        return MainWindowStartupDecision::Remembered(saved_size);
    }

    if primary_display_width.is_some_and(|width| width < SMALL_SCREEN_MAXIMIZED_WIDTH_THRESHOLD) {
        MainWindowStartupDecision::DefaultMaximized
    } else {
        MainWindowStartupDecision::DefaultWindowed
    }
}

/// 把主窗口启动策略转换成 GPUI 窗口边界。
///
/// 业务意图：
/// - 历史宽高和大屏默认都以居中窗口打开，不恢复上次位置。
/// - 小屏默认最大化时仍把 1600x900 作为恢复尺寸交给 GPUI，用户退出最大化后能得到稳定默认宽高。
fn main_window_bounds_for_decision(
    decision: MainWindowStartupDecision,
    display_id: Option<DisplayId>,
    app: &App,
) -> WindowBounds {
    match decision {
        MainWindowStartupDecision::Remembered(saved_size) => {
            WindowBounds::Windowed(Bounds::centered(
                display_id,
                size(px(saved_size.width), px(saved_size.height)),
                app,
            ))
        }
        MainWindowStartupDecision::DefaultWindowed => WindowBounds::Windowed(Bounds::centered(
            display_id,
            size(px(MAIN_WINDOW_WIDTH), px(MAIN_WINDOW_HEIGHT)),
            app,
        )),
        MainWindowStartupDecision::DefaultMaximized => WindowBounds::Maximized(Bounds::centered(
            display_id,
            size(px(MAIN_WINDOW_WIDTH), px(MAIN_WINDOW_HEIGHT)),
            app,
        )),
    }
}

/// 读取当前主显示器的逻辑像素宽度。
///
/// 跨平台约束：
/// - GPUI 负责把 macOS Retina、Windows 缩放和平台显示器坐标转换为逻辑像素。
/// - 如果应用启动早期无法取得主显示器，则返回 `None`，由启动策略回退到固定窗口化。
fn primary_display_width(app: &App) -> Option<f32> {
    app.primary_display()
        .map(|display| display.bounds().size.width / px(1.0))
}

/// 读取当前主显示器 ID。
///
/// 业务意图：
/// - GPUI 创建窗口和计算居中位置都支持 display id；显式使用同一个主显示器可以避免多屏环境下
///   “按一个屏幕计算中心、实际在另一个屏幕打开”导致窗口看起来靠左。
fn primary_display_id(app: &App) -> Option<DisplayId> {
    app.primary_display().map(|display| display.id())
}

/// 构造主窗口启动边界。
///
/// 业务意图：
/// - 主入口只需要调用这个函数即可获得完整窗口策略，避免把配置读取、显示器判断和 GPUI 边界构造散落在 `main` 中。
fn build_main_window_bounds(app: &App) -> WindowBounds {
    let saved_size = load_main_window_size_preference();
    let decision = decide_main_window_startup(saved_size, primary_display_width(app));
    main_window_bounds_for_decision(decision, primary_display_id(app), app)
}

/// 校验模型配置表单的必填字段和 URL 协议。
///
/// 业务意图：
/// - 保存和测试都必须使用同一套校验，避免 UI 能保存但不能测试，或测试能发出非法 URL 请求。
/// - API Key 明确允许为空，以兼容本地 Ollama/vLLM 等不需要 Bearer 鉴权的服务。
fn validate_model_profile_fields(name: &str, base_url: &str, model: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("配置名称不能为空".to_string());
    }
    if base_url.trim().is_empty() {
        return Err("Base URL 不能为空".to_string());
    }
    if model.trim().is_empty() {
        return Err("模型 ID 不能为空".to_string());
    }
    let base_url = base_url.trim();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err("Base URL 必须以 http:// 或 https:// 开头".to_string());
    }
    Ok(())
}

/// 拼接模型测试请求 URL。
///
/// 边界条件：
/// - 用户可能在 Base URL 末尾输入一个或多个 `/`，拼接时统一去掉末尾斜杠，避免出现双斜杠路径。
fn model_test_chat_completions_url(base_url: &str) -> Result<String, String> {
    let base_url = base_url.trim();
    if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
        return Err("Base URL 必须以 http:// 或 https:// 开头".to_string());
    }
    Ok(format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        MODEL_TEST_CHAT_COMPLETIONS_PATH
    ))
}

/// 构造模型测试请求体。
///
/// 业务意图：
/// - 第一版只支持 OpenAI Chat Completions 兼容接口，固定发送最小 `ping` 请求，降低真实调用成本。
fn model_test_request_body(model: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model.trim(),
        "messages": [
            {
                "role": "user",
                "content": "ping"
            }
        ],
        "max_tokens": 1,
        "stream": false
    })
}

/// 返回测试请求需要发送的 Authorization 头。
///
/// 业务意图：
/// - API Key 非空才发送 Bearer header，避免本地模型服务因为无意义空鉴权头拒绝请求。
fn model_test_authorization_header(api_key: &str) -> Option<String> {
    let api_key = api_key.trim();
    (!api_key.is_empty()).then(|| format!("Bearer {api_key}"))
}

/// 判断模型测试响应是否包含 Chat Completions 的 choices。
///
/// 边界条件：
/// - 只要求 `choices` 是数组，不强制数组非空；部分兼容服务在 `max_tokens=1` 下仍可能返回空内容但格式有效。
fn model_test_response_has_choices(value: &serde_json::Value) -> bool {
    value
        .get("choices")
        .is_some_and(|choices| choices.is_array())
}

/// 将 HTTP 错误响应体裁剪成适合 UI 展示的短文本。
///
/// 业务意图：
/// - 兼容服务可能返回很长的 JSON 错误，设置窗口只需要展示可理解的前段原因，避免撑破状态栏。
fn model_test_http_error_body_snippet(body: &str) -> String {
    let normalized = body.replace(['\r', '\n'], " ");
    let trimmed = normalized.trim();
    if trimmed.chars().count() <= 160 {
        trimmed.to_string()
    } else {
        let snippet: String = trimmed.chars().take(160).collect();
        format!("{snippet}...")
    }
}

/// 执行一次 OpenAI 兼容模型测试请求。
///
/// 业务意图：
/// - 该函数只在后台执行器中调用，使用 blocking client 可以避免把额外异步运行时引入 GPUI 主线程。
/// - 请求不记录 API Key，也不会把完整请求头写入错误文案，降低明文 Key 暴露风险。
fn test_openai_compatible_model(profile: ModelProfile) -> Result<String, String> {
    validate_model_profile_fields(&profile.name, &profile.base_url, &profile.model)?;
    let url = model_test_chat_completions_url(&profile.base_url)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(MODEL_TEST_TIMEOUT_SECONDS))
        .build()
        .map_err(|error| format!("测试失败：创建 HTTP 客户端失败：{error}"))?;

    let mut request = client
        .post(url)
        .json(&model_test_request_body(&profile.model));
    if let Some(authorization) = model_test_authorization_header(&profile.api_key) {
        request = request.header(reqwest::header::AUTHORIZATION, authorization);
    }

    let response = request
        .send()
        .map_err(|error| format!("测试失败：请求接口失败：{error}"))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        let snippet = model_test_http_error_body_snippet(&body);
        if snippet.is_empty() {
            return Err(format!("测试失败：HTTP 状态码 {status}"));
        }
        return Err(format!("测试失败：HTTP 状态码 {status}，{snippet}"));
    }

    let value = response
        .json::<serde_json::Value>()
        .map_err(|error| format!("测试失败：响应 JSON 解析失败：{error}"))?;
    if model_test_response_has_choices(&value) {
        Ok("测试成功：模型接口可用".to_string())
    } else {
        Err("测试失败：响应 JSON 缺少 choices 字段".to_string())
    }
}

/// 日志功能页顶部操作栏的固定高度。
///
/// 业务意图：
/// - 根级文字工具栏已经迁移到左侧大导航；日志页仍需要加载日志和搜索入口，因此保留页内操作栏。
/// - 操作栏高度和 tab 页签高度一致，可以让日志页顶部控件和右侧 tab 栏的垂直节奏一致，减少首屏跳变感。
///
/// 边界条件：
/// - 当前不实现可换行操作栏；如果后续窗口宽度允许缩小，需要再定义窄宽度下的折叠策略。
const TOOLBAR_HEIGHT: f32 = LOG_TAB_BAR_HEIGHT;

/// 主窗口左侧大导航竖条宽度。
///
/// 业务意图：
/// - 用户要求最左侧固定一个只显示图标的大导航竖条，用于在日志分析、HPROF 解析和 AI 对话之间切换。
/// - 固定 56px 可以在 macOS 和 Windows 上容纳 40px 命中按钮，同时不明显挤压日志内容区。
const MAIN_NAV_WIDTH: f32 = 56.0;

/// 主导航按钮固定命中尺寸。
///
/// 业务意图：
/// - 导航按钮只显示图标，命中区域必须比图标更大，保证鼠标点击稳定且 hover 气泡容易触发。
const MAIN_NAV_BUTTON_SIZE: f32 = 40.0;

/// 主导航图标字号。
///
/// 业务意图：
/// - 20px 图标在 56px 竖条内足够清晰，同时不会显得比日志页操作图标过重。
const MAIN_NAV_ICON_SIZE: f32 = 20.0;

/// 主导航图标可视宽度。
///
/// 业务意图：
/// - Lucide 图标不同字形的宽度略有差异，固定可视宽度可以让所有入口在竖条中严格居中。
const MAIN_NAV_ICON_WIDTH: f32 = 20.0;

/// 主导航按钮之间的垂直间距。
///
/// 业务意图：
/// - 顶部三个主功能和底部通用入口都使用同一间距，保持竖向节奏稳定。
const MAIN_NAV_BUTTON_GAP: f32 = 8.0;

/// 主导航内边距。
///
/// 边界条件：
/// - 顶部和底部都保留同样内缩，避免导航按钮贴住系统标题栏或窗口底边。
const MAIN_NAV_PADDING: f32 = 8.0;

/// 主导航悬浮气泡宽度。
///
/// 业务意图：
/// - 气泡只展示功能名称，固定宽度可以避免不同中文名称长度导致 hover 时布局抖动。
const MAIN_NAV_TOOLTIP_WIDTH: f32 = 84.0;

/// 主导航悬浮气泡高度。
///
/// 业务意图：
/// - 气泡高度小于图标按钮命中区，既能和按钮垂直居中，也不会遮住相邻导航入口。
const MAIN_NAV_TOOLTIP_HEIGHT: f32 = 30.0;

/// 主导航悬浮气泡相对竖条右侧的间距。
///
/// 边界条件：
/// - 气泡需要离开图标命中区一点距离，避免鼠标在图标和气泡之间移动时频繁闪烁。
const MAIN_NAV_TOOLTIP_GAP: f32 = 8.0;

/// 主导航悬浮气泡相对按钮顶部或底部的垂直内缩。
///
/// 实现原因：
/// - 气泡由主窗口根节点覆盖绘制，不再是按钮子元素；独立常量能保证顶部入口和底部入口都仍与按钮视觉居中。
const MAIN_NAV_TOOLTIP_BUTTON_INSET: f32 = (MAIN_NAV_BUTTON_SIZE - MAIN_NAV_TOOLTIP_HEIGHT) / 2.0;

/// 主界面工具栏按钮的水平内边距。
///
/// 实现原因：
/// - 当前按钮是文字按钮，不再使用边框和实体背景，因此水平内边距需要更紧凑。
/// - 统一内边距可以保证三个入口在 macOS 和 Windows 字体渲染差异下仍保持一致点击区域。
const TOOLBAR_BUTTON_HORIZONTAL_PADDING: f32 = 8.0;

/// 主界面工具栏按钮的垂直内边距。
///
/// 边界说明：
/// - 工具栏高度和 tab 栏一致后，垂直内边距仍保持紧凑，避免文字和图标被裁切。
/// - 当前按钮不承载多行文本，也不展示快捷键提示，因此不需要动态高度。
const TOOLBAR_BUTTON_VERTICAL_PADDING: f32 = 4.0;

/// Lucide 图标字体在 GPUI 中使用的字体族名称。
///
/// 业务意图：
/// - `lucide-icons` 通过字体字形暴露图标，必须在渲染图标字符时指定该字体族。
/// - 显式定义字体族名称可以避免图标字符被系统默认字体渲染成方块或错误符号。
///
/// 边界条件：
/// - 字体数据来自 `lucide-icons` 依赖内置的 `LUCIDE_FONT_BYTES`，启动时注册到 GPUI 文本系统。
/// - 如果未来改为 SVG 图标或其它图标库，应同步替换字体注册和图标渲染逻辑。
const LUCIDE_FONT_FAMILY: &str = "lucide";

/// 工具栏按钮图标的固定宽度。
///
/// 业务意图：
/// - 三个 Lucide 图标字形宽度可能不同，固定图标区域可以让按钮文字起点更稳定。
/// - 该宽度只约束图标可视区域，不改变按钮整体点击区域。
///
/// 边界条件：
/// - 当前图标尺寸与工具栏高度配套，避免在 macOS 和 Windows 字体度量差异下发生裁切。
/// - 固定宽度只约束单个图标，不处理多个图标、徽标或加载状态。
const TOOLBAR_BUTTON_ICON_WIDTH: f32 = 16.0;

/// 工具栏按钮图标字号。
///
/// 业务意图：
/// - Lucide 字体图标需要使用接近 16px 的字号才能保持清晰线条和按钮视觉平衡。
/// - 单独定义字号便于后续统一调整工具栏密度。
///
/// 边界条件：
/// - 字号不能超过工具栏高度，否则部分平台的字体上升/下降空间可能导致图标裁切。
const TOOLBAR_BUTTON_ICON_SIZE: f32 = 16.0;

/// 左侧内容区域的默认宽度。
///
/// 业务意图：
/// - 用户明确要求左右两栏布局默认左侧区域宽度为 300px。
/// - 当前不持久化用户拖动后的宽度，避免在配置存储位置和权限未定义前写入用户环境。
const LEFT_PANEL_DEFAULT_WIDTH: f32 = 300.0;

/// 左侧内容区域可拖动到的最小宽度。
///
/// 业务意图：
/// - 左侧区域后续预计承载日志来源、文件列表或过滤条件，过窄会导致基础交互不可用。
/// - 用户未定义最小宽度，因此采用保守的桌面客户端默认值，避免拖动后面板完全消失。
///
/// 边界条件：
/// - 该值只约束当前进程内拖动行为，不代表后续设置持久化或布局配置规则。
const LEFT_PANEL_MIN_WIDTH: f32 = 180.0;

/// 右侧内容区域可保留的最小宽度。
///
/// 业务意图：
/// - 右侧区域后续预计承载日志内容主视图，需要保留足够宽度避免拖动左栏挤占主体区域。
/// - 用户未定义右侧最小宽度，因此当前用 320px 作为临时保护边界。
///
/// 边界条件：
/// - 如果后续右侧加入表格、详情面板或诊断视图，应重新定义最小宽度和窄屏折叠规则。
const RIGHT_PANEL_MIN_WIDTH: f32 = 320.0;

/// 左右内容区域之间的可拖动命中区域宽度。
///
/// 业务意图：
/// - 用户要求分隔条视觉上改成细线，但仍然需要支持拖动改变宽度。
/// - 因此这里保留 6px 透明命中区域，让鼠标更容易命中；命中区通过覆盖在右侧内容上的透明层实现，
///   不再占用额外布局宽度，避免分割线右侧出现白色视觉缝隙。
///
/// 边界条件：
/// - 当前仅支持鼠标拖动，不支持键盘调整、双击重置或触控手势。
/// - 命中区域不参与 flex 布局，拖动边界计算只需要扣除真实可见线条宽度。
const SPLITTER_HIT_WIDTH: f32 = 6.0;

/// 左右内容区域之间分割线的可见宽度。
///
/// 业务意图：
/// - 用户反馈上一版分隔条太粗，因此可见线条收敛为 1px。
/// - 视觉宽度与拖拽命中宽度分离，保证界面轻量的同时不牺牲可用性。
///
/// 边界条件：
/// - 当前线条颜色固定；如果后续加入主题系统，应把颜色纳入主题令牌而不是继续硬编码。
const SPLITTER_VISIBLE_WIDTH: f32 = 1.0;

/// 覆盖在右侧内容上的透明拖动命中宽度。
///
/// 业务意图：
/// - 可见分割线只占 1px，剩余命中区域放到右侧内容之上，让右侧内容视觉上紧贴分割线。
const SPLITTER_OVERLAY_HIT_WIDTH: f32 = SPLITTER_HIT_WIDTH - SPLITTER_VISIBLE_WIDTH;

/// 左侧目录树标题区域高度。
///
/// 业务意图：
/// - 左侧区域用于展示真实加载后的日志目录树，标题区域帮助用户识别当前面板语义。
/// - 当前标题不展示真实绝对路径，避免路径脱敏和窄面板排版规则未定义前暴露过多信息。
const LOG_TREE_HEADER_HEIGHT: f32 = 34.0;

/// 左侧目录树文本字号。
///
/// 业务意图：
/// - 目录树用于快速扫描文件层级、压缩包成员和错误节点，12px 可以在固定侧栏宽度内提高信息密度。
/// - 字号独立于右侧日志正文的用户设置，避免用户调大正文后影响目录树和 tab 页签的导航密度。
///
/// 边界条件：
/// - 行高仍由 `LOG_TREE_ROW_HEIGHT` 控制，字号变化不能破坏 `uniform_list` 的等高行假设。
/// - macOS 和 Windows 字体度量不同，因此使用显式像素字号，避免主题级 `text_sm` 映射差异。
const LOG_TREE_FONT_SIZE: f32 = 12.0;

/// 左侧目录树每行固定高度。
///
/// 业务意图：
/// - 固定行高可以让树结构在 macOS 和 Windows 字体渲染差异下保持稳定密度。
/// - 后续接入虚拟列表或大目录时，也可以基于固定行高计算滚动区域。
///
/// 边界条件：
/// - 当前未实现超长目录名的多行展示；长文本统一截断，避免挤压布局。
const LOG_TREE_ROW_HEIGHT: f32 = 28.0;

/// 左侧目录树首次加载后的默认展开深度。
///
/// 业务意图：
/// - 用户要求默认展开 2 级目录，因此深度 0 和深度 1 的可展开节点默认处于展开状态。
/// - 深度 2 的节点默认可见但不继续展开，避免大目录首次渲染时一次性暴露过多文件行。
///
/// 边界条件：
/// - 这里使用加载层提供的 `depth`，只影响初始 UI 展开状态，不裁剪真实扫描结果。
/// - 用户手动展开或收起后只在当前进程内生效，重新加载日志会重新套用默认展开规则。
const LOG_TREE_DEFAULT_EXPANDED_DEPTH: usize = 2;

/// 左侧目录树每一级层级缩进。
///
/// 业务意图：
/// - 树形结构依赖稳定缩进表达父子关系，16px 可以容纳展开箭头和文件夹图标。
/// - 缩进值固定，避免不同节点因为文本长度不同导致层级难以扫描。
const LOG_TREE_ROW_INDENT: f32 = 16.0;

/// 左侧目录树行内容的基础水平内边距。
///
/// 业务意图：
/// - 给树节点与面板边缘之间保留基础留白，避免图标贴边影响可读性。
/// - 缩进会叠加在该基础内边距之上，因此根节点和子节点都保持一致起点规则。
const LOG_TREE_ROW_HORIZONTAL_PADDING: f32 = 10.0;

/// 左侧目录树展开箭头占位宽度。
///
/// 业务意图：
/// - 文件节点没有展开箭头，但仍需要占用同等宽度，保证文件夹和文件名称左边缘对齐。
/// - 该值只影响视觉布局，不代表当前节点支持真实展开或收起。
const LOG_TREE_CHEVRON_WIDTH: f32 = 14.0;

/// 左侧目录树展开箭头字号。
///
/// 边界条件：
/// - 箭头图标必须小于行高，避免在低 DPI 或 Windows 字体度量差异下被裁切。
const LOG_TREE_CHEVRON_SIZE: f32 = 13.0;

/// 左侧目录树文件夹和文件图标占位宽度。
///
/// 业务意图：
/// - 固定图标宽度可以让节点文字起点稳定，提升目录树快速扫描体验。
const LOG_TREE_ITEM_ICON_WIDTH: f32 = 16.0;

/// 左侧目录树文件夹和文件图标字号。
///
/// 边界条件：
/// - 图标字号与行高配套，避免树节点在不同平台字体度量下出现垂直裁切。
const LOG_TREE_ITEM_ICON_SIZE: f32 = 15.0;

/// 左侧目录树滚动条可见宽度。
///
/// 业务意图：
/// - 左侧目录树也使用虚拟列表，文件较多时需要稳定显示滚动位置，避免用户误以为列表只展示当前屏内容。
/// - 宽度和右侧日志正文滚动条保持一致，降低左右区域的视觉差异。
const LOG_TREE_SCROLLBAR_WIDTH: f32 = 6.0;

/// 左侧目录树滚动条最小滑块高度。
///
/// 边界条件：
/// - 大目录中按比例计算出的滑块可能过小，最小高度保证鼠标仍能命中并拖动。
const LOG_TREE_SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 36.0;

/// 左侧目录树滚动条距离面板边缘的内缩。
///
/// 业务意图：
/// - 滚动条贴近右侧但不压住分割线，避免目录树和右侧内容边界混在一起。
const LOG_TREE_SCROLLBAR_PADDING: f32 = 3.0;

/// 右侧日志 tab 栏高度。
///
/// 业务意图：
/// - tab 栏需要容纳多个已打开日志文件的切换入口，同时不抢占日志正文可视高度。
/// - 固定高度便于右侧主体区域继续使用等高虚拟列表渲染日志行。
const LOG_TAB_BAR_HEIGHT: f32 = 34.0;

/// 右侧日志 tab 页签文本字号。
///
/// 业务意图：
/// - tab 页签主要承担多文件导航，12px 可以容纳更多文件名，减少横向滚动频率。
/// - 该字号只作用于 tab 页签本身，不影响日志正文、编码工具条或右键菜单。
///
/// 边界条件：
/// - tab 高度继续由 `LOG_TAB_BAR_HEIGHT` 决定，避免字号调整引发顶部布局抖动。
/// - 显式像素字号可以让 macOS 和 Windows 上页签标题接近一致。
const LOG_TAB_FONT_SIZE: f32 = 12.0;

/// tab 栏两侧滚动箭头按钮宽度。
///
/// 业务意图：
/// - 用户要求打开日志较多时 tab 可以横向滚动，并在两边显示滚动箭头。
/// - 按钮固定宽度可以让 tab 可视区域在滚动前后保持稳定。
const LOG_TAB_SCROLL_BUTTON_WIDTH: f32 = 28.0;

/// tab 栏每次点击滚动箭头时移动的距离。
///
/// 业务意图：
/// - 该值约等于两个中等长度 tab 的宽度，点击次数和定位速度比较平衡。
const LOG_TAB_SCROLL_STEP: f32 = 180.0;

/// tab 内关闭图标按钮宽度。
///
/// 业务意图：
/// - 关闭按钮需要稳定命中区，避免用户只能点到很小的 `X` 字形。
const LOG_TAB_CLOSE_BUTTON_WIDTH: f32 = 18.0;

/// 右侧日志文档工具条高度。
///
/// 业务意图：
/// - 当前工具条只承载编码切换和当前文件状态，后续如果加入搜索或跳转行号也应从这里扩展。
const LOG_DOCUMENT_TOOLBAR_HEIGHT: f32 = 36.0;

/// 右侧日志正文每行固定高度。
///
/// 业务意图：
/// - 日志正文使用 `uniform_list` 进行虚拟渲染，必须保持每行高度一致，才能在大文件滚动时稳定计算可见区间。
const LOG_VIEWER_ROW_HEIGHT: f32 = 22.0;

/// 分页日志每帧额外渲染的缓冲行数。
///
/// 业务意图：
/// - 分页模式不再把完整行数交给 GPUI 做绝对像素滚动，而是只渲染视口附近行。
/// - 缓冲行用于覆盖小幅滚动和首帧视口尺寸尚未回填的情况，避免边缘出现空白。
const PAGED_LOG_RENDER_BUFFER_ROWS: usize = 8;

/// 分页日志首帧视口高度未知时使用的保守行数。
///
/// 边界条件：
/// - `ScrollHandle::bounds` 需要经过一帧布局才会写入；首帧用固定行数可以先展示内容，下一帧再按真实视口收敛。
const PAGED_LOG_FALLBACK_VISIBLE_ROWS: usize = 180;

/// 分页日志横向宽度估算的等宽字符比例。
///
/// 业务意图：
/// - 分页模式不能再依赖 `uniform_list` 测量完整最长行，否则会重新引入超大虚拟坐标。
/// - JetBrains Mono 的常见字符宽度约为字号的六成；这里只用于横向滚动范围估算，真实文本命中仍由 GPUI shaping 计算。
const PAGED_LOG_MONOSPACE_WIDTH_RATIO: f32 = 0.62;

/// 日志查看器行号列固定宽度。
///
/// 业务意图：
/// - 行号和正文分开渲染，便于用户定位日志上下文，同时避免行号挤压正文起点。
const LOG_VIEWER_LINE_NUMBER_MIN_WIDTH: f32 = 46.0;

/// 日志查看器行号列最大宽度。
///
/// 业务意图：
/// - 超大分页日志可能有数千万行，行号列必须允许 8 位以上数字完整显示，避免左侧高位被裁切。
/// - 仍保留上限，防止异常索引行数把正文区域完全挤出可视范围。
const LOG_VIEWER_LINE_NUMBER_MAX_WIDTH: f32 = 128.0;

/// 日志查看器行号单个数字的估算宽度。
///
/// 业务意图：
/// - GPUI 当前没有在渲染前测量等宽行号文本的必要性，使用稳定估算即可让常见行数下列宽更紧凑。
const LOG_VIEWER_LINE_NUMBER_DIGIT_WIDTH: f32 = 7.0;

/// 日志查看器行号列右侧留白和边框的估算宽度。
///
/// 业务意图：
/// - 行号需要和边框保持一点距离，避免数字贴线影响扫描。
const LOG_VIEWER_LINE_NUMBER_PADDING_WIDTH: f32 = 18.0;

/// 日志正文相对行号列右侧的水平内边距。
///
/// 业务意图：
/// - 行号列固定在左侧，正文需要留出少量间距，避免文本贴着行号分割线显示。
/// - 文本选择命中测试也必须使用同一个偏移，保证鼠标选中的列和实际渲染起点一致。
const LOG_VIEWER_TEXT_LEFT_PADDING: f32 = 8.0;

/// 日志正文使用的等宽字体族。
///
/// 业务意图：
/// - 日志通常包含时间戳、线程名和列对齐字段，等宽字体能提升扫描和对比效率。
/// - 用户指定使用 JetBrains Mono，并要求内置到程序中，因此启动时会注册随项目打包的字体字节。
const LOG_VIEWER_FONT_FAMILY: &str = "JetBrains Mono";

/// 日志查看器中制表符按固定 4 列展开。
///
/// 业务意图：
/// - 一些日志用 `\t` 分隔字段，VS Code 等编辑器会按 tab stop 展开，所以列之间看起来有稳定间隔。
/// - GPUI 当前直接渲染 `\t` 时宽度不符合日志阅读预期，字段会贴在一起；这里在显示层展开为空格。
///
/// 边界条件：
/// - 这里只改变视觉显示，不改原始日志文本；复制、搜索、另存为仍保留文件里的真实 `\t`。
/// - tab stop 按用户要求固定为 4 列，暂不做设置项，避免影响现有信息密度。
const LOG_VIEWER_TAB_WIDTH: usize = 4;

/// 内置 JetBrains Mono Regular 字体数据。
///
/// 业务意图：
/// - 日志正文必须使用 JetBrains Mono，不能依赖用户系统是否安装该字体。
/// - `include_bytes!` 会把字体文件编译进可执行产物，macOS 和 Windows 启动时通过同一套 GPUI 字体注册流程加载。
///
/// 边界条件：
/// - 当前只内置 Regular 字重；日志正文高亮仍通过字体系统模拟粗体，后续若要求更高字重质量可追加 Bold 字体文件。
const JETBRAINS_MONO_REGULAR_FONT_BYTES: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

/// 日志正文默认字号。
///
/// 边界条件：
/// - 用户要求默认字号为 12px；当前继续沿用 22px 行高，保证单屏可见行数和滚动计算稳定。
/// - 设置页只调整文字字号，不改变固定行高和信息密度。
const LOG_VIEWER_DEFAULT_FONT_SIZE: f32 = 12.0;

/// 日志正文可设置的最小字号。
///
/// 边界条件：
/// - 过小字体会导致中文、英文和符号在长时间阅读时难以辨认，因此设置页不允许继续减小。
const LOG_VIEWER_MIN_FONT_SIZE: f32 = 10.0;

/// 日志正文可设置的最大字号。
///
/// 边界条件：
/// - 当前日志行高仍保持固定密度；过大字体可能与行高冲突并影响虚拟列表测量，因此先限制到 20px。
const LOG_VIEWER_MAX_FONT_SIZE: f32 = 20.0;

/// 日志字号设置的单次调整步长。
///
/// 业务意图：
/// - 1px 步进足够细，用户可以在不改变布局密度的前提下微调阅读舒适度。
const LOG_VIEWER_FONT_SIZE_STEP: f32 = 1.0;

/// 日志正文滚动条可见宽度。
///
/// 业务意图：
/// - GPUI `uniform_list` 默认只处理滚动交互，不预留可见滚动条；这里自绘轻量滚动条提示当前位置。
const LOG_VIEWER_SCROLLBAR_WIDTH: f32 = 6.0;

/// 日志正文滚动条最小滑块长度。
///
/// 边界条件：
/// - 超长或超宽日志中按比例计算出的滑块可能过小，最小长度保证用户仍能看到并拖动滚动位置。
const LOG_VIEWER_SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 36.0;

/// 日志正文滚动条距离内容边缘的内缩。
///
/// 业务意图：
/// - 让滚动条不贴到窗口边缘和 tab 内容边界，视觉上更轻量。
const LOG_VIEWER_SCROLLBAR_PADDING: f32 = 3.0;

/// 编码下拉框按钮宽度。
///
/// 业务意图：
/// - 编码选择器现在嵌入右侧状态信息中，宽度需要比旧版左侧独立控件更紧凑。
/// - 该宽度仍需容纳最长的 “UTF-8 BOM” 文案和下拉箭头。
const ENCODING_DROPDOWN_BUTTON_WIDTH: f32 = 94.0;

/// 编码下拉框弹层宽度。
///
/// 业务意图：
/// - 所有编码选项文案长度固定，使用稳定宽度避免菜单在不同平台字体下抖动。
const ENCODING_DROPDOWN_WIDTH: f32 = 116.0;

/// 编码下拉按钮高度。
///
/// 业务意图：
/// - 菜单弹层位置需要和按钮底边对齐，因此按钮高度集中成常量，避免渲染处和定位处写两份数字。
const ENCODING_DROPDOWN_BUTTON_HEIGHT: f32 = 24.0;

/// 编码下拉菜单距离按钮底边的垂直间隔。
///
/// 边界条件：
/// - 保留 2px 间隔可以让菜单边框和按钮边框不粘连。
const ENCODING_DROPDOWN_MENU_GAP: f32 = 2.0;

/// 编码下拉框单项高度。
///
/// 边界条件：
/// - 当前每项只包含单行中文或 ASCII 编码名，不需要多行布局。
const ENCODING_DROPDOWN_ITEM_HEIGHT: f32 = 28.0;

/// tab 右键菜单宽度。
///
/// 业务意图：
/// - 三个菜单项文案长度固定，使用稳定宽度可以避免菜单因为字体差异跳动。
const TAB_CONTEXT_MENU_WIDTH: f32 = 132.0;

/// tab 右键菜单单项高度。
///
/// 边界条件：
/// - 菜单项只显示单行中文命令，不承载图标或快捷键。
const TAB_CONTEXT_MENU_ITEM_HEIGHT: f32 = 30.0;

/// 搜索结果右键菜单宽度。
///
/// 业务意图：
/// - 搜索结果菜单只承载展开/收起两类批量操作，固定宽度可以和 tab 菜单保持一致的视觉节奏。
const SEARCH_RESULTS_CONTEXT_MENU_WIDTH: f32 = 132.0;

/// 搜索结果右键菜单单项高度。
///
/// 边界条件：
/// - 菜单项只显示单行中文命令，不承载二级菜单。
const SEARCH_RESULTS_CONTEXT_MENU_ITEM_HEIGHT: f32 = 30.0;

/// 加载日志来源菜单宽度。
///
/// 业务意图：
/// - Windows 原生选择器不能混选文件和目录，因此工具栏“加载日志”需要先给出两个明确入口。
/// - 菜单宽度要容纳“文件/压缩包”文案和图标，同时不遮挡后续工具栏按钮。
const LOAD_SOURCE_MENU_WIDTH: f32 = 164.0;

/// 加载日志来源菜单单项高度。
///
/// 业务意图：
/// - 与目录树右键菜单保持接近的点击高度，让鼠标操作体验一致。
const LOAD_SOURCE_MENU_ITEM_HEIGHT: f32 = 34.0;

/// 加载日志来源菜单到工具栏底部的间隔。
const LOAD_SOURCE_MENU_TOP_GAP: f32 = 4.0;

/// 加载日志来源菜单相对点击位置的横向回退。
///
/// 业务意图：
/// - 点击通常落在按钮文字或图标中部，菜单向左回退后能更自然地贴住“加载日志”按钮。
const LOAD_SOURCE_MENU_POINTER_BACKTRACK: f32 = 24.0;

/// 搜索对话框默认宽度。
///
/// 业务意图：
/// - 对话框需要同时容纳查询输入、范围切换和大小写开关，宽度固定可以让独立窗口尺寸稳定。
/// - 搜索对话框已经从主窗口浮层迁移为独立浮动窗口，因此不再参与右侧日志正文布局。
const SEARCH_DIALOG_WIDTH: f32 = 430.0;

/// 搜索对话框独立窗口默认高度。
///
/// 业务意图：
/// - 独立窗口需要一次性容纳“当前文件”和“当前目录”两种搜索模式下的控件，避免切换范围时窗口高度跳变。
/// - 当前目录模式会额外显示目标目录输入框，因此高度按较高形态预留。
///
/// 边界条件：
/// - 该窗口不可调整大小；如果后续增加搜索历史、正则等更多控件，应同步重新定义窗口高度策略。
const SEARCH_DIALOG_WINDOW_HEIGHT: f32 = 300.0;

/// 搜索关键字历史记录上限。
///
/// 业务意图：
/// - 搜索窗口再次打开时需要能恢复最近一次关键字，减少重复输入。
/// - 只保留最近 10 条，避免当前会话内频繁搜索导致状态无限增长；当前不持久化到磁盘，避免隐私规则未定义前保存用户日志关键字。
const SEARCH_QUERY_HISTORY_LIMIT: usize = 10;

/// 搜索关键字历史下拉按钮宽度。
///
/// 业务意图：
/// - 搜索框右侧需要一个独立、稳定的点击区域展开历史关键字，避免和文本输入区的光标定位互相干扰。
const SEARCH_HISTORY_DROPDOWN_BUTTON_WIDTH: f32 = 26.0;

/// 搜索关键字历史下拉菜单与搜索框的间距。
///
/// 业务意图：
/// - 菜单贴近搜索框但留出细微间隔，用户能明确它属于关键字输入框而不是下面的范围控件。
const SEARCH_HISTORY_DROPDOWN_GAP: f32 = 4.0;

/// 搜索关键字历史下拉菜单单项高度。
///
/// 业务意图：
/// - 历史关键字是短文本选择项，固定高度让最多 10 条历史在滚动容器里保持稳定命中区域。
const SEARCH_HISTORY_DROPDOWN_ITEM_HEIGHT: f32 = 28.0;

/// 搜索关键字历史下拉菜单最大高度。
///
/// 业务意图：
/// - 历史最多保存 10 条，但搜索窗口高度有限；菜单超过该高度后滚动，避免遮住整个窗口内容。
const SEARCH_HISTORY_DROPDOWN_MAX_HEIGHT: f32 = 176.0;

/// 设置窗口默认宽度。
///
/// 业务意图：
/// - 设置窗口需要容纳左侧页签和右侧统一表单布局；日志页包含多行线程堆栈过滤输入区，因此需要比早期设置窗口更宽。
/// - 模型页是“配置列表 + 表单 + 四个操作按钮”的两栏布局，宽度不足会导致按钮越过详情卡片边框，因此按该页的最小可用宽度取值。
/// - 固定宽度可以让独立窗口在 macOS 和 Windows 上保持稳定布局，不受系统字体度量差异影响。
const SETTINGS_WINDOW_WIDTH: f32 = 900.0;

/// 设置窗口默认高度。
///
/// 业务意图：
/// - 日志页需要直接粘贴线程堆栈，较高窗口可以减少输入区滚动，同时保留通用页紧凑布局。
/// - 模型页签需要容纳配置列表、表单和测试状态；固定高度配合页内滚动，避免不同页签切换时窗口跳动。
const SETTINGS_WINDOW_HEIGHT: f32 = 520.0;

/// 设置窗口左侧页签栏宽度。
///
/// 业务意图：
/// - 页签名称为中文短文本，固定宽度可以让右侧内容区宽度稳定，后续增加更多设置项时仍易于扫描。
const SETTINGS_TAB_SIDEBAR_WIDTH: f32 = 132.0;

/// 设置页线程过滤多行输入区高度。
///
/// 业务意图：
/// - 线程堆栈通常包含多行调用栈，输入区需要在设置窗口内提供足够预览空间，同时不能挤掉标题和说明。
const THREAD_ANALYSIS_FILTER_TEXTAREA_HEIGHT: f32 = 300.0;

/// 设置页线程过滤输入区单行高度。
///
/// 业务意图：
/// - 多行输入区使用等宽字体展示堆栈，固定行高便于鼠标命中、选区绘制和滚动内容高度计算保持一致。
const THREAD_ANALYSIS_FILTER_TEXT_LINE_HEIGHT: f32 = 18.0;

/// 搜索输入框高度。
///
/// 业务意图：
/// - 搜索框只承载单行查询词，不支持多行输入，因此固定高度可以简化键盘和点击焦点处理。
const SEARCH_INPUT_HEIGHT: f32 = 30.0;

/// 搜索窗口内容区内边距。
///
/// 业务意图：
/// - 搜索历史下拉菜单作为内容区绝对定位弹层，需要和输入框左、右边缘对齐；该值和搜索窗口内容容器的 `p_3` 保持一致。
const SEARCH_DIALOG_CONTENT_PADDING: f32 = 16.0;

/// 搜索结果面板默认高度。
///
/// 业务意图：
/// - 面板从右侧内容区底部弹出，默认高度需要能展示多条结果，同时保留上方日志上下文。
const SEARCH_RESULTS_PANEL_DEFAULT_HEIGHT: f32 = 260.0;

/// 搜索结果面板最小高度。
///
/// 边界条件：
/// - 继续允许用户拖小结果面板，但不能小到关闭按钮、摘要和至少一条结果不可见。
const SEARCH_RESULTS_PANEL_MIN_HEIGHT: f32 = 140.0;

/// 搜索结果面板相对右侧内容区的最大高度比例。
///
/// 业务意图：
/// - 用户要求结果面板可以上下拖动高度，但不能完全覆盖日志正文。
const SEARCH_RESULTS_PANEL_MAX_RATIO: f32 = 0.6;

/// 搜索结果面板拖拽条高度。
const SEARCH_RESULTS_PANEL_RESIZER_HEIGHT: f32 = 8.0;

/// 搜索结果标题栏顶部视觉留白。
///
/// 业务意图：
/// - 拖拽命中区需要保持足够高度，方便用户调整面板；但标题内容不应因此显得上边距过大。
/// - 将视觉留白和拖拽命中区拆开，标题栏能保持紧凑，同时顶部仍保留可拖动热区。
const SEARCH_RESULTS_PANEL_HEADER_TOP_PADDING: f32 = 3.0;

/// 搜索结果行固定高度。
///
/// 业务意图：
/// - 结果列表使用虚拟列表，固定行高可以避免大量命中时创建全部行元素。
const SEARCH_RESULT_ROW_HEIGHT: f32 = 34.0;

/// 搜索结果面板滚动条可见宽度。
///
/// 业务意图：
/// - 搜索结果可能包含大量历史记录、文件分组和命中明细，面板也需要像日志正文一样提供稳定可见的滚动位置提示。
/// - 宽度沿用日志正文滚动条的视觉尺度，避免底部面板看起来像另一套控件体系。
const SEARCH_RESULTS_SCROLLBAR_WIDTH: f32 = 6.0;

/// 搜索结果面板滚动条最小滑块长度。
///
/// 边界条件：
/// - 大量结果会让按比例计算的滑块非常短，最小长度保证用户仍能稳定点击和拖动。
const SEARCH_RESULTS_SCROLLBAR_MIN_THUMB_HEIGHT: f32 = 36.0;

/// 搜索结果面板滚动条距离列表边缘的内缩。
///
/// 业务意图：
/// - 右侧保留轻微内缩，让滚动条不贴边，并和日志正文、左侧目录树滚动条的视觉位置保持一致。
const SEARCH_RESULTS_SCROLLBAR_PADDING: f32 = 3.0;

/// `Ctrl+C` 在部分 macOS 输入路径下对应的 ASCII 控制字符。
///
/// 业务意图：
/// - 日志正文复制和搜索快捷键使用同一套全局键盘入口；复制也需要兼容 Control 字母键被平台编码成控制字符的情况。
const CONTROL_C_CODE: &str = "\u{3}";

/// `Ctrl+V` 在部分 macOS 输入路径下对应的 ASCII 控制字符。
///
/// 业务意图：
/// - 搜索关键字和目录输入框需要支持粘贴；日志查看器只读，因此在查看器中粘贴会打开搜索窗口并填入剪贴板文本。
const CONTROL_V_CODE: &str = "\u{16}";

/// `Ctrl+X` 在部分平台输入路径下对应的 ASCII 控制字符。
///
/// 业务意图：
/// - 线程过滤多行输入区需要支持剪切；不同系统可能把 `Ctrl+X` 表示为字母 `x` 或控制字符。
const CONTROL_X_CODE: &str = "\u{18}";

/// `Ctrl+A` 在部分平台输入路径下对应的 ASCII 控制字符。
///
/// 业务意图：
/// - 搜索关键字和目录输入框需要支持全选；不同系统可能把 `Ctrl+A` 表示为字母 `a` 或控制字符。
/// - 与复制快捷键保持同一套兼容策略，避免 Windows/macOS 键盘路径出现只在某个平台生效的问题。
const CONTROL_A_CODE: &str = "\u{1}";

/// 日志页操作栏按钮的声明式配置。
///
/// 业务意图：
/// - 将按钮标签和图标绑定在一起，避免日志页操作栏渲染代码里出现分散的硬编码。
/// - 后续如果按钮需要权限、禁用态或快捷键，可以在这个结构上扩展字段。
///
/// 边界条件：
/// - 当前只包含静态字符串，所有按钮在进程生命周期内固定不变。
/// - “加载日志”已绑定真实系统选择器和路径扫描流程；“搜索”复用现有独立搜索窗口逻辑。
struct ToolbarAction {
    /// 按钮前置图标。
    ///
    /// 业务意图：
    /// - 使用 Lucide 图标库提供的类型安全枚举，替换上一版难看的纯文本符号。
    /// - 图标语义需要和按钮动作一致，帮助用户快速识别入口类别。
    icon: Icon,

    /// 按钮显示文案。
    ///
    /// 业务意图：
    /// - 文案直接来自用户当前需求，后续如需国际化或快捷键提示再统一抽象。
    label: &'static str,
}

/// 日志页操作栏按钮列表。
///
/// 业务意图：
/// - “加载日志”使用 `FileText`，表达日志文本文件入口。
/// - “搜索”使用 `Search`，提供鼠标入口打开搜索窗口，避免快捷键异常时用户无法触达搜索能力。
///
/// 边界条件：
/// - 当前图标依赖启动时注册的 Lucide 字体；如果字体注册失败，启动阶段会直接暴露错误。
const TOOLBAR_ACTIONS: &[ToolbarAction] = &[
    ToolbarAction {
        icon: Icon::FileText,
        label: "加载日志",
    },
    ToolbarAction {
        icon: Icon::Search,
        label: "搜索",
    },
];

/// 已加载日志目录树在 UI 层的交互状态。
///
/// 业务意图：
/// - `LoadedLogTree` 是加载模块输出的完整树数据，不能直接混入展开/收起这种界面会话状态。
/// - 这里集中保存展开节点集合和可见行缓存，让目录树可以支持折叠，同时为虚拟列表提供稳定数据源。
///
/// 边界条件：
/// - 展开集合只保存当前加载结果内部的节点 ID，重新加载后必须重新初始化，不能跨加载复用。
/// - 可见行缓存只包含当前应该渲染的行，不影响完整树数据；节点点击读取正文时仍应回到完整树来源模型。
struct LoadedLogTreeState {
    /// 加载层生成的完整目录树。
    ///
    /// 业务意图：
    /// - 完整树用于在用户切换展开状态时重新计算可见行。
    /// - 后续接入节点点击时，也应从完整树继续补充来源定位信息，而不是只依赖可见行。
    tree: LoadedLogTree,

    /// 当前处于展开状态的节点 ID 集合。
    ///
    /// 业务意图：
    /// - 使用集合可以让展开/收起切换保持 O(1) 查询，避免滚动渲染时反复线性查找。
    /// - 只记录展开节点；未出现的可展开节点视为收起。
    expanded_node_ids: HashSet<usize>,

    /// 当前需要展示给目录树虚拟列表的可见行。
    ///
    /// 业务意图：
    /// - 文件很多时不能把完整树每一行都渲染成 GPUI 元素，否则滚动会卡顿。
    /// - 先按展开状态算出可见行，再交给 `uniform_list` 按可视区间懒渲染。
    visible_rows: Vec<LoadedLogTreeRow>,

    /// 当前加载结果是否只包含一个可打开日志来源。
    ///
    /// 业务意图：
    /// - 用户加载单个日志时应直接进入右侧浏览，不再显示只有一个文件的左侧树。
    /// - 目录或压缩包内部如果最终只有一个可读日志，也按同一规则处理；多文件仍保留树用于选择。
    ///
    /// 边界条件：
    /// - 加载过程中出现错误时不隐藏树，避免错误节点被自动打开流程吞掉，用户仍能看到失败原因。
    /// - 只统计 `File` 节点且必须带有 `LogFileSource`，目录、压缩包容器、符号链接和错误节点不算可打开日志。
    single_log_source: Option<LogFileSource>,
}

impl LoadedLogTreeState {
    /// 根据完整加载结果创建 UI 交互状态。
    ///
    /// 业务意图：
    /// - 首次加载时按用户要求默认展开 2 级目录，避免大目录一次性完全展开导致首屏过载。
    /// - 初始化后立即构建可见行缓存，保证标题摘要、虚拟列表数量和点击状态同步。
    fn new(tree: LoadedLogTree) -> Self {
        let expanded_node_ids = tree
            .rows
            .iter()
            .filter(|row| row.has_children && row.depth < LOG_TREE_DEFAULT_EXPANDED_DEPTH)
            .map(|row| row.id)
            .collect();
        let single_log_source = Self::single_log_source_for_tree(&tree);

        let mut state = Self {
            tree,
            expanded_node_ids,
            visible_rows: Vec::new(),
            single_log_source,
        };
        state.rebuild_visible_rows();
        state
    }

    /// 返回整棵加载树中唯一可打开日志来源。
    ///
    /// 业务意图：
    /// - 加载单个普通文件、只含一个日志的目录、只含一个日志成员的压缩包时，主界面可以跳过左侧树并自动打开日志。
    ///
    /// 边界条件：
    /// - 如果存在扫描错误，保持 `None`，让左侧树继续展示错误行，避免用户误以为目录已完整加载。
    /// - 如果发现两个及以上可打开文件，立即返回 `None`，避免自动打开其中任意一个造成选择歧义。
    fn single_log_source_for_tree(tree: &LoadedLogTree) -> Option<LogFileSource> {
        if tree.error_count > 0 {
            return None;
        }

        let mut single_source: Option<LogFileSource> = None;
        for row in &tree.rows {
            if row.kind != LogTreeEntryKind::File {
                continue;
            }

            let Some(source) = row.source.clone() else {
                continue;
            };
            if single_source.replace(source).is_some() {
                return None;
            }
        }

        single_source
    }

    /// 返回当前加载结果中唯一可打开日志来源。
    ///
    /// 业务意图：
    /// - 该值在构建 UI 状态时缓存，渲染每一帧不需要重新遍历大目录树。
    fn single_log_source(&self) -> Option<LogFileSource> {
        self.single_log_source.clone()
    }

    /// 清理加载树持有的临时物化路径。
    ///
    /// 业务意图：
    /// - 7Z 在加载阶段会把内部成员物化成本地文件，重新加载日志后旧树不再可见，应释放对应磁盘空间。
    /// - 清理目录失败不影响 UI 状态切换，异常退出残留由下次启动的过期清理兜底。
    fn cleanup_temporary_paths(&self) {
        for path in &self.tree.temporary_paths {
            if path.is_dir() {
                let _ = fs::remove_dir_all(path);
            } else {
                cleanup_materialized_file(path);
            }
        }
    }

    /// 返回标题区域展示的加载摘要。
    ///
    /// 边界条件：
    /// - 摘要来自加载层统计，表达完整树节点数量，而不是当前可见行数量。
    fn summary(&self) -> &str {
        &self.tree.summary
    }

    /// 判断指定节点是否处于展开状态。
    ///
    /// 业务意图：
    /// - 渲染可见行时需要根据该状态选择向右或向下的展开箭头。
    fn is_expanded(&self, node_id: usize) -> bool {
        self.expanded_node_ids.contains(&node_id)
    }

    /// 如果指定压缩包节点内部只有一个可打开文件，则返回该文件来源。
    ///
    /// 业务意图：
    /// - 用户从左侧树点击压缩包文件时，如果压缩包内部只有一个日志文件，直接打开正文比先展开再点文件更符合预期。
    /// - 该规则只在 UI 交互层生效，不改变加载层的目录树结构，后续仍可以展示完整压缩包内容。
    ///
    /// 边界条件：
    /// - 只有 `Archive` 节点会触发该规则；普通目录即使只有一个文件也保持展开/收起行为。
    /// - “只有一个文件”按可打开的文件节点统计，目录节点不计数；如果发现两个及以上文件则返回 `None`。
    /// - 错误节点和符号链接不作为可打开日志来源，避免把不可读条目误当作候选文件。
    fn single_file_source_for_archive(&self, node_id: usize) -> Option<LogFileSource> {
        let archive_index = self
            .tree
            .rows
            .iter()
            .position(|row| row.id == node_id && row.kind == LogTreeEntryKind::Archive)?;
        let archive_depth = self.tree.rows[archive_index].depth;
        let mut single_source: Option<LogFileSource> = None;

        for row in self.tree.rows.iter().skip(archive_index + 1) {
            if row.depth <= archive_depth {
                break;
            }

            if row.kind != LogTreeEntryKind::File {
                continue;
            }

            let source = row.source.clone()?;
            if single_source.replace(source).is_some() {
                return None;
            }
        }

        single_source
    }

    /// 切换某个可展开节点的展开状态。
    ///
    /// 业务意图：
    /// - 文件夹点击后需要立即展开或收起子树，并刷新虚拟列表的可见行数量。
    ///
    /// 边界条件：
    /// - 非文件夹或没有子节点的节点不能进入展开集合，避免 UI 状态出现无效 ID。
    /// - 如果传入的 ID 不属于当前树，函数保持静默，避免异步加载切换后旧事件导致崩溃。
    fn toggle_node(&mut self, node_id: usize) {
        let Some(row) = self.tree.rows.iter().find(|row| row.id == node_id) else {
            return;
        };

        if !row.has_children {
            return;
        }

        if !self.expanded_node_ids.remove(&node_id) {
            self.expanded_node_ids.insert(node_id);
        }

        self.rebuild_visible_rows();
    }

    /// 重新按展开状态计算虚拟列表可见行。
    ///
    /// 业务意图：
    /// - 完整目录树是前序扁平结构，收起某个节点时，其后连续的更深层级行都应该隐藏。
    /// - 使用一次线性扫描构建缓存，避免每次虚拟列表渲染可见范围时都重复计算父节点展开链。
    ///
    /// 边界条件：
    /// - 收起父节点后，即使子节点 ID 仍在展开集合中也不会显示；再次展开父节点时会恢复子节点原状态。
    /// - 错误节点、文件节点和符号链接没有子节点，不参与展开控制。
    fn rebuild_visible_rows(&mut self) {
        self.visible_rows.clear();

        let mut collapsed_depth: Option<usize> = None;
        for row in &self.tree.rows {
            if let Some(depth) = collapsed_depth {
                if row.depth > depth {
                    continue;
                }

                collapsed_depth = None;
            }

            self.visible_rows.push(row.clone());

            if row.has_children && !self.expanded_node_ids.contains(&row.id) {
                collapsed_depth = Some(row.depth);
            }
        }
    }
}

/// 左侧日志目录树当前展示的数据状态。
///
/// 业务意图：
/// - 初始状态不展示左侧目录树，让主内容区完整显示“需要先加载日志”的提示。
/// - 加载中、加载成功和加载失败都由同一个枚举表达，避免 UI 层使用多个布尔字段组合出非法状态。
///
/// 边界条件：
/// - 当前不保留历史加载结果；用户再次选择来源时，新结果会替换旧目录树。
/// - 当前不支持取消正在进行的扫描任务，后续大目录扫描若需要取消必须补充任务句柄和状态规则。
/// - `Loaded` 变体保留完整树状态，避免仅为压缩枚举大小引入额外堆分配和借用复杂度。
#[allow(clippy::large_enum_variant)]
enum LogTreeLoadState {
    /// 尚未加载真实来源。
    Empty,
    /// 已经打开系统选择器并开始扫描用户选择的来源。
    Loading {
        /// 展示给用户的加载说明。
        ///
        /// 业务意图：
        /// - 说明当前是在扫描真实来源，而不是应用卡死或仍处于未加载状态。
        message: String,
    },
    /// 真实日志来源已经扫描完成，并已经构建 UI 交互状态。
    Loaded(LoadedLogTreeState),
    /// 加载流程发生致命错误，无法形成可展示结果。
    Failed {
        /// 中文错误说明。
        ///
        /// 边界条件：
        /// - 局部权限错误会作为树节点展示，只有系统对话框失败或加载流程整体失败才进入该状态。
        message: String,
    },
}

/// 右侧已经打开的日志 tab。
///
/// 业务意图：
/// - 每个日志文件对应一个 tab，tab 内保留来源、原始字节、编码选择和渲染状态。
/// - 同一来源重复点击时通过 `source_key` 定位已有 tab，避免重复打开同一份日志。
///
/// 边界条件：
/// - tab 状态只存在于当前进程内，不持久化到配置文件。
/// - 后台读取任务完成时如果 tab 已被关闭，会按 `id` 查找失败并静默忽略。
struct OpenLogTab {
    /// 当前进程内唯一 tab ID。
    id: usize,
    /// 日志文件来源。
    source: LogFileSource,
    /// 用于去重的稳定来源键。
    source_key: String,
    /// tab 上展示的短标题。
    title: String,
    /// 用户当前选择的编码策略。
    encoding_choice: EncodingChoice,
    /// 已读取的原始字节。
    ///
    /// 业务意图：
    /// - 自动检测失败时仍保留原始字节，用户手动切换编码可以直接重新解码。
    /// - 读取失败时保持 `None`，因为没有可复用的正文数据。
    raw_bytes: Option<Arc<Vec<u8>>>,
    /// tab 当前正文状态。
    state: LogTabState,
    /// 当前 tab 的日志正文虚拟列表滚动句柄。
    ///
    /// 业务意图：
    /// - 每个 tab 独立保存滚动上下文，切换 tab 时不会把其它文件的滚动位置混进来。
    /// - 普通内存日志继续把该句柄绑定到 `uniform_list`；分页日志只复用它的横向兼容语义，真实纵向滚动由 `paged_scroll` 保存。
    scroll_handle: UniformListScrollHandle,
    /// 分页日志正文视口测量句柄。
    ///
    /// 业务意图：
    /// - 分页模式不能再让 GPUI 布局完整行数，因此需要单独跟踪正文视口 bounds，用于滚动条尺寸和鼠标命中坐标换算。
    /// - 该句柄不承载真实滚动偏移，避免 GPUI 在超大内容高度上使用 `f32` 坐标。
    paged_viewport_handle: ScrollHandle,
    /// 分页日志的逻辑滚动位置。
    ///
    /// 业务意图：
    /// - 超大日志的真实滚动距离可能达到数亿像素，必须用 `f64` 保存在应用状态中，渲染时再映射到视口内的小坐标。
    /// - 普通内存日志不读取该字段；切换编码或重新加载时必须重置。
    paged_scroll: PagedLogScrollState,
    /// 打开后需要滚动定位的目标行。
    ///
    /// 业务意图：
    /// - 搜索结果点击可能打开一个尚未读取完成的新 tab，目标行必须暂存到 tab 上，等后台解码成功后再滚动。
    /// - 使用 0 基行号与 `DecodedLogDocument.lines` 下标保持一致，避免 UI 展示行号和数据下标混用。
    pending_scroll_to_line: Option<usize>,
    /// 最近一次通过搜索结果跳转的命中行。
    ///
    /// 业务意图：
    /// - 点击搜索结果后不仅要滚动到目标位置，还要用背景色标记命中行，避免用户在密集日志中丢失上下文。
    /// - 该状态只属于当前 tab 的临时视觉反馈，不持久化，也不影响日志语法高亮。
    highlighted_search_line: Option<usize>,
    /// 当前 tab 内由用户手动打标的日志行。
    ///
    /// 业务意图：
    /// - 用户排查日志时经常需要在多个关键位置之间来回跳转，标记行只属于当前打开的 tab。
    /// - 使用 `BTreeSet` 保存 0 基行号，可以天然按行号排序，便于 `F2` 查找下一个标记并在末尾循环回到第一个。
    ///
    /// 边界条件：
    /// - 标记不写入配置、不跨会话保存；关闭 tab 或重新加载日志后自然清空。
    /// - 切换编码时保留标记，因为同一份原始日志只是重新解码，用户已经标出的行号仍有参考价值。
    marked_lines: BTreeSet<usize>,
    /// 最近一次通过 `F2` 跳转到的标记行。
    ///
    /// 业务意图：
    /// - 连续按 `F2` 时应从上一次跳转目标之后继续寻找，而不是每次都从当前视口顶部开始。
    /// - 当用户手动滚动到其它位置后，如果该行不再可见，下一次 `F2` 会重新以当前可视顶部为起点。
    last_marker_jump_line: Option<usize>,
    /// 当前 tab 内日志正文的只读文本选择范围。
    ///
    /// 业务意图：
    /// - 日志正文采用虚拟列表自绘，不是系统文本控件，GPUI 不会自动保存跨行选择状态。
    /// - 将选择范围绑定到 tab，可以让用户切换 tab 后仍保留当前文件的选择上下文，并支持复制和填入搜索框。
    ///
    /// 边界条件：
    /// - 选择范围使用行号和字符列，不使用 UTF-8 字节下标；复制时再转换为安全字节边界，避免中文被截断。
    /// - 重新解码、重新加载或打开失败时必须清理该字段，因为旧行列可能不再对应新文档。
    text_selection: Option<LogTextSelection>,
    /// 当前正在拖拽选择时的固定起点。
    ///
    /// 业务意图：
    /// - 鼠标按下确定锚点，后续移动只更新焦点，才能正确支持从下往上或从右往左反向选择。
    /// - 鼠标释放后清空该临时字段，但保留 `text_selection` 供复制和搜索预填使用。
    selection_drag_anchor: Option<LogTextPosition>,
}

/// 分页日志的逻辑滚动位置。
///
/// 业务意图：
/// - `top_px` 和 `left_px` 都以日志正文内容坐标表示，不直接交给 GPUI 布局系统。
/// - 渲染时只根据这两个值计算“当前视口附近有哪些真实行号”，从而避开几千万行带来的 `f32` 精度损失。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct PagedLogScrollState {
    /// 当前视口顶部对应的纵向内容偏移，单位为逻辑像素。
    top_px: f64,
    /// 当前视口左侧对应的横向内容偏移，单位为逻辑像素。
    left_px: f64,
}

/// 日志正文中的文本位置。
///
/// 业务意图：
/// - 自绘日志行没有系统 selection，因此需要一个轻量位置类型表达“第几行第几个字符”。
/// - `column` 使用 Unicode 字符序号而不是字节序号，保证中文、全角符号和 emoji 不会被拆成非法 UTF-8。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct LogTextPosition {
    /// 0 基日志行号，对应 `DecodedLogDocument.lines` 下标。
    line_index: usize,
    /// 0 基字符列，对应该行 `chars()` 序号。
    column: usize,
}

/// 日志正文只读选择范围。
///
/// 业务意图：
/// - `anchor` 表示鼠标按下位置，`focus` 表示当前拖拽位置；二者顺序不固定。
/// - 复制、渲染选区和填充搜索框前都通过 `normalized` 统一为从前到后的范围。
#[derive(Clone, Debug, PartialEq, Eq)]
struct LogTextSelection {
    /// 选择起始锚点。
    anchor: LogTextPosition,
    /// 选择当前焦点。
    focus: LogTextPosition,
}

impl LogTextSelection {
    /// 返回按文档顺序排列后的选择范围端点。
    ///
    /// 边界条件：
    /// - 用户可能从后往前拖选，因此不能假设 `anchor <= focus`。
    fn normalized(&self) -> (LogTextPosition, LogTextPosition) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// 判断当前选择是否为空。
    fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }
}

/// 单行日志渲染所需的输入数据。
///
/// 业务意图：
/// - 日志行渲染需要同时知道 tab、行号、正文、高亮、行号列和搜索定位状态；集中成结构体可以避免函数参数膨胀。
/// - 该结构只在当前 UI 帧内短暂使用，不保存到 tab 状态，避免复制日志正文或高亮状态的生命周期变复杂。
struct LogLineRenderData {
    /// 所属 tab ID，用于鼠标选择事件回写到正确 tab。
    tab_id: usize,
    /// 0 基日志行号。
    line_index: usize,
    /// 当前行原始正文。
    ///
    /// 业务意图：
    /// - 鼠标选择、复制和搜索都必须继续使用日志文件里的真实文本，不能因为显示层展开 `\t` 而改写内容。
    line: String,
    /// 当前行用于视觉渲染的正文。
    ///
    /// 业务意图：
    /// - `\t` 会在显示层按固定 tab stop 展开为空格，让日志列间距与常见编辑器一致。
    /// - 该字段只用于 `StyledText`，不参与复制或保存。
    display_line: String,
    /// 当前行语法高亮与选区高亮范围，范围基于 `display_line` 的 UTF-8 字节边界。
    highlights: Vec<(Range<usize>, gpui::HighlightStyle)>,
    /// 行号列宽度。
    line_number_width: f32,
    /// 当前日志显示字号。
    ///
    /// 业务意图：
    /// - 日志正文渲染由静态辅助函数完成，必须显式传入当前设置字号，避免继续读取旧的固定常量。
    font_size: f32,
    /// 横向滚动时行号列的反向补偿偏移。
    horizontal_line_number_offset: Pixels,
    /// 横向滚动时正文内容的局部偏移。
    ///
    /// 业务意图：
    /// - 普通 `uniform_list` 路径由 GPUI 整行平移，正文不需要额外偏移。
    /// - 分页窗口化路径只在视口内绝对定位行，必须单独平移正文，同时保持行号列固定。
    horizontal_content_offset: Pixels,
    /// 当前行是否是搜索结果跳转后的目标行。
    search_highlighted: bool,
    /// 当前行是否被用户手动标记。
    ///
    /// 业务意图：
    /// - 标记只显示在行号 gutter 内，避免改变正文背景后和搜索跳转高亮、选区高亮互相干扰。
    marked: bool,
    /// 是否临时禁用行 hover 样式。
    ///
    /// 业务意图：
    /// - 调整搜索结果面板高度或拖动日志正文滚动条时，鼠标可能经过底层日志行；即使不应触发选区，GPUI hover 仍可能命中行。
    /// - 在渲染数据中显式携带禁用标记，可以让日志行完全不注册 hover 样式，避免拖动控件时底层正文闪动。
    suppress_hover: bool,
    /// 当前主题调色板。
    palette: AppThemePalette,
}

/// 日志行显示层展开结果。
///
/// 业务意图：
/// - 日志原文里的 `\t` 需要按固定 4 列 tab stop 展示为空格，但选区、搜索高亮和鼠标命中仍要回到原始文本。
/// - 该结构同时保存原始字节下标和显示字节下标的双向映射，避免中文、多字节字符和 tab 混合时出现高亮错位。
///
/// 边界条件：
/// - 映射数组长度分别为原始文本和显示文本的 `len + 1`，保证行尾位置也能安全换算。
/// - 对于 UTF-8 多字节字符的内部字节，映射会回退到字符起点；正常业务范围都应落在字符边界。
#[derive(Debug, PartialEq, Eq)]
struct ExpandedLogLine {
    /// 展开 tab 后用于渲染的文本。
    text: String,
    /// 原始文本字节下标到显示文本字节下标的映射。
    original_to_display_bytes: Vec<usize>,
    /// 显示文本字节下标到原始文本字节下标的映射。
    display_to_original_bytes: Vec<usize>,
}

/// 日志正文自绘滚动条的方向。
///
/// 业务意图：
/// - 右侧日志查看器需要同时支持纵向和横向滚动条拖动，两者共享拖动生命周期，但坐标轴和偏移字段不同。
/// - 使用枚举明确区分方向，避免在拖动计算中通过布尔值表达业务含义。
#[derive(Clone, Copy)]
enum LogScrollbarAxis {
    /// 控制日志正文上下滚动。
    Vertical,
    /// 控制日志正文左右滚动。
    Horizontal,
}

/// 日志正文滚动条正在被拖动时的临时状态。
///
/// 业务意图：
/// - 鼠标按下滑块后，后续移动事件需要知道拖动的是哪个 tab、哪个方向，以及鼠标按下点在滑块内的偏移。
/// - 保存滑块内偏移可以避免拖动开始瞬间滑块跳动，符合原生滚动条的交互预期。
///
/// 边界条件：
/// - 该状态只存在于鼠标左键拖动期间；关闭 tab、释放鼠标或来源重新加载时都会被清理。
#[derive(Clone, Copy)]
struct LogScrollbarDrag {
    /// 正在拖动滚动条的 tab ID。
    tab_id: usize,
    /// 当前拖动的滚动条方向。
    axis: LogScrollbarAxis,
    /// 鼠标按下点相对滑块起点的偏移。
    cursor_offset: Pixels,
}

/// 日志正文自绘滚动条的布局测量结果。
///
/// 业务意图：
/// - 滑块渲染、按下命中和拖动换算都必须使用同一套轨道与滑块尺寸，避免视觉位置和实际滚动位置不一致。
/// - 坐标均为相对日志列表视口左上角的局部坐标，调用方再根据方向映射到窗口坐标。
#[derive(Clone, Copy)]
struct LogScrollbarMetrics {
    /// 滑块起点在当前轴向上的局部坐标。
    thumb_start: Pixels,
    /// 滑块在当前轴向上的长度。
    thumb_length: Pixels,
    /// 轨道起点在当前轴向上的局部坐标。
    track_start: Pixels,
    /// 轨道在当前轴向上的总长度。
    track_length: Pixels,
    /// 当前轴向可滚动的最大距离。
    max_scroll: Pixels,
    /// 当前轴向可滚动的最大距离，使用 `f64` 保留超大分页日志的逻辑滚动精度。
    ///
    /// 业务意图：
    /// - 滑块自身仍以 `Pixels` 渲染，但分页日志的真实内容高度可能达到数亿像素。
    /// - 拖动换算回逻辑滚动位置时使用该字段，避免再次把深处行号压回 `f32` 精度。
    max_scroll_px: f64,
}

/// 左侧目录树滚动条正在被拖动时的临时状态。
///
/// 业务意图：
/// - 左侧树和右侧日志正文都使用虚拟列表，但左侧树没有 tab 维度，因此用独立状态记录滑块内鼠标偏移。
/// - 保存偏移可以避免拖动开始时滑块跳到鼠标中心，交互行为更接近系统滚动条。
///
/// 边界条件：
/// - 该状态只在鼠标左键拖动期间有效；重新加载日志、鼠标释放或列表测量失效时都会清空。
#[derive(Clone, Copy)]
struct LogTreeScrollbarDrag {
    /// 鼠标按下点相对滑块顶部的偏移。
    cursor_offset: Pixels,
}

/// 搜索结果面板滚动条正在被拖动时的临时状态。
///
/// 业务意图：
/// - 搜索结果面板使用虚拟列表承载历史记录和命中明细，滚动条拖动需要保存鼠标按下点在滑块内的偏移。
/// - 只保存偏移而不保存列表内容，避免拖动过程和搜索结果数据生命周期耦合。
///
/// 边界条件：
/// - 该状态只在鼠标左键拖动期间有效；关闭结果面板、释放鼠标或列表测量失效时都会清空。
#[derive(Clone, Copy)]
struct SearchResultsScrollbarDrag {
    /// 鼠标按下点相对滑块顶部的偏移。
    cursor_offset: Pixels,
}

/// 搜索对话框中的文本输入槽位。
///
/// 业务意图：
/// - 搜索对话框现在包含“关键字”和“目录目标”两个可编辑文本框，二者都需要走 GPUI 平台输入协议以支持中文 IME。
/// - 用枚举标识当前编辑槽位，可以复用同一套 UTF-16/UTF-8 范围转换和组合文本替换逻辑。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchTextInputKind {
    /// 搜索关键字输入框。
    Query,
    /// 当前目录搜索的目标目录输入框。
    DirectoryTarget,
}

/// 设置页模型配置表单中的单行输入槽位。
///
/// 业务意图：
/// - “模型”页包含配置名称、Base URL、API Key 和模型 ID 四个自绘输入框，平台 IME 和鼠标命中需要知道当前焦点属于哪个字段。
/// - 使用枚举集中区分字段，可以复用同一套复制、粘贴、全选、删除和组合文本处理逻辑。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelConfigInputKind {
    /// 用户可读的配置名称。
    Name,
    /// OpenAI 兼容 API 根路径。
    BaseUrl,
    /// OpenAI 兼容 API Key，可为空以兼容本地 Ollama/vLLM。
    ApiKey,
    /// Chat Completions 请求中的模型 ID。
    Model,
}

/// 单行文本输入框的通用编辑状态。
///
/// 业务意图：
/// - 搜索关键字、目录目标、快搜关键字和模型配置字段都是单行自绘输入框，核心状态都由 UTF-8 文本、
///   UTF-8 选择范围和 IME 组合范围组成。
/// - 抽出该状态后，后续按键处理、UTF-16/UTF-8 转换和组合文本替换可以逐步收敛到同一套工具函数。
///
/// 边界条件：
/// - 该结构不包含焦点句柄、字形布局或鼠标拖拽锚点；这些数据受窗口、渲染元素和具体交互区域约束，
///   第一轮仍保留在原功能域内，避免搜索窗口和设置窗口的焦点生命周期发生变化。
#[derive(Clone)]
struct SingleLineTextInputState {
    /// 当前字段文本，使用 UTF-8 保存，平台输入协议回调时再和 UTF-16 范围互转。
    text: String,
    /// 当前选择范围，按 UTF-8 字节下标保存，必须始终夹到字符边界。
    selection_range: Range<usize>,
    /// 中文等输入法正在组合的文本范围，提交或取消组合时清空。
    marked_range: Option<Range<usize>>,
}

impl SingleLineTextInputState {
    /// 创建空单行输入状态。
    fn empty() -> Self {
        Self::from_text(String::new())
    }

    /// 使用指定文本创建单行输入状态并把光标放到末尾。
    fn from_text(text: String) -> Self {
        let cursor = text.len();
        Self {
            text,
            selection_range: cursor..cursor,
            marked_range: None,
        }
    }

    /// 用新文本替换输入框内容并把光标放到末尾。
    ///
    /// 业务意图：
    /// - 切换搜索预填文本、模型配置或设置草稿时，输入框必须一次性切换到目标文本，不能保留旧选区或 IME 组合状态。
    fn set_text(&mut self, text: String) {
        let cursor = text.len();
        self.text = text;
        self.selection_range = cursor..cursor;
        self.marked_range = None;
    }
}

/// 将平台 UTF-16 范围转换为单行输入内部可安全切片的 UTF-8 字节范围。
///
/// 业务意图：
/// - GPUI 平台输入协议按 UTF-16 计数，Rust `String` 必须按 UTF-8 字节边界切片。
/// - 搜索框、目录输入框、快搜关键字和模型配置字段共享该工具，保证中文、emoji 和其它非 ASCII 输入行为一致。
fn single_line_range_from_utf16(text: &str, range_utf16: Range<usize>) -> Range<usize> {
    let start = single_line_byte_index_from_utf16(text, range_utf16.start);
    let end = single_line_byte_index_from_utf16(text, range_utf16.end);
    start.min(end)..end.max(start)
}

/// 将单行输入内部 UTF-8 字节范围转换为平台输入协议需要的 UTF-16 范围。
fn single_line_range_to_utf16(text: &str, range: Range<usize>) -> Range<usize> {
    let start = single_line_utf16_offset_from_byte(text, range.start);
    let end = single_line_utf16_offset_from_byte(text, range.end);
    start..end
}

/// 把 UTF-16 偏移映射到 UTF-8 字节边界。
///
/// 边界条件：
/// - 如果平台给出超过文本长度的偏移，统一夹到字符串末尾。
/// - 如果偏移落在代理对或多字节字符内部，返回该字符起点，保证后续 `replace_range` 安全。
fn single_line_byte_index_from_utf16(text: &str, target_utf16: usize) -> usize {
    let mut utf16_cursor = 0usize;
    for (byte_index, character) in text.char_indices() {
        if utf16_cursor >= target_utf16 {
            return byte_index;
        }
        utf16_cursor += character.len_utf16();
    }
    text.len()
}

/// 把 UTF-8 字节边界映射到 UTF-16 偏移。
fn single_line_utf16_offset_from_byte(text: &str, byte_index: usize) -> usize {
    text.char_indices()
        .take_while(|(index, _)| *index < byte_index)
        .map(|(_, character)| character.len_utf16())
        .sum()
}

/// 模型配置单行输入框的窗口相关状态。
///
/// 业务意图：
/// - GPUI 当前版本没有现成文本输入控件，设置页每个模型字段都需要保存文本、选区、IME 组合态和鼠标命中布局。
/// - 该状态只服务当前设置窗口会话；保存时才会转换成 `ModelProfile` 并写入配置文件。
struct ModelConfigTextFieldState {
    /// 单行输入框的通用编辑状态。
    input: SingleLineTextInputState,
    /// 当前字段焦点句柄，用于 GPUI 平台输入路由和光标绘制判断。
    focus: gpui::FocusHandle,
    /// 最近一次绘制的单行字形布局，用于鼠标点击和拖拽反推出字符位置。
    last_layout: Option<ShapedLine>,
    /// 最近一次绘制的输入框窗口坐标边界，用于 IME 候选窗口定位。
    last_bounds: Option<Bounds<Pixels>>,
    /// 鼠标拖拽选择时的固定锚点，释放鼠标后清空。
    selection_drag: Option<usize>,
}

impl ModelConfigTextFieldState {
    /// 创建一个空模型配置输入状态。
    fn new(context: &mut Context<MainView>) -> Self {
        Self {
            input: SingleLineTextInputState::empty(),
            focus: context.focus_handle(),
            last_layout: None,
            last_bounds: None,
            selection_drag: None,
        }
    }

    /// 用新文本替换输入框内容并把光标放到末尾。
    ///
    /// 业务意图：
    /// - 切换模型配置或点击新增时，表单字段必须一次性切换到目标配置，不能保留旧选区或 IME 组合状态。
    fn set_text(&mut self, text: String) {
        self.input.set_text(text);
        self.selection_drag = None;
    }

    /// 清空排版缓存。
    ///
    /// 边界条件：
    /// - 字段内容、掩码显示或窗口尺寸变化后，旧布局不再代表当前可见文本；清空后鼠标命中会安全回退到文本末尾。
    fn clear_layout(&mut self) {
        self.last_layout = None;
        self.last_bounds = None;
    }
}

impl Deref for ModelConfigTextFieldState {
    type Target = SingleLineTextInputState;

    /// 让模型配置字段继续像普通单行输入状态一样访问文本、选区和组合范围。
    fn deref(&self) -> &Self::Target {
        &self.input
    }
}

impl DerefMut for ModelConfigTextFieldState {
    /// 让既有 IME 和按键处理逻辑可以在第一轮重构中继续就地修改通用输入状态。
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.input
    }
}

/// OpenAI 兼容模型配置文件的单条档案。
///
/// 业务意图：
/// - 每条档案保存一个可命名的 API 端点和模型 ID，用户可以在不同 OpenAI 兼容服务之间切换。
/// - 字段保持简单字符串，便于 JSON 配置手工排查和后续迁移。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ModelProfile {
    /// 稳定 ID，用于列表选择和默认模型引用；不直接展示给用户。
    id: String,
    /// 用户可读配置名称。
    name: String,
    /// OpenAI 兼容 API 根路径，例如 `https://api.openai.com/v1`。
    base_url: String,
    /// API Key，允许为空以兼容不需要鉴权的本地服务。
    api_key: String,
    /// Chat Completions 请求体中的模型 ID。
    model: String,
}

/// 模型配置文件的完整结构。
///
/// 业务意图：
/// - `profiles` 保存多条配置，`default_profile_id` 保存当前默认配置的稳定 ID。
/// - 删除或读取损坏配置时会通过规范化逻辑清理悬空默认 ID，避免 UI 指向不存在的配置。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ModelConfigs {
    /// 已保存的模型配置列表。
    profiles: Vec<ModelProfile>,
    /// 当前默认配置 ID；没有默认或默认配置被删除时为 `None`。
    default_profile_id: Option<String>,
}

/// 模型测试按钮的 UI 状态。
///
/// 业务意图：
/// - 测试请求在后台执行，状态需要区分空闲、进行中、成功和失败，避免旧请求返回后覆盖新请求结果。
#[derive(Clone, Debug, PartialEq, Eq)]
enum ModelTestStatus {
    /// 尚未测试或结果被用户编辑操作清空。
    Idle,
    /// 当前正在执行的测试任务 ID。
    Testing { job_id: usize },
    /// 最近一次测试成功。
    Success(String),
    /// 最近一次测试失败，字符串为用户可见中文原因。
    Failed(String),
}

impl ModelTestStatus {
    /// 返回用户可见状态文案。
    fn message(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::Testing { .. } => Some("测试中..."),
            Self::Success(message) | Self::Failed(message) => Some(message.as_str()),
        }
    }

    /// 返回是否处于正在测试状态。
    fn is_testing(&self) -> bool {
        matches!(self, Self::Testing { .. })
    }
}

/// 线程日志分析过滤输入区中的单行排版缓存。
///
/// 业务意图：
/// - 多行输入区需要根据用户点击的窗口坐标反推出 UTF-8 字节下标；保存每行真实字形布局可以复用 GPUI 文本系统的命中算法。
/// - `byte_range` 不包含行尾换行符，光标落在换行符前后时分别映射到上一行末尾或下一行开头。
struct ThreadAnalysisFilterLineLayout {
    /// 当前可视行对应的原始文本 UTF-8 字节范围。
    byte_range: Range<usize>,
    /// 当前行的 GPUI 字形布局。
    line: ShapedLine,
    /// 当前行在窗口中的绘制边界。
    bounds: Bounds<Pixels>,
}

/// 主窗口当前展示的大功能页。
///
/// 业务意图：
/// - 主窗口左侧固定大导航只负责在日志分析、HPROF 解析和 AI 对话三个主要工作区之间切换。
/// - 状态只保存在当前会话，不写入配置文件，避免后续调整默认入口或恢复策略时被旧配置约束。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MainFeature {
    /// 日志分析页，承载日志加载、目录树、日志 tab、搜索和线程日志分析入口。
    LogAnalysis,
    /// HPROF 解析页，承载 heap dump 文件选择、解析进度和 dominator tree 结果。
    HprofAnalysis,
    /// AI 对话页，承载本地会话历史、模型选择和 OpenAI 兼容流式对话。
    AiChat,
}

impl Default for MainFeature {
    /// 默认进入日志分析页。
    fn default() -> Self {
        Self::LogAnalysis
    }
}

impl MainFeature {
    /// 返回主功能在左侧大导航中的固定展示顺序。
    fn all() -> &'static [Self] {
        &[Self::LogAnalysis, Self::HprofAnalysis, Self::AiChat]
    }

    /// 返回主功能中文名称。
    fn label(self) -> &'static str {
        match self {
            Self::LogAnalysis => "日志分析",
            Self::HprofAnalysis => "HPROF解析",
            Self::AiChat => "AI对话",
        }
    }

    /// 返回主功能导航图标。
    fn icon(self) -> Icon {
        match self {
            Self::LogAnalysis => Icon::Search,
            Self::HprofAnalysis => Icon::ChartNoAxesCombined,
            Self::AiChat => Icon::BotMessageSquare,
        }
    }
}

/// 左侧大导航中的可悬浮入口。
///
/// 业务意图：
/// - 导航栏只显示图标，hover 气泡需要知道当前入口名称和纵向位置；设置属于通用入口，不计入主功能状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MainNavigationItem {
    /// 三个主功能页入口。
    Feature(MainFeature),
    /// 设置独立窗口入口。
    Settings,
}

impl MainNavigationItem {
    /// 返回导航入口中文名称。
    fn label(self) -> &'static str {
        match self {
            Self::Feature(feature) => feature.label(),
            Self::Settings => "设置",
        }
    }

    /// 返回导航入口图标。
    fn icon(self) -> Icon {
        match self {
            Self::Feature(feature) => feature.icon(),
            Self::Settings => Icon::Settings,
        }
    }

    /// 返回 hover 气泡在主窗口根节点中的垂直锚点。
    ///
    /// 业务意图：
    /// - 气泡必须绘制在右侧功能页之上，不能作为导航按钮子元素被后续兄弟节点盖住；因此这里用纯函数复刻导航按钮的垂直位置。
    /// - 顶部三个主功能从上向下定位，底部两个通用入口从下向上定位，避免依赖运行时窗口高度。
    fn tooltip_anchor(self) -> MainNavigationTooltipAnchor {
        match self {
            Self::Feature(MainFeature::LogAnalysis) => {
                MainNavigationTooltipAnchor::Top(MAIN_NAV_PADDING + MAIN_NAV_TOOLTIP_BUTTON_INSET)
            }
            Self::Feature(MainFeature::HprofAnalysis) => MainNavigationTooltipAnchor::Top(
                MAIN_NAV_PADDING
                    + MAIN_NAV_BUTTON_SIZE
                    + MAIN_NAV_BUTTON_GAP
                    + MAIN_NAV_TOOLTIP_BUTTON_INSET,
            ),
            Self::Feature(MainFeature::AiChat) => MainNavigationTooltipAnchor::Top(
                MAIN_NAV_PADDING
                    + (MAIN_NAV_BUTTON_SIZE + MAIN_NAV_BUTTON_GAP) * 2.0
                    + MAIN_NAV_TOOLTIP_BUTTON_INSET,
            ),
            Self::Settings => MainNavigationTooltipAnchor::Bottom(
                MAIN_NAV_PADDING + MAIN_NAV_TOOLTIP_BUTTON_INSET,
            ),
        }
    }
}

/// 左侧大导航 hover 气泡的垂直锚点。
///
/// 业务意图：
/// - 主功能入口贴近导航顶部，通用入口贴近导航底部；气泡提到根节点覆盖绘制后，必须保留这两类入口的原始视觉位置。
#[derive(Clone, Copy, Debug, PartialEq)]
enum MainNavigationTooltipAnchor {
    /// 从主窗口顶部向下定位。
    Top(f32),
    /// 从主窗口底部向上定位。
    Bottom(f32),
}

/// 设置窗口当前激活的页签。
///
/// 业务意图：
/// - 设置窗口按用户要求拆成“通用 / 日志 / 模型 / 关于”页签；状态放在主视图中，避免关闭窗口后当前会话选择丢失。
/// - 当前页签状态只存在于进程内，不写入配置文件；后续若需要记忆页签，应先定义设置持久化策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SettingsTab {
    /// 通用设置页签，当前承载主题和日志显示字号设置。
    General,
    /// 日志设置页签，当前承载线程日志分析过滤配置。
    Log,
    /// 模型设置页签，当前承载 OpenAI 兼容模型配置管理。
    Model,
    /// 关于页签，承载软件版本、作者和特色功能说明。
    About,
}

impl SettingsTab {
    /// 返回设置页签固定展示顺序。
    ///
    /// 业务意图：
    /// - 页签顺序集中定义，避免渲染和测试出现顺序分歧；关于收入口设置后放在最后，保留配置类页签优先级。
    fn all() -> &'static [Self] {
        &[Self::General, Self::Log, Self::Model, Self::About]
    }

    /// 返回页签中文标签。
    fn label(self) -> &'static str {
        match self {
            Self::General => "通用",
            Self::Log => "日志",
            Self::Model => "模型",
            Self::About => "关于",
        }
    }

    /// 返回页签图标。
    ///
    /// 业务意图：
    /// - 独立设置窗口的页签入口使用图标加文本，帮助用户快速区分通用配置和模型配置。
    fn icon(self) -> Icon {
        match self {
            Self::General => Icon::Settings,
            Self::Log => Icon::FileText,
            Self::Model => Icon::MonitorCog,
            Self::About => Icon::Info,
        }
    }
}

/// 搜索对话框级键盘命令。
///
/// 业务意图：
/// - `Enter` 和 `Escape` 在搜索窗口里分别代表执行搜索和关闭窗口，但设置窗口也有自绘文本框；
///   先把这两个控制键归类，便于全局快捷键在焦点位于设置输入框时明确放行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SearchDialogControlKey {
    /// 执行当前搜索条件。
    Submit,
    /// 关闭搜索对话框。
    Close,
}

/// 键盘滚动快捷键的目标区域。
///
/// 业务意图：
/// - 日志正文、搜索结果和左侧目录树都有独立滚动上下文，键盘滚动必须知道用户最近关注的是哪一块。
/// - 该枚举只记录主窗口内的只读滚动区域；搜索框、设置文本框等可编辑控件聚焦时会直接放行按键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyboardScrollRegion {
    /// 右侧当前日志正文区域。
    LogContent,
    /// 右侧底部搜索结果面板。
    SearchResults,
    /// 左侧日志目录树。
    LogTree,
}

/// 键盘滚动命令。
///
/// 业务意图：
/// - 把平台按键字符串转换为有限命令后，滚动计算和 UI 事件处理都可以复用同一套逻辑。
/// - 本次只支持垂直滚动，不改变横向滚动、日志选区或目录树展开规则。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyboardScrollCommand {
    /// 向上滚动一页。
    PageUp,
    /// 向下滚动一页。
    PageDown,
    /// 滚动到顶部，对应 `Ctrl+Home`。
    Top,
    /// 滚动到底部，对应 `Ctrl+End`。
    Bottom,
}

/// 当前会话内的搜索关键字历史项。
///
/// 业务意图：
/// - 普通搜索历史需要同时恢复查询词和匹配模式，避免用户从历史选择正则表达式后仍按普通文本执行。
/// - 历史只保存在内存中，不写入磁盘，降低日志关键字或敏感正则被持久化的风险。
#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchQueryHistoryItem {
    /// 用户输入的查询词，已经去掉首尾空白。
    query: String,
    /// 查询词对应的匹配模式。
    match_mode: SearchMatchMode,
}

impl SearchQueryHistoryItem {
    /// 构造历史项；空白查询词不会生成历史。
    fn new(query: &str, match_mode: SearchMatchMode) -> Option<Self> {
        let query = query.trim();
        (!query.is_empty()).then(|| Self {
            query: query.to_string(),
            match_mode,
        })
    }
}

/// 搜索对话框的交互状态。
///
/// 业务意图：
/// - `Ctrl+F` 打开后，对话框需要保存查询词、搜索范围、大小写策略和实时进度。
/// - 该状态只存在于当前会话，不写入配置文件，避免在搜索历史和隐私规则未定义前持久化用户输入。
///
/// 边界条件：
/// - 关闭对话框会取消当前搜索任务，但不会主动清空结果面板，方便用户继续查看上一次结果。
#[derive(Clone)]
struct SearchDialogState {
    /// 查询词输入框状态。
    ///
    /// 业务意图：
    /// - 普通搜索支持单行普通文本或正则表达式，输入中的换行会被忽略。
    /// - GPUI 输入协议对外使用 UTF-16 下标，通用单行状态负责保存内部 UTF-8 范围。
    query_input: SingleLineTextInputState,
    /// 搜索关键字历史下拉菜单是否展开。
    ///
    /// 业务意图：
    /// - 历史记录本身保存在 `MainView`，展开状态属于当前搜索窗口交互，随搜索窗口关闭一起丢弃。
    ///
    /// 边界条件：
    /// - 当历史为空、用户选择历史项、编辑关键字或开始搜索时都应关闭，避免显示过期选项。
    query_history_menu_open: bool,
    /// 搜索范围。
    scope: SearchScope,
    /// 当前目录搜索的目标目录输入框状态。
    ///
    /// 业务意图：
    /// - 选择“当前目录”时，需要把实际搜索目标展示给用户，允许用户缩小到子目录或修改为加载树中的其它目录片段。
    /// - 该字段只用于过滤已经加载树中收集到的可搜索来源，不会扩大文件系统访问范围。
    directory_input: SingleLineTextInputState,
    /// 是否区分大小写。
    case_sensitive: bool,
    /// 当前普通搜索的匹配模式。
    ///
    /// 业务意图：
    /// - 普通搜索可以切换为正则；快搜不读取该字段，始终按普通文本 OR 执行。
    /// - 正则模式下 `case_sensitive` 仍保留但 UI 置灰，便于用户切回普通文本时恢复之前选择。
    match_mode: SearchMatchMode,
    /// 当前关键字在当前激活文件中的出现次数。
    ///
    /// 业务意图：
    /// - 搜索窗口需要即时反馈关键字在当前文件中的命中次数，帮助用户决定是否继续执行完整搜索。
    /// - 该字段是缓存值，避免光标闪烁导致窗口频繁重绘时反复扫描大日志。
    ///
    /// 边界条件：
    /// - `None` 表示没有关键字、没有当前文件、当前文件仍在加载或打开失败；UI 应展示占位而不是误报 0。
    current_file_match_count: Option<usize>,
    /// 当前是否有后台搜索任务仍在运行。
    is_searching: bool,
    /// 当前搜索任务的进度快照。
    progress: SearchProgress,
    /// 搜索状态提示。
    message: String,
    /// 对话框绑定的任务 ID。
    ///
    /// 业务意图：
    /// - 用户快速连续搜索时，旧后台任务可能晚于新任务返回；任务 ID 用于丢弃过期更新。
    job_id: usize,
}

/// 单次搜索历史记录。
///
/// 业务意图：
/// - 用户可能连续尝试多个关键字或范围，底部面板需要保留每一次搜索作为可展开记录，避免新搜索覆盖旧上下文。
/// - 每条记录独立保存进度、命中和错误，后台任务回调只更新对应 `job_id` 的记录。
#[derive(Clone)]
struct SearchHistoryRecord {
    /// 后台任务 ID。
    job_id: usize,
    /// 本次搜索使用的查询词。
    query: String,
    /// 本次搜索范围。
    scope: SearchScope,
    /// 当前目录搜索的目标目录；当前文件搜索为 `None`。
    directory_target: Option<String>,
    /// 本次搜索是否区分大小写。
    case_sensitive: bool,
    /// 本次搜索的匹配模式。
    ///
    /// 业务意图：
    /// - 结果面板摘要必须准确说明这条记录按普通文本还是正则执行，避免历史记录混淆。
    match_mode: SearchMatchMode,
    /// 搜索进度终态或当前进度。
    progress: SearchProgress,
    /// 搜索命中结果。
    results: Vec<SearchResultItem>,
    /// 单文件读取或解码失败列表。
    errors: Vec<SearchFileError>,
    /// 当前记录是否来自被用户取消的搜索任务。
    canceled: bool,
    /// 记录是否展开显示明细。
    expanded: bool,
    /// 当前记录中已经展开的文件分组来源键。
    ///
    /// 业务意图：
    /// - 历史记录只负责“这次搜索是否展开”，文件分组负责“某个文件内结果是否展开”。
    /// - 使用来源稳定键而不是文件名，避免同名文件或压缩包内同名成员互相影响展开状态。
    expanded_file_keys: HashSet<String>,
}

impl SearchHistoryRecord {
    /// 返回当前记录的状态文案。
    ///
    /// 边界条件：
    /// - 取消状态优先于进度判断，否则关闭对话框后旧任务回调被丢弃，记录会长期误显示为“搜索中”。
    fn state_label(&self) -> &'static str {
        if self.canceled {
            "已取消"
        } else if self.progress.searched_files < self.progress.total_files {
            "搜索中"
        } else {
            "完成"
        }
    }
}

/// 搜索结果面板虚拟列表行。
///
/// 业务意图：
/// - 面板需要同时展示搜索历史记录头、命中明细、错误明细和空态说明。
/// - 统一展平成固定高度行后仍可继续使用 `uniform_list`，避免大量结果时重新引入滚动卡顿。
#[derive(Clone)]
enum SearchResultsPanelRow {
    /// 一条搜索历史记录的摘要行。
    RecordHeader { record_index: usize },
    /// 一条文件分组摘要行。
    FileHeader {
        /// 所属历史记录下标。
        record_index: usize,
        /// 文件来源稳定键。
        source_key: String,
        /// 文件完整展示路径。
        ///
        /// 业务意图：
        /// - 文件分组行渲染时直接使用缓存的展示文本，避免滚动可见行时重新扫描该搜索记录的全部命中。
        full_path: String,
        /// 当前文件内命中数量。
        ///
        /// 业务意图：
        /// - 该数量在展平行时已经可以确定，缓存到行模型中可以让虚拟列表渲染保持 O(可见行数)。
        result_count: usize,
    },
    /// 一条搜索命中明细行。
    Result {
        /// 所属历史记录下标。
        record_index: usize,
        /// 命中结果下标。
        result_index: usize,
    },
    /// 一条文件级错误明细行。
    Error {
        /// 所属历史记录下标。
        record_index: usize,
        /// 错误下标。
        error_index: usize,
    },
    /// 展开记录但暂无命中或错误时显示的说明行。
    Empty { record_index: usize },
}

/// 搜索历史记录内的文件分组。
///
/// 业务意图：
/// - 同一个文件可能有大量命中，结果面板需要先按文件归类，再让用户按需展开某个文件。
/// - 分组只保存命中结果在 `record.results` 中的下标，避免复制整条命中记录导致内存上涨。
#[derive(Clone)]
struct SearchResultFileGroup {
    /// 文件来源稳定键。
    source_key: String,
    /// 文件完整展示路径。
    ///
    /// 业务意图：
    /// - 结果面板按文件分组时，用户需要直接看到完整路径判断命中来源，避免文件名相同但目录不同造成误判。
    full_path: String,
    /// 当前文件内的命中结果下标。
    result_indices: Vec<usize>,
}

/// 搜索结果面板状态。
///
/// 业务意图：
/// - 面板从右侧内容区底部弹出，保存搜索历史记录和高度。
/// - 结果列表使用虚拟列表滚动句柄，避免大量命中时滚动卡顿。
#[derive(Clone)]
struct SearchResultsPanelState {
    /// 搜索历史记录，按发起时间从旧到新排列。
    records: Vec<SearchHistoryRecord>,
    /// 已展平的虚拟列表行缓存。
    ///
    /// 业务意图：
    /// - 搜索结果可能包含成千上万条命中，滚动时不能每一帧都重新展平全部历史记录和文件分组。
    /// - 该缓存只在记录增删、命中追加或展开状态改变时重建，渲染时按可见 range 直接取片段。
    rows: Vec<SearchResultsPanelRow>,
    /// 面板当前高度。
    height: f32,
    /// 结果虚拟列表滚动句柄。
    scroll_handle: UniformListScrollHandle,
}

/// 搜索结果面板右键菜单状态。
///
/// 业务意图：
/// - 结果面板可能包含多次搜索和大量文件分组，用户需要批量展开或收起以快速调整信息密度。
/// - GPUI 0.2.2 没有适合该场景的现成上下文菜单，因此保存右键位置并自绘菜单。
struct SearchResultsContextMenu {
    /// 菜单左上角相对右侧日志工作区的横坐标。
    x: f32,
    /// 菜单左上角相对右侧日志工作区的纵坐标。
    y: f32,
}

/// 搜索结果面板右键菜单命令。
#[derive(Clone, Copy)]
enum SearchResultsContextMenuAction {
    /// 展开所有搜索历史记录和文件分组。
    ExpandAll,
    /// 收起所有搜索历史记录和文件分组。
    CollapseAll,
}

/// 业务意图：
/// - `Ready` 直接保存当前日志文档，方便 UI 渲染和后台回调按同一状态枚举合并；暂不为了 lint 把文档额外装箱。
#[allow(clippy::large_enum_variant)]
enum LogTabState {
    /// 正在读取或重新解码。
    Loading {
        /// 展示给用户的中文状态。
        message: String,
    },
    /// 已成功解码。
    Ready {
        /// 可供右侧虚拟列表渲染的日志文档。
        ///
        /// 业务意图：
        /// - 小文件保存完整解码文档，超大文件保存分页文档；UI 渲染层通过统一枚举读取行数和可见文本。
        document: LogTabDocument,
    },
    /// 读取或解码失败。
    Failed {
        /// 中文错误说明。
        message: String,
    },
}

/// 日志 tab 读取完成后回到 UI 线程的数据。
///
/// 业务意图：
/// - 后台任务不能直接修改 GPUI 状态，必须把结果封装后在 `view.update` 中合并。
/// - 后台读取结果只在线程完成时短暂传回 UI，保持直接携带文档可以减少合并阶段的状态拆解。
#[allow(clippy::large_enum_variant)]
enum LogTabLoadResult {
    /// 读取和自动解码都成功。
    Ready {
        /// 原始字节，用于后续手动切换编码。
        raw_bytes: Option<Arc<Vec<u8>>>,
        /// 自动解码后的文档。
        document: LogTabDocument,
    },
    /// 原始字节读取成功，但自动检测或解码失败。
    DecodeFailed {
        /// 原始字节仍需保留，用户可以手动选择编码重新解析。
        raw_bytes: Arc<Vec<u8>>,
        /// 中文错误说明。
        message: String,
    },
    /// 原始字节读取失败。
    ReadFailed {
        /// 中文错误说明。
        message: String,
    },
}

/// 日志 tab 重新解码完成后的结果。
///
/// 业务意图：
/// - 手动切换编码只需要更新文档状态，不需要替换原始字节。
/// - 手动切换编码的结果同样只作为后台任务返回值，暂不引入装箱以免扩大解码合并路径改动。
#[allow(clippy::large_enum_variant)]
enum LogTabDecodeResult {
    /// 解码成功。
    Ready {
        /// 触发该后台任务时用户选择的编码策略。
        ///
        /// 业务意图：
        /// - 用户快速连续切换编码时，较早的后台任务可能晚返回；合并阶段需要用该字段识别并丢弃过期结果。
        encoding_choice: EncodingChoice,
        /// 新编码下的日志文档。
        document: LogTabDocument,
    },
    /// 解码失败。
    Failed {
        /// 触发该后台任务时用户选择的编码策略。
        ///
        /// 边界条件：
        /// - 如果该值已经不是 tab 当前选择，说明用户又切换了编码，旧错误不能覆盖新的加载状态。
        encoding_choice: EncodingChoice,
        /// 中文错误说明。
        message: String,
    },
}

/// tab 右键菜单状态。
///
/// 业务意图：
/// - GPUI 0.2.2 的 `show_window_menu` 是系统窗口菜单，不适合作为 tab 操作菜单。
/// - 因此这里保存右键位置并用 GPUI 元素自绘一个轻量菜单。
struct TabContextMenu {
    /// 菜单针对的 tab ID。
    tab_id: usize,
    /// 菜单左上角的窗口内容区横坐标。
    x: f32,
    /// 菜单左上角的窗口内容区纵坐标。
    y: f32,
}

/// 加载日志来源菜单状态。
///
/// 业务意图：
/// - Windows 不支持文件和目录混选时，工具栏按钮先展示自绘菜单，让用户明确进入文件/压缩包选择器或目录选择器。
/// - 菜单坐标使用窗口内容区坐标，作为主窗口根节点的绝对定位元素渲染，避免受左右分栏是否显示影响。
struct LoadSourceMenu {
    /// 菜单左上角的窗口内容区横坐标。
    x: f32,
    /// 菜单左上角的窗口内容区纵坐标。
    y: f32,
}

/// 编码选择下拉框状态。
///
/// 业务意图：
/// - 用户要求编码选择不要平铺在顶部，因此这里把当前打开的编码菜单作为独立弹层状态保存。
/// - 菜单只绑定一个 tab，切换 tab、关闭 tab 或重新加载日志时都会收起，避免对旧 tab 继续操作。
struct EncodingDropdownMenu {
    /// 下拉框所属的 tab ID。
    tab_id: usize,
    /// 菜单左上角相对右侧日志工作区的横坐标。
    ///
    /// 业务意图：
    /// - 编码选择器已经移动到右侧状态栏，按钮位置会随状态文本宽度变化，不能继续使用固定左边距。
    x: f32,
    /// 菜单左上角相对右侧日志工作区的纵坐标。
    y: f32,
}

/// tab 右键菜单命令。
///
/// 业务意图：
/// - 菜单项直接对应用户要求的三个关闭操作，避免 UI 文案和处理逻辑分散。
#[derive(Clone, Copy)]
enum TabContextMenuAction {
    /// 关闭被右键点击的 tab。
    Current,
    /// 关闭除被右键点击 tab 之外的所有 tab。
    OtherTabs,
    /// 关闭所有 tab。
    AllTabs,
}

/// 日志正文右键菜单状态。
///
/// 业务意图：
/// - 日志正文使用自绘虚拟列表，不能依赖系统文本控件菜单；这里保存菜单所需的 tab 和右侧面板局部坐标。
/// - 菜单只作用于当前右键所在 tab，避免用户切换 tab 后复制或保存到错误文件。
struct LogViewerContextMenu {
    /// 右键打开菜单时对应的日志 tab。
    tab_id: usize,
    /// 菜单左上角相对右侧日志工作区的横坐标。
    x: f32,
    /// 菜单左上角相对右侧日志工作区的纵坐标。
    y: f32,
}

/// 日志正文右键菜单命令。
///
/// 业务意图：
/// - 复制依赖当前正文选区；另存为依赖当前 tab 的原始日志来源，集中枚举可以保持渲染文案和执行逻辑一致。
#[derive(Clone, Copy)]
enum LogViewerContextMenuAction {
    /// 复制当前正文选区到剪贴板。
    Copy,
    /// 将当前 tab 的日志来源保存到用户选择的目录。
    SaveAs,
}

/// 左侧目录树单行渲染所需的输入数据。
///
/// 业务意图：
/// - 目录树行需要同时表达层级、图标、元信息和点击行为，字段较多；集中成结构体可以保持渲染函数签名稳定。
/// - 该结构只服务 UI 渲染，不回写加载层，也不作为文件来源持久化模型。
struct LogTreeRowRenderData {
    /// 当前加载结果内部的节点 ID，用于展开状态和元素稳定标识。
    node_id: usize,
    /// 当前节点在树中的深度，用于计算左侧缩进。
    depth: usize,
    /// 展开箭头图标；没有子节点时为 `None`，仍会保留占位宽度。
    expand_icon: Option<Icon>,
    /// 节点类型图标。
    item_icon: Icon,
    /// 节点类型图标颜色。
    icon_color: u32,
    /// 当前行是否可以展开或收起。
    can_toggle: bool,
    /// 当前行是否绑定日志正文来源；存在时点击打开右侧 tab。
    source: Option<LogFileSource>,
    /// 当前行在可见目录树中的下标。
    ///
    /// 业务意图：
    /// - Shift 多选需要按当前可见顺序选择连续行，折叠隐藏的节点不应被纳入本次范围。
    visible_index: usize,
    /// 当前行是否处于左侧树选择集合中。
    ///
    /// 业务意图：
    /// - 单击、多选和右键菜单都依赖可见选中态，渲染层需要用该字段决定背景和文字强调。
    selected: bool,
    /// 当前行展示文本。
    label: String,
    /// 当前行右侧短元信息。
    meta: Option<String>,
}

/// 左侧目录树普通点击触发的主动作。
///
/// 业务意图：
/// - 目录树需要同时支持“单击打开/展开”和 Shift、Ctrl/Command 多选；集中描述主动作可以避免点击处理里散落判断。
/// - 该动作不携带真实文件来源，文件来源仍由调用点从加载层传入，避免测试辅助类型复制路径模型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogTreePrimaryClickAction {
    /// 不执行打开或展开，只保留选择变化。
    None,
    /// 打开当前日志文件到右侧 tab。
    OpenSource,
    /// 展开或收起当前目录/压缩包节点。
    ToggleNode,
}

/// 左侧目录树右键菜单状态。
///
/// 业务意图：
/// - 右键菜单作用于左侧当前选择文件集合；如果用户右键未选中文件，先把右键行作为唯一选择。
/// - 菜单位置保存为左侧面板局部坐标，避免主窗口顶部工具栏或右侧区域偏移影响定位。
struct LogTreeContextMenu {
    /// 右键落点对应的节点 ID。
    node_id: usize,
    /// 右键落点对应的可读取文件来源。
    ///
    /// 业务意图：
    /// - 如果当前多选集合里没有可读取文件，右键菜单命令仍应能作用于右键点中的文件。
    /// - 单文件压缩包会在渲染阶段被映射成内部唯一文件来源，因此这里保存的是最终可打开/可分析来源。
    source: Option<LogFileSource>,
    /// 菜单左上角相对左侧目录树面板的横坐标。
    x: f32,
    /// 菜单左上角相对左侧目录树面板的纵坐标。
    y: f32,
}

/// 左侧目录树右键菜单命令。
///
/// 业务意图：
/// - 文件另存为和线程日志分析都基于当前多选文件集合，集中枚举可以让渲染和执行逻辑保持一致。
#[derive(Clone, Copy)]
enum LogTreeContextMenuAction {
    /// 把当前多选日志文件保存到用户指定目录。
    SaveAs,
    /// 对当前多选日志文件执行 Java thread dump 时间线分析。
    AnalyzeThreads,
}

/// 批量另存为的后台结果。
///
/// 业务意图：
/// - 文件复制发生在后台线程，结果回到 UI 后只需要知道成功和失败数量，用于开发期诊断或后续状态栏展示。
struct SaveSelectedLogsResult {
    /// 成功写入的文件数量。
    saved_count: usize,
    /// 因用户选择“跳过”而未覆盖的同名文件数量。
    skipped_count: usize,
    /// 失败的文件数量。
    failed_count: usize,
}

/// 另存为遇到同名目标文件时的处理策略。
///
/// 业务意图：
/// - 用户要求目标目录已有同名文件时必须弹窗确认，并提供“跳过”和“覆盖”两种明确选择。
/// - 策略作为纯数据传入后台保存函数，避免 UI 弹窗逻辑和文件复制逻辑耦合。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveConflictPolicy {
    /// 跳过已存在的目标文件。
    SkipExisting,
    /// 覆盖已存在的目标文件。
    OverwriteExisting,
}

/// 另存为同名文件确认弹窗状态。
///
/// 业务意图：
/// - 批量另存为可能一次命中多个同名文件；主窗口只弹一次确认，用户选择后对本次保存任务统一应用。
///
/// 边界条件：
/// - 这里只保存必要展示信息和待保存来源，不保存文件句柄；确认后仍按原管线重新物化和复制来源。
struct SaveOverwriteConfirmDialog {
    /// 待保存的日志来源。
    sources: Vec<LogFileSource>,
    /// 用户选择的目标目录。
    target_directory: PathBuf,
    /// 已存在目标路径数量。
    conflict_count: usize,
    /// 首个冲突目标路径，用于弹窗展示具体示例，避免用户无法判断风险。
    first_conflict_path: PathBuf,
}

/// 用户通过“加载日志”按钮选择的路径来源类型。
///
/// 业务意图：
/// - “加载日志”按钮现在直接打开系统选择器，减少点击后无反馈的歧义。
/// - 枚举保留路径选择配置入口，后续如需区分平台或补充 Windows 专用目录选择策略时不影响调用方。
/// - 变体保留 `Log` 前缀是为了让调用处语义直接对应用户菜单文案。
#[derive(Clone, Copy)]
#[allow(clippy::enum_variant_names)]
enum LoadPromptKind {
    /// 选择普通文件、目录或压缩包来源。
    LogSources,
    /// 选择普通文件或压缩包来源。
    LogFilesOrArchives,
    /// 选择目录来源。
    LogDirectories,
}

impl LoadPromptKind {
    /// 为 GPUI 系统路径选择器生成配置。
    ///
    /// 业务意图：
    /// - `files` 和 `directories` 同时开启，让 macOS 能像常见桌面工具一样在一次对话框里选择文件或目录。
    /// - Windows 和部分 Linux 后端不支持文件、目录混选，此时优先显示文件，保证 ZIP/RAR/7Z/TAR.GZ/GZ 等压缩包可直接选择。
    /// - `multiple` 保持为 `true`，允许用户一次加载多个文件、目录或压缩包来源。
    ///
    /// 边界条件：
    /// - GPUI 0.2.2 的 `PathPromptOptions` 不提供扩展名过滤字段，因此压缩包类型由加载层识别。
    /// - 不支持混选的平台如果仍传 `directories=true`，Windows 会进入只选目录模式，导致用户看不到压缩包文件。
    fn to_prompt_options(self, can_select_mixed_files_and_dirs: bool) -> PathPromptOptions {
        match self {
            Self::LogSources => PathPromptOptions {
                files: true,
                directories: can_select_mixed_files_and_dirs,
                multiple: true,
                prompt: Some("选择日志来源".into()),
            },
            Self::LogFilesOrArchives => PathPromptOptions {
                files: true,
                directories: false,
                multiple: true,
                prompt: Some("选择日志文件或压缩包".into()),
            },
            Self::LogDirectories => PathPromptOptions {
                files: false,
                directories: true,
                multiple: true,
                prompt: Some("选择日志目录".into()),
            },
        }
    }

    /// 返回进入加载状态时展示的中文说明。
    ///
    /// 边界条件：
    /// - 当前文案只用于左侧目录树区域，不作为错误判断依据。
    fn loading_message(self) -> &'static str {
        match self {
            Self::LogSources => "正在扫描已选择的日志来源...",
            Self::LogFilesOrArchives => "正在扫描已选择的日志文件或压缩包...",
            Self::LogDirectories => "正在扫描已选择的日志目录...",
        }
    }
}

/// 主窗口根视图。
///
/// 业务意图：
/// - 作为 GPUI 窗口的根实体，当前承载顶部工具栏、左右分栏和左侧日志目录树。
/// - 后续日志列表、状态栏、设置面板等 UI 应从这个根视图向下扩展，而不是直接写在
///   `main` 函数里，从而保持启动流程和渲染逻辑分离。
///
/// 边界条件：
/// - 当前视图只在用户点击“加载日志”并完成系统选择器确认后读取路径。
/// - 当前视图不保存持久状态，因此不会产生跨平台配置写入差异。
struct MainView {
    /// 主窗口句柄。
    ///
    /// 业务意图：
    /// - 线程分析、搜索等独立窗口可能触发主窗口定位行为；保存主窗口句柄后，这些辅助窗口可以在操作完成后
    ///   把主窗口重新激活到前台，让用户立刻看到跳转结果。
    ///
    /// 边界条件：
    /// - `MainView::new` 执行时主窗口句柄尚未由 GPUI 返回，因此初始化为 `None`，窗口创建完成后立即回填。
    main_window: Option<WindowHandle<MainView>>,

    /// 左侧内容区域当前宽度。
    ///
    /// 业务意图：
    /// - 保存用户在当前进程内拖动分割线后的结果，让左右栏布局能即时响应。
    /// - 初始值来自 `LEFT_PANEL_DEFAULT_WIDTH`，严格满足默认左侧 300px 的需求。
    ///
    /// 边界条件：
    /// - 当前宽度不写入磁盘；关闭应用后会恢复默认值。
    /// - 更新宽度时必须经过最小宽度约束，避免左右任一区域被拖到不可用。
    left_panel_width: f32,

    /// 当前是否处于拖动分割线状态。
    ///
    /// 业务意图：
    /// - 区分普通鼠标移动和调整分栏宽度，避免鼠标经过内容区域时误改布局。
    ///
    /// 边界条件：
    /// - 鼠标左键在分割线按下时变为 `true`，在内容区域收到左键释放时恢复为 `false`。
    /// - 当前不处理窗口失焦或鼠标释放发生在窗口外的情况，后续如需更强交互再补充捕获策略。
    is_resizing_splitter: bool,

    /// 主窗口左侧导航和全局滚动目标状态。
    ///
    /// 业务意图：
    /// - 导航状态已经从根视图拆入独立容器，避免日志、搜索和设置功能继续直接扩张 `MainView` 字段列表。
    /// - 该状态只保存当前会话内的临时交互信息，不写入配置文件。
    navigation: NavigationState,

    /// 日志加载、目录树、tab、编码菜单和日志正文相关状态。
    ///
    /// 业务意图：
    /// - 日志查看主流程的状态统一收口到该字段，后续重新加载日志或关闭 tab 时可以更清晰地区分工作区状态和全局偏好。
    /// - 本次迁移不改变任何日志来源、编码、搜索跳转或另存为行为。
    log: LogWorkspaceState,

    /// 搜索窗口、搜索历史和底部结果面板状态。
    ///
    /// 业务意图：
    /// - 搜索状态与日志 tab 状态仍需协作，但先拆成独立容器可以减少 `MainView` 的字段耦合。
    /// - 后台搜索任务 ID、独立窗口句柄和结果面板滚动状态继续保持当前会话内有效。
    search: SearchWorkspaceState,

    /// 设置窗口、主题、字号、快搜和线程过滤配置状态。
    ///
    /// 业务意图：
    /// - 设置相关字段集中后，配置读写迁移和后续设置窗口维护都不再需要穿过完整 `MainView` 字段列表。
    /// - 当前只迁移状态所有权，不改变配置文件路径、默认值或保存时机。
    settings: SettingsState,

    /// 模型配置列表、表单和接口测试状态。
    ///
    /// 业务意图：
    /// - 模型配置既服务设置页，也服务 AI 对话默认模型选择；单独成组可以让这两个功能共享同一份状态快照。
    /// - API Key 的明文存储策略保持原状，本次重构不扩大可见范围。
    model_config: ModelConfigState,

    /// 线程日志分析独立窗口句柄。
    ///
    /// 业务意图：
    /// - 线程分析窗口生命周期仍由主视图协调，避免本次状态拆分改变独立窗口复用和激活规则。
    /// - 句柄只服务当前会话；关闭窗口后由回调清空。
    thread_analysis_window: Option<WindowHandle<ThreadAnalysisWindowView>>,

    /// HPROF dump 分析内嵌视图实体。
    ///
    /// 业务意图：
    /// - HPROF 分析视图仍保持原有实体生命周期，后续单独拆分 HPROF 模块时再调整内部结构。
    /// - 实体只服务当前会话；应用关闭或实体释放时由视图自身取消后台解析任务。
    hprof_analysis_view: Option<Entity<HprofAnalysisView>>,

    /// AI 对话会话列表。
    ///
    /// 业务意图：
    /// - 左侧会话栏需要展示所有持久化会话摘要，按更新时间倒序排列；消息正文只在切换会话时加载。
    ai_chat_conversations: Vec<AiChatConversation>,

    /// 当前 AI 对话会话 ID。
    ///
    /// 边界条件：
    /// - 数据库不可用或会话全部删除时为空；右侧工作区显示空态并禁止发送。
    ai_chat_active_conversation_id: Option<String>,

    /// 当前 AI 对话消息列表。
    ///
    /// 业务意图：
    /// - 该字段只保存当前会话的消息，避免会话很多时主视图长期持有所有正文。
    ai_chat_messages: Vec<AiChatMessage>,

    /// AI 对话左侧历史会话虚拟列表状态。
    ///
    /// 性能约束：
    /// - 会话很多时只渲染可视范围；新增、删除或重排会话时必须同步该状态的条目数量。
    ai_chat_conversation_list_state: ListState,

    /// AI 对话右侧消息流虚拟列表状态。
    ///
    /// 性能约束：
    /// - 消息气泡高度会随内容和窗口宽度变化，使用 `ListState` 而不是等高列表，避免长回答滚动时卡顿或被裁切。
    ai_chat_message_list_state: ListState,

    /// AI 对话列表滚动条拖动状态。
    ///
    /// 边界条件：
    /// - 鼠标释放、切换会话或列表被重建时需要清空，避免旧拖动写入新的列表状态。
    ai_chat_scrollbar_drag: Option<AiChatScrollbarDrag>,

    /// AI 对话数据库错误。
    ///
    /// 错误处理：
    /// - SQLite 打开、迁移或写入失败不能影响日志查看主流程；错误保存在这里并展示到 AI 页面。
    ai_chat_database_error: Option<String>,

    /// AI 对话模型选择下拉菜单是否展开。
    ai_chat_model_menu_open: bool,

    /// AI 对话输入框文本。
    ///
    /// 业务意图：
    /// - 输入框支持多行提示词，按 UTF-8 保存，平台 IME 回调时再与 UTF-16 范围互转。
    ai_chat_input_text: String,

    /// AI 对话输入框选择范围。
    ai_chat_input_selection_range: Range<usize>,

    /// AI 对话输入框输入法组合文本范围。
    ai_chat_input_marked_range: Option<Range<usize>>,

    /// AI 对话输入框焦点句柄。
    ai_chat_input_focus: gpui::FocusHandle,

    /// AI 对话输入框最近一次绘制的逐行布局。
    ai_chat_input_last_layouts: Vec<AiChatInputLineLayout>,

    /// AI 对话输入框最近一次整体绘制边界。
    ai_chat_input_last_bounds: Option<Bounds<Pixels>>,

    /// AI 对话输入框拖拽选择锚点。
    ai_chat_input_selection_drag: Option<usize>,

    /// AI 对话输入区当前高度。
    ///
    /// 业务意图：
    /// - 用户可以拖拽输入区顶部调整高度；高度仅在当前运行会话中保留，避免临时编辑长提示词后永久改变默认布局。
    ///
    /// 边界条件：
    /// - 赋值必须经过 `clamp_ai_chat_input_height`，防止高度过小遮挡浮层控件或过大挤压消息列表。
    ai_chat_input_height: f32,

    /// AI 对话输入区高度拖拽状态。
    ///
    /// 边界条件：
    /// - 鼠标释放、离开窗口释放或切换到其它拖拽行为时需要清空，避免后续移动继续修改高度。
    ai_chat_input_resize_drag: Option<AiChatInputResizeDrag>,

    /// 当前正在进行的 AI 流式任务。
    ai_chat_streaming_task: Option<AiChatStreamingTask>,

    /// 下一个 AI 流式任务 ID。
    next_ai_chat_job_id: usize,

    /// 当前窗口系统外观。
    ///
    /// 业务意图：
    /// - 当主题偏好为“跟随系统”时，需要用 GPUI 提供的窗口外观计算实际调色板。
    /// - 该字段会随系统外观变化更新，驱动主窗口和独立窗口重绘。
    system_window_appearance: WindowAppearance,

    /// 主窗口外观变化订阅。
    ///
    /// 业务意图：
    /// - GPUI 的窗口外观监听返回订阅句柄，必须保存在主视图中，否则监听会立即失效。
    window_appearance_subscription: Option<gpui::Subscription>,

    /// 全局键盘监听订阅。
    ///
    /// 业务意图：
    /// - GPUI 的全局快捷键拦截器返回订阅句柄，必须跟随主视图保存，否则订阅被释放后 `Ctrl+F` 将不再生效。
    /// - 使用拦截器而不是事后观察器，是为了在 macOS key equivalent 和输入控件提前消费事件前捕获搜索快捷键。
    global_keystroke_subscription: Option<gpui::Subscription>,

    /// 主界面根节点焦点句柄。
    ///
    /// 业务意图：
    /// - 主窗口需要存在稳定焦点路径，搜索对话框关闭后可以把焦点恢复到根节点，后续全局快捷键和普通鼠标操作才能继续落到主界面。
    /// - 搜索对话框关闭后可把焦点还给根节点，避免焦点停在已经关闭的搜索窗口上导致后续快捷键无响应。
    root_focus_handle: gpui::FocusHandle,
}

impl MainView {
    /// 创建主窗口根视图。
    ///
    /// 业务意图：
    /// - 集中初始化所有首屏 UI 状态，避免在 `main` 的窗口创建回调中散落默认值。
    /// - 左侧栏默认 300px 是用户明确要求，必须从这里作为唯一入口初始化。
    fn new(context: &mut Context<Self>) -> Self {
        let model_configs = load_model_configs_preference();
        let default_ai_model_profile_id = ai_chat_default_model_profile_id(
            &model_configs.profiles,
            model_configs.default_profile_id.as_deref(),
        );
        let model_config = ModelConfigState::new(context, model_configs);
        let (
            ai_chat_conversations,
            ai_chat_active_conversation_id,
            ai_chat_messages,
            ai_chat_database_error,
        ) = if let Some(path) = ai_chat_database_path() {
            match load_ai_chat_conversations(&path) {
                Ok(mut conversations) => {
                    if conversations.is_empty() {
                        let conversation =
                            new_ai_chat_conversation(default_ai_model_profile_id.clone());
                        match insert_ai_chat_conversation(&path, &conversation) {
                            Ok(()) => {
                                let active_id = Some(conversation.id.clone());
                                conversations.push(conversation);
                                (conversations, active_id, Vec::new(), None)
                            }
                            Err(error) => (Vec::new(), None, Vec::new(), Some(error)),
                        }
                    } else {
                        let active_id = conversations
                            .first()
                            .map(|conversation| conversation.id.clone());
                        let messages = active_id
                            .as_deref()
                            .map(|conversation_id| load_ai_chat_messages(&path, conversation_id))
                            .transpose();
                        match messages {
                            Ok(messages) => {
                                (conversations, active_id, messages.unwrap_or_default(), None)
                            }
                            Err(error) => (conversations, active_id, Vec::new(), Some(error)),
                        }
                    }
                }
                Err(error) => (Vec::new(), None, Vec::new(), Some(error)),
            }
        } else {
            (
                Vec::new(),
                None,
                Vec::new(),
                Some("当前平台没有可用的应用配置目录，无法保存 AI 对话历史".to_string()),
            )
        };
        let ai_chat_conversation_list_state = ListState::new(
            ai_chat_conversations.len(),
            ListAlignment::Top,
            px(AI_CHAT_VIRTUAL_LIST_OVERDRAW),
        );
        let ai_chat_message_list_state = ListState::new(
            ai_chat_messages.len(),
            ListAlignment::Bottom,
            px(AI_CHAT_VIRTUAL_LIST_OVERDRAW),
        );

        Self {
            left_panel_width: LEFT_PANEL_DEFAULT_WIDTH,
            main_window: None,
            is_resizing_splitter: false,
            navigation: NavigationState::new(),
            log: LogWorkspaceState::new(),
            search: SearchWorkspaceState::new(context),
            settings: SettingsState::new(context),
            model_config,
            thread_analysis_window: None,
            hprof_analysis_view: None,
            ai_chat_conversations,
            ai_chat_active_conversation_id,
            ai_chat_messages,
            ai_chat_conversation_list_state,
            ai_chat_message_list_state,
            ai_chat_scrollbar_drag: None,
            ai_chat_database_error,
            ai_chat_model_menu_open: false,
            ai_chat_input_text: String::new(),
            ai_chat_input_selection_range: 0..0,
            ai_chat_input_marked_range: None,
            ai_chat_input_focus: context.focus_handle(),
            ai_chat_input_last_layouts: Vec::new(),
            ai_chat_input_last_bounds: None,
            ai_chat_input_selection_drag: None,
            ai_chat_input_height: AI_CHAT_INPUT_DEFAULT_HEIGHT,
            ai_chat_input_resize_drag: None,
            ai_chat_streaming_task: None,
            next_ai_chat_job_id: 1,
            system_window_appearance: WindowAppearance::Light,
            window_appearance_subscription: None,
            global_keystroke_subscription: None,
            root_focus_handle: context.focus_handle(),
        }
    }

    /// 返回当前主视图实际生效的主题。
    fn effective_theme(&self) -> EffectiveTheme {
        EffectiveTheme::resolve(
            self.settings.theme_preference,
            self.system_window_appearance,
        )
    }

    /// 返回当前主视图调色板。
    fn palette(&self) -> AppThemePalette {
        AppThemePalette::for_theme(self.effective_theme())
    }

    /// 更新系统窗口外观。
    ///
    /// 业务意图：
    /// - 当用户选择“跟随系统”时，系统外观变化应立即驱动当前窗口重绘。
    /// - 即使用户强制选择明亮或暗色，也保存最新系统外观，方便之后切回“跟随系统”时立即正确。
    fn set_system_window_appearance(
        &mut self,
        appearance: WindowAppearance,
        context: &mut Context<Self>,
    ) {
        self.system_window_appearance = appearance;
        context.notify();
    }

    /// 渲染主窗口左侧大导航竖条。
    ///
    /// 业务意图：
    /// - 左侧导航是主窗口唯一的全局功能入口，顶部三个图标切换大功能，底部设置图标打开设置窗口。
    /// - 关于内容已经收入口设置窗口，不再在大导航底部占用独立入口。
    /// - 按用户要求导航本体不显示文字；功能名称只在 hover 气泡中显示。
    ///
    /// 边界条件：
    /// - 导航宽度固定，不随窗口缩放变化；窄屏时优先保留入口可点击性，再由右侧内容区自行处理可用宽度。
    fn render_main_navigation(&self, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette();
        let main_items = MainFeature::all()
            .iter()
            .copied()
            .map(|feature| {
                self.render_main_navigation_button(
                    MainNavigationItem::Feature(feature),
                    self.navigation.active_main_feature == feature,
                    palette,
                    context,
                )
            })
            .collect::<Vec<_>>();

        div()
            .id("main-navigation")
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .justify_between()
            .w(px(MAIN_NAV_WIDTH))
            .h_full()
            .flex_none()
            .py(px(MAIN_NAV_PADDING))
            .bg(rgb(palette.panel))
            .border_r_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(MAIN_NAV_BUTTON_GAP))
                    .children(main_items),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(MAIN_NAV_BUTTON_GAP))
                    .child(self.render_main_navigation_button(
                        MainNavigationItem::Settings,
                        false,
                        palette,
                        context,
                    )),
            )
    }

    /// 渲染左侧大导航中的单个图标按钮。
    ///
    /// 业务意图：
    /// - 按钮本体只渲染图标，hover 状态下把中文名称作为气泡渲染到按钮右侧。
    /// - 设置不是主功能页，不参与选中态；点击后打开设置窗口，关于内容从设置窗口页签访问。
    fn render_main_navigation_button(
        &self,
        item: MainNavigationItem,
        selected: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let hovered = self.navigation.hovered_navigation_item == Some(item);
        let text_color = if selected || hovered {
            palette.accent
        } else {
            palette.muted_text
        };

        div()
            .id(SharedString::from(format!("main-nav-{}", item.label())))
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .w(px(MAIN_NAV_BUTTON_SIZE))
            .h(px(MAIN_NAV_BUTTON_SIZE))
            .rounded(px(8.0))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.panel
            }))
            .text_color(rgb(text_color))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .active(|button| button.opacity(0.82))
            .child(Self::render_lucide_icon(
                Some(item.icon()),
                MAIN_NAV_ICON_WIDTH,
                MAIN_NAV_ICON_SIZE,
                text_color,
            ))
            .on_hover(
                context.listener(move |view, is_hovered: &bool, _window, context| {
                    let next_item = (*is_hovered).then_some(item);
                    if view.navigation.hovered_navigation_item != next_item {
                        view.navigation.hovered_navigation_item = next_item;
                        context.notify();
                    }
                }),
            )
            .on_click(
                context.listener(move |view, _event: &ClickEvent, window, context| {
                    match item {
                        MainNavigationItem::Feature(feature) => {
                            view.select_main_feature(feature, context);
                        }
                        MainNavigationItem::Settings => {
                            view.schedule_open_settings_window(window, context);
                        }
                    }
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染左侧导航 hover 名称气泡覆盖层。
    ///
    /// 业务意图：
    /// - 主导航只显示图标，为了避免用户猜测图标语义，鼠标悬浮时在右侧显示中文功能名称。
    /// - 气泡必须作为主窗口根节点的后置覆盖层绘制；如果作为导航按钮子元素，右侧功能页会在后续绘制中覆盖它。
    fn render_main_navigation_tooltip_overlay(&self) -> gpui::Stateful<gpui::Div> {
        let Some(item) = self.navigation.hovered_navigation_item else {
            return div().id("main-nav-tooltip-empty").hidden();
        };
        let palette = self.palette();
        let tooltip = Self::render_main_navigation_tooltip(item.label(), palette);

        match item.tooltip_anchor() {
            MainNavigationTooltipAnchor::Top(top) => tooltip.top(px(top)),
            MainNavigationTooltipAnchor::Bottom(bottom) => tooltip.bottom(px(bottom)),
        }
    }

    /// 渲染左侧导航 hover 名称气泡本体。
    ///
    /// 业务意图：
    /// - 气泡样式集中在这里，垂直位置由 `MainNavigationItem::tooltip_anchor` 决定，避免按钮布局和根层覆盖层重复写样式。
    fn render_main_navigation_tooltip(
        label: &'static str,
        palette: AppThemePalette,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("main-nav-tooltip-{label}")))
            .absolute()
            .left(px(MAIN_NAV_WIDTH + MAIN_NAV_TOOLTIP_GAP))
            .w(px(MAIN_NAV_TOOLTIP_WIDTH))
            .h(px(MAIN_NAV_TOOLTIP_HEIGHT))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(palette.text))
            .child(label)
    }

    /// 切换主窗口大功能页。
    ///
    /// 业务意图：
    /// - 大导航切换时应收起日志页中的临时弹层，避免用户进入 HPROF 或 AI 页后仍看到旧 tab 菜单、编码下拉或搜索结果菜单。
    fn select_main_feature(&mut self, feature: MainFeature, context: &mut Context<Self>) {
        self.navigation.active_main_feature = feature;
        self.log.load_source_menu = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        self.log.log_tree_context_menu = None;
        self.search.search_results_context_menu = None;
        context.notify();
    }

    /// 构建日志分析页顶部操作栏。
    ///
    /// 业务意图：
    /// - 根级文字工具栏已迁移到左侧大导航；日志页仍需要保留“加载日志”和“搜索”两个高频入口。
    fn render_log_action_bar(&self, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette();

        div()
            .id("log-action-bar")
            .flex()
            .items_center()
            .gap_2()
            .h(px(TOOLBAR_HEIGHT))
            .pl(px(LOG_TREE_ROW_HORIZONTAL_PADDING))
            .pr_4()
            .bg(rgb(palette.panel))
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(self.render_load_toolbar_button(context))
            .child(self.render_search_toolbar_button(context))
    }

    /// 构建“加载日志”工具栏按钮。
    ///
    /// 业务意图：
    /// - 该按钮是当前首个真实业务入口，点击后直接打开系统选择器，避免用户误判为没有响应。
    /// - 与其它工具栏按钮保持相同文字按钮样式，避免功能入口因为有状态而产生视觉突兀。
    ///
    /// 边界条件：
    /// - 按钮左内边距为 0，使图标左缘和加载后目录树标题左缘使用同一条基准线。
    /// - Windows 原生选择器不能在同一个对话框里同时选择文件和目录，因此加载入口会先弹出来源类型菜单。
    fn render_load_toolbar_button(&self, context: &mut Context<Self>) -> impl IntoElement {
        let action = &TOOLBAR_ACTIONS[0];
        let palette = self.palette();

        div()
            .id(SharedString::from(action.label))
            .flex()
            .items_center()
            .gap_1()
            .flex_none()
            .pl(px(0.0))
            .pr(px(TOOLBAR_BUTTON_HORIZONTAL_PADDING))
            .py(px(TOOLBAR_BUTTON_VERTICAL_PADDING))
            .text_sm()
            .text_color(rgb(palette.text))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(move |button| button.text_color(rgb(palette.accent)))
            .active(|button| button.opacity(0.82))
            .child(Self::render_lucide_icon(
                Some(action.icon),
                TOOLBAR_BUTTON_ICON_WIDTH,
                TOOLBAR_BUTTON_ICON_SIZE,
                palette.muted_text,
            ))
            .child(action.label)
            .on_click(context.listener(Self::open_log_sources_prompt))
    }

    /// 构建“搜索”工具栏按钮。
    ///
    /// 业务意图：
    /// - 搜索除了快捷键外必须有可见入口，用户在 macOS/Windows 快捷键被系统或输入法拦截时仍能打开搜索窗口。
    /// - 搜索按钮放在日志分析页操作栏内，只作用于当前日志工作区，不污染 HPROF 和 AI 页的占位状态。
    ///
    /// 边界条件：
    /// - 点击按钮来自鼠标事件，不处于 macOS key equivalent 回调栈中，因此可以直接打开或激活独立搜索窗口。
    /// - 如果日志正文已有选区，沿用快捷键入口的预填逻辑，把选中文本写入搜索关键字。
    fn render_search_toolbar_button(&self, context: &mut Context<Self>) -> impl IntoElement {
        let action = &TOOLBAR_ACTIONS[1];
        let palette = self.palette();

        div()
            .id(SharedString::from(action.label))
            .flex()
            .items_center()
            .gap_1()
            .flex_none()
            .px(px(TOOLBAR_BUTTON_HORIZONTAL_PADDING))
            .py(px(TOOLBAR_BUTTON_VERTICAL_PADDING))
            .text_sm()
            .text_color(rgb(palette.text))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(move |button| button.text_color(rgb(palette.accent)))
            .active(|button| button.opacity(0.82))
            .child(Self::render_lucide_icon(
                Some(action.icon),
                TOOLBAR_BUTTON_ICON_WIDTH,
                TOOLBAR_BUTTON_ICON_SIZE,
                palette.muted_text,
            ))
            .child(action.label)
            .on_click(context.listener(Self::open_search_from_toolbar))
    }

    /// 打开日志来源选择器。
    ///
    /// 业务意图：
    /// - 用户点击“加载日志”后必须立即看到系统选择器反馈，避免工具栏按钮看起来没有响应。
    /// - 支持混选的平台直接打开文件/目录混选选择器；Windows 等不支持混选的平台先展示来源类型菜单。
    fn open_log_sources_prompt(
        &mut self,
        event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if Self::should_show_load_source_menu() {
            let position = event.position();
            self.toggle_load_source_menu(f32::from(position.x), context);
        } else {
            self.begin_path_prompt(LoadPromptKind::LogSources, context);
        }
    }

    /// 从 HPROF 页打开 HPROF 文件选择器。
    ///
    /// 业务意图：
    /// - HPROF 分析只接受单个 dump 文件，不依赖当前日志树状态，也不会清空已经加载的日志工作区。
    /// - 选择器本身无法过滤 `.hprof/.bin`，因此这里只负责拿到路径，真正校验由 HPROF 页后台任务执行并展示错误。
    fn open_hprof_from_toolbar(
        &mut self,
        _event: &ClickEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.begin_hprof_file_prompt(context);
    }

    /// 从工具栏按钮打开搜索窗口。
    ///
    /// 业务意图：
    /// - 该入口和 `Ctrl+F` / `Cmd+F` 使用同一套 `open_search_dialog` 状态初始化逻辑，保证查询词预填、目录目标和已有窗口激活行为一致。
    /// - GPUI 的点击监听执行时 `MainView` 仍处于更新租借中；搜索窗口创建会观察并读取 `MainView`，
    ///   因此这里也必须排到下一帧执行，避免“cannot read MainView while it is already being updated”。
    fn open_search_from_toolbar(
        &mut self,
        _event: &ClickEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.schedule_open_search_dialog(window, context);
    }

    /// 打开 HPROF 文件选择器，并在确认后打开分析窗口。
    ///
    /// 边界条件：
    /// - 用户取消选择时不改变现有 UI。
    /// - 当前只取第一个路径；`multiple=false` 已要求系统选择器只返回单个文件，但这里仍防御性处理平台差异。
    fn begin_hprof_file_prompt(&mut self, context: &mut Context<Self>) {
        let main_view = context.entity();
        context
            .spawn(async move |_view, app| {
                let options = PathPromptOptions {
                    files: true,
                    directories: false,
                    multiple: false,
                    prompt: Some("选择 HPROF dump 文件".into()),
                };
                let receiver = match app.update(|app| app.prompt_for_paths(options)) {
                    Ok(receiver) => receiver,
                    Err(_error) => return,
                };
                let selected_path = match receiver.await {
                    Ok(Ok(Some(paths))) => paths.into_iter().next(),
                    Ok(Ok(None)) | Ok(Err(_)) | Err(_) => None,
                };
                let Some(selected_path) = selected_path else {
                    return;
                };

                app.update(move |app| {
                    Self::open_hprof_analysis_page_after_main_update(main_view, selected_path, app);
                })
                .ok();
            })
            .detach();
    }

    /// 判断当前平台是否需要先展示“加载日志”来源类型菜单。
    ///
    /// 业务意图：
    /// - GPUI 0.2.2 的 Windows 和 Linux 后端目前不能在同一个系统对话框里混选文件与目录。
    /// - Windows 用户已经反馈只看到目录、看不到压缩包，因此这些平台先让用户选择“文件/压缩包”或“目录”。
    ///
    /// 边界条件：
    /// - macOS 后端支持混选，继续保持一次打开系统选择器的原有高效路径。
    fn should_show_load_source_menu() -> bool {
        cfg!(any(target_os = "windows", target_os = "linux"))
    }

    /// 切换加载日志来源类型菜单。
    ///
    /// 业务意图：
    /// - 用户在 Windows 上点击“加载日志”时先看到两个明确入口，避免系统选择器隐藏压缩包文件。
    /// - 再次点击工具栏按钮会收起菜单，符合下拉按钮的常见交互预期。
    fn toggle_load_source_menu(&mut self, window_x: f32, context: &mut Context<Self>) {
        self.log.load_source_menu = if self.log.load_source_menu.is_some() {
            None
        } else {
            Some(LoadSourceMenu {
                x: Self::load_source_menu_x(window_x),
                y: TOOLBAR_HEIGHT + LOAD_SOURCE_MENU_TOP_GAP,
            })
        };
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        self.log.log_tree_context_menu = None;
        self.search.search_results_context_menu = None;
        context.notify();
    }

    /// 计算加载日志来源菜单的横坐标。
    ///
    /// 边界条件：
    /// - 点击可能落在按钮图标或文字上，横向回退后再限制到窗口左边界，避免菜单超出可见区域。
    fn load_source_menu_x(window_x: f32) -> f32 {
        (window_x - LOAD_SOURCE_MENU_POINTER_BACKTRACK).max(0.0)
    }

    /// 执行加载日志来源菜单命令。
    ///
    /// 业务意图：
    /// - 菜单项只负责选择系统对话框类型；最终路径扫描仍复用 `begin_path_prompt`，保证文件、目录、拖拽入口共用加载流程。
    fn select_load_source_kind(
        &mut self,
        prompt_kind: LoadPromptKind,
        context: &mut Context<Self>,
    ) {
        self.log.load_source_menu = None;
        self.begin_path_prompt(prompt_kind, context);
        context.notify();
    }

    /// 渲染加载日志来源菜单的关闭遮罩。
    ///
    /// 业务意图：
    /// - 菜单打开后用户点击菜单外任意位置都应收起，避免悬浮菜单长期遮挡工具栏或内容区。
    fn render_load_source_menu_dismiss_overlay(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        if self.log.load_source_menu.is_none() {
            return div().id("load-source-menu-dismiss-overlay-empty").hidden();
        }

        div()
            .id("load-source-menu-dismiss-overlay")
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, _event: &MouseDownEvent, _window, context| {
                    view.log.load_source_menu = None;
                    context.notify();
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染加载日志来源类型菜单。
    ///
    /// 业务意图：
    /// - Windows 不能混选文件和目录时，通过两个自绘菜单项明确区分“文件/压缩包”和“目录”入口。
    /// - 菜单作为根节点弹层渲染，不受日志目录树是否已经加载、是否隐藏左侧树影响。
    fn render_load_source_menu(&self, context: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = &self.log.load_source_menu else {
            return div().id("load-source-menu-empty").hidden();
        };
        let palette = self.palette();

        div()
            .id("load-source-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(LOAD_SOURCE_MENU_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 菜单自身覆盖在全屏关闭遮罩之上，空白边距点击不能继续落到遮罩或底层工具栏。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 右键点在菜单上只应停留在当前菜单层，避免误触发底层区域的右键处理。
                    context.stop_propagation();
                }),
            )
            .child(self.render_load_source_menu_item(
                LoadPromptKind::LogFilesOrArchives,
                Icon::FileArchive,
                "文件/压缩包",
                palette,
                context,
            ))
            .child(self.render_load_source_menu_item(
                LoadPromptKind::LogDirectories,
                Icon::FolderOpen,
                "目录",
                palette,
                context,
            ))
    }

    /// 渲染加载日志来源菜单单项。
    ///
    /// 业务意图：
    /// - 菜单项用图标区分文件/压缩包和目录，减少 Windows 下需要理解系统选择器模式的负担。
    fn render_load_source_menu_item(
        &self,
        prompt_kind: LoadPromptKind,
        icon: Icon,
        label: &'static str,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("load-source-menu-{label}")))
            .flex()
            .items_center()
            .gap_2()
            .h(px(LOAD_SOURCE_MENU_ITEM_HEIGHT))
            .px_3()
            .text_sm()
            .text_color(rgb(palette.text))
            .cursor_pointer()
            .hover(move |item| item.bg(rgb(palette.hover)).text_color(rgb(palette.accent)))
            .child(Self::render_lucide_icon(
                Some(icon),
                16.0,
                15.0,
                palette.muted_text,
            ))
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                    view.select_load_source_kind(prompt_kind, context);
                    context.stop_propagation();
                }),
            )
    }

    /// 在当前主视图更新结束后打开设置窗口。
    ///
    /// 业务意图：
    /// - 设置窗口会读取并观察 `MainView`，直接在按钮监听中创建会和当前更新租借冲突。
    /// - 重复点击设置按钮时只排队一次，避免同一帧创建多个设置窗口。
    fn schedule_open_settings_window(&mut self, window: &mut Window, context: &mut Context<Self>) {
        if self.settings.settings_window_open_pending {
            return;
        }

        self.settings.settings_window_open_pending = true;
        let main_view = context.entity();
        window.defer(context, move |_window, app| {
            Self::open_settings_window_after_main_update(main_view, app);
        });
        context.notify();
    }

    /// 启动系统路径选择器，并在用户确认后异步扫描路径。
    ///
    /// 业务意图：
    /// - 系统选择器必须在前台应用上下文中打开；文件系统扫描可能较慢，因此放到后台执行器。
    /// - 新一轮扫描完成前会清空旧 tab，避免右侧继续展示不属于当前来源树的日志内容。
    /// - Windows 后端不能混选文件和目录，打开选择器前必须根据平台能力生成参数，避免只显示目录而隐藏压缩包文件。
    ///
    /// 边界条件：
    /// - 用户取消选择时保持现有目录树不变。
    /// - 当前不支持取消后台扫描；如果用户连续触发多次加载，后完成的任务会覆盖先完成的任务。
    fn begin_path_prompt(&mut self, prompt_kind: LoadPromptKind, context: &mut Context<Self>) {
        self.log.load_source_menu = None;
        let loading_message = prompt_kind.loading_message().to_string();

        context
            .spawn(async move |view, app| {
                let receiver = match app.update(|app| {
                    let options =
                        prompt_kind.to_prompt_options(app.can_select_mixed_files_and_dirs());
                    app.prompt_for_paths(options)
                }) {
                    Ok(receiver) => receiver,
                    Err(error) => {
                        view.update(app, |view, context| {
                            view.log.load_state = LogTreeLoadState::Failed {
                                message: format!("无法打开系统路径选择器：{}", error),
                            };
                            context.notify();
                        })
                        .ok();
                        return;
                    }
                };

                let selected_paths: Vec<PathBuf> = match receiver.await {
                    Ok(Ok(Some(paths))) if !paths.is_empty() => paths,
                    Ok(Ok(_)) => return,
                    Ok(Err(error)) => {
                        view.update(app, |view, context| {
                            view.log.load_state = LogTreeLoadState::Failed {
                                message: format!("路径选择器返回错误：{}", error),
                            };
                            context.notify();
                        })
                        .ok();
                        return;
                    }
                    Err(error) => {
                        view.update(app, |view, context| {
                            view.log.load_state = LogTreeLoadState::Failed {
                                message: format!("路径选择器被中断：{}", error),
                            };
                            context.notify();
                        })
                        .ok();
                        return;
                    }
                };

                view.update(app, |view, context| {
                    view.start_log_source_load(selected_paths, loading_message, context);
                })
                .ok();
            })
            .detach();
    }

    /// 启动一轮日志来源加载。
    ///
    /// 业务意图：
    /// - 系统路径选择器、窗口拖放、程序图标拖放和命令行启动都应该复用同一套加载流程。
    /// - 入口统一后，清理旧 tab、重置搜索结果、单日志自动打开和错误展示不会在不同入口之间出现行为差异。
    ///
    /// 边界条件：
    /// - 空路径通常代表用户取消或平台传入了非文件 URL，直接忽略，避免清空当前工作区。
    /// - 后台扫描不能阻塞 GPUI 主线程；完成后再回到主视图更新 UI。
    fn start_log_source_load(
        &mut self,
        selected_paths: Vec<PathBuf>,
        loading_message: String,
        context: &mut Context<Self>,
    ) {
        if selected_paths.is_empty() {
            return;
        }

        self.clear_workspace_for_new_log_load();
        self.log.load_state = LogTreeLoadState::Loading {
            message: loading_message,
        };
        context.notify();

        context
            .spawn(async move |view, app| {
                let load_result = app
                    .background_executor()
                    .spawn(async move { load_log_sources(selected_paths) })
                    .await;

                view.update(app, |view, context| {
                    view.log.load_state = match load_result {
                        Ok(tree) => {
                            view.log.log_tree_scroll_handle = UniformListScrollHandle::new();
                            view.log.log_tree_scrollbar_drag = None;
                            view.log.log_tree_context_menu = None;
                            view.log.log_tree_selected_node_ids.clear();
                            view.log.log_tree_selection_anchor = None;
                            let tree_state = LoadedLogTreeState::new(tree);
                            let single_log_source = tree_state.single_log_source();
                            let load_state = LogTreeLoadState::Loaded(tree_state);

                            // 加载结果只有一个日志时直接打开正文，左侧树由渲染层隐藏。
                            // 这里仍然保留 `Loaded` 状态，搜索当前目录等功能可以继续复用完整来源树。
                            if let Some(source) = single_log_source {
                                view.log.load_state = load_state;
                                view.open_log_file(source, context);
                                return;
                            }

                            load_state
                        }
                        Err(error) => LogTreeLoadState::Failed {
                            message: error.to_string(),
                        },
                    };
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 清理重新加载日志前依赖旧来源树的工作区状态。
    ///
    /// 业务意图：
    /// - 用户重新加载日志后，右侧 tab 和搜索结果都不应继续展示旧目录树中的文件，否则会误以为这些结果来自新加载内容。
    /// - 清理集中在一个函数里，避免后续新增右侧工作区状态时只清 tab、漏掉搜索结果或弹层。
    ///
    /// 边界条件：
    /// - 搜索窗口本身不强制关闭，保留用户输入的关键字；但正在运行的搜索会被置为无效，旧后台回调无法继续写回结果面板。
    /// - 只清理会引用旧日志来源的数据，不重置主题、窗口、左侧宽度等会话级偏好。
    fn clear_workspace_for_new_log_load(&mut self) {
        for tab in &self.log.open_tabs {
            Self::cleanup_tab_paged_resources(tab);
        }
        if let LogTreeLoadState::Loaded(tree_state) = &self.log.load_state {
            tree_state.cleanup_temporary_paths();
        }
        self.log.open_tabs.clear();
        self.log.active_tab_id = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        self.log.log_tree_context_menu = None;
        self.log.save_overwrite_confirm_dialog = None;
        self.log.log_tree_selected_node_ids.clear();
        self.log.log_tree_selection_anchor = None;
        self.log.log_scrollbar_drag = None;
        self.log.log_tree_scrollbar_drag = None;
        self.log.tab_bar_scroll_handle = ScrollHandle::new();

        self.search.search_results_panel = None;
        self.search.search_results_resize_drag = None;
        self.search.search_results_scrollbar_drag = None;
        self.search.search_results_context_menu = None;
        self.search.next_search_job_id += 1;
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.current_file_match_count = None;
            dialog.is_searching = false;
            dialog.progress = SearchProgress::default();
            dialog.message = "日志已重新加载，请重新打开文件后搜索".to_string();
        }
    }

    /// 渲染一个 Lucide 字体图标。
    ///
    /// 业务意图：
    /// - 工具栏和目录树都依赖同一套第三方图标库，集中渲染可以避免字体族、字号和占位宽度分散。
    /// - `icon` 允许为空，用于目录树文件节点的展开箭头占位，保证不同类型节点文本对齐。
    ///
    /// 边界条件：
    /// - 该函数只负责渲染图标字形，不处理点击、悬浮说明或无障碍标签。
    /// - 字体必须已在应用启动时注册，否则图标字符可能被系统字体渲染成错误符号。
    fn render_lucide_icon(
        icon: Option<Icon>,
        width: f32,
        icon_size: f32,
        icon_color: u32,
    ) -> impl IntoElement {
        // Lucide 枚举通过 `char` 映射到字体字形；空图标使用空字符串占位，避免目录树行错位。
        let icon_text = icon
            .map(|icon| char::from(icon).to_string())
            .unwrap_or_default();

        div()
            .w(px(width))
            .flex_none()
            .font_family(LUCIDE_FONT_FAMILY)
            .text_size(px(icon_size))
            .text_color(rgb(icon_color))
            .child(icon_text)
    }
}

impl MainView {
    fn open_thread_analysis_for_sources(
        &mut self,
        sources: Vec<LogFileSource>,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if sources.is_empty() {
            return;
        }
        let source_count = sources.len();
        let filter_rules =
            Self::parse_thread_analysis_filter_rules(self.thread_analysis_filter_effective_text());
        let main_view = context.entity();
        let main_view_for_loading = main_view.clone();
        let loading_analysis = ThreadAnalysisData {
            title: "线程日志分析".to_string(),
            summary: format!("正在分析 {} 个文件...", source_count),
            snapshots: Vec::new(),
            thread_names: Vec::new(),
            matrix: Vec::new(),
        };
        // 先在当前事件循环结束后打开窗口，给用户即时反馈；后台读取和解析完成后再替换为真实结果。
        window.defer(context, move |_window, app| {
            Self::open_thread_analysis_window_after_main_update(
                main_view_for_loading,
                loading_analysis,
                app,
            );
        });
        context
            .spawn(async move |view, app| {
                let analysis = app
                    .background_executor()
                    .spawn(
                        async move { Self::analyze_thread_dump_sources(&sources, &filter_rules) },
                    )
                    .await;

                let _ = view;
                app.update(move |app| {
                    Self::open_thread_analysis_window_after_main_update(main_view, analysis, app);
                })
                .ok();
            })
            .detach();
    }

    /// 读取并分析多个日志来源中的 Java thread dump。
    ///
    /// 业务意图：
    /// - 分析入口接受 `LogFileSource`，复用现有读取和自动编码识别逻辑，避免另建一套文件/压缩包读取路径。
    ///
    /// 边界条件：
    /// - 某个文件读取或解码失败时跳过该文件，继续分析其它文件，避免单个坏文件阻断整批分析。
    /// - 如果没有识别到任何快照，返回空分析数据，窗口会展示“未识别到快照”的摘要。
    fn analyze_thread_dump_sources(
        sources: &[LogFileSource],
        filter_rules: &[ThreadAnalysisFilterRule],
    ) -> ThreadAnalysisData {
        let mut snapshots = Vec::new();
        let mut skipped_files = 0usize;
        for (source_index, source) in sources.iter().enumerate() {
            let source_name = source.display_name();
            let result = read_log_source_bytes(source)
                .and_then(|bytes| decode_log_bytes(&bytes, EncodingChoice::Auto, &source_name));
            match result {
                Ok(document) => {
                    snapshots.extend(Self::parse_thread_dump_snapshots(
                        &document.lines,
                        &source_name,
                        source_index,
                        source,
                    ));
                }
                Err(_) => {
                    skipped_files += 1;
                }
            }
        }

        Self::build_thread_analysis_data(sources.len(), skipped_files, snapshots, filter_rules)
    }

    /// 从解码后的日志行中解析 Java thread dump 快照。
    ///
    /// 业务意图：
    /// - Java thread dump 以 `Full thread dump` 作为快照边界，线程头通常以双引号线程名开头，
    ///   状态行包含 `java.lang.Thread.State:`；解析这两个稳定特征即可形成线程状态时间线。
    ///
    /// 边界条件：
    /// - 线程日志常见格式会先输出打印时间，再输出 `Full thread dump`；这里优先把打印时间作为横轴标签。
    /// - 如果缺失状态行，当前线程不会加入快照，避免用未知状态污染时间线。
    fn parse_thread_dump_snapshots(
        lines: &[String],
        source_name: &str,
        source_index: usize,
        source: &LogFileSource,
    ) -> Vec<ThreadSnapshot> {
        let mut snapshots = Vec::new();
        let mut current_snapshot: Option<ThreadSnapshot> = None;
        let mut pending_thread: Option<ThreadStateSamplePending> = None;
        let mut last_timestamp: Option<String> = None;

        for (line_index, line) in lines.iter().enumerate() {
            if line.contains("Full thread dump") {
                if let (Some(snapshot), Some(pending)) =
                    (current_snapshot.as_mut(), pending_thread.take())
                {
                    Self::finish_pending_thread_sample(snapshot, pending);
                }
                if let Some(snapshot) = current_snapshot.take()
                    && !snapshot.threads.is_empty()
                {
                    snapshots.push(snapshot);
                }
                let label = last_timestamp
                    .clone()
                    .unwrap_or_else(|| format!("{} #{}", source_name, snapshots.len() + 1));
                current_snapshot = Some(ThreadSnapshot {
                    label,
                    source_index,
                    source: source.clone(),
                    threads: Vec::new(),
                });
                pending_thread = None;
                continue;
            }
            if let Some(timestamp) = Self::extract_thread_dump_timestamp(line) {
                last_timestamp = Some(timestamp);
            }

            let Some(snapshot) = current_snapshot.as_mut() else {
                continue;
            };
            if let Some((thread_name, thread_id)) = Self::parse_thread_header_details(line) {
                if let Some(pending) = pending_thread.take() {
                    Self::finish_pending_thread_sample(snapshot, pending);
                }
                pending_thread = Some(ThreadStateSamplePending {
                    name: thread_name,
                    thread_id,
                    line_index,
                    stack_lines: vec![line.clone()],
                    state: None,
                });
                continue;
            }
            if let Some(pending) = pending_thread.as_mut() {
                pending.stack_lines.push(line.clone());
                if let Some(state_text) = line.split("java.lang.Thread.State:").nth(1) {
                    pending.state = Some(ThreadStateKind::parse(state_text));
                }
            }
        }

        if let (Some(snapshot), Some(pending)) = (current_snapshot.as_mut(), pending_thread.take())
        {
            Self::finish_pending_thread_sample(snapshot, pending);
        }
        if let Some(snapshot) = current_snapshot
            && !snapshot.threads.is_empty()
        {
            snapshots.push(snapshot);
        }

        snapshots
    }

    /// 将已收集完的线程片段写入当前快照。
    ///
    /// 业务意图：
    /// - 线程头、状态行和后续堆栈帧分散在多行；只有遇到下一个线程或快照边界时才知道完整片段。
    /// - 没有状态行的片段不生成样本，避免未知状态污染时间线；有状态行的片段保留完整堆栈供设置过滤匹配。
    fn finish_pending_thread_sample(
        snapshot: &mut ThreadSnapshot,
        pending: ThreadStateSamplePending,
    ) {
        let Some(state) = pending.state else {
            return;
        };
        let preview_lines = Self::thread_stack_preview_lines(&pending.stack_lines);
        snapshot.threads.push(ThreadStateSample {
            name: pending.name,
            thread_id: pending.thread_id,
            state,
            line_index: pending.line_index,
            preview_lines,
            stack_lines: pending.stack_lines,
        });
    }

    /// 提取单个线程片段的前 5 行预览。
    ///
    /// 业务意图：
    /// - 悬浮气泡用于查看当前色块对应线程的原始上下文，不能把下一个线程头或下一个 dump 快照混入预览。
    /// - 预览限制为 5 行，避免超长堆栈在气泡中占满窗口。
    fn thread_stack_preview_lines(stack_lines: &[String]) -> Vec<String> {
        stack_lines.iter().take(5).cloned().collect()
    }

    /// 提取 thread dump 附近的时间戳文案。
    ///
    /// 业务意图：
    /// - 用户要求横轴为时间线；常见日志会在 dump 前输出 `YYYY-MM-DD HH:MM:SS` 或 JVM 日期行。
    /// - 不引入时间解析依赖，只提取稳定前缀作为显示标签，避免因时区或本地化月份解析失败丢失标签。
    fn extract_thread_dump_timestamp(line: &str) -> Option<String> {
        let trimmed = line.trim();
        if let Some(timestamp) =
            Self::extract_thread_dump_timestamp_after_marker(trimmed, "打印时间")
        {
            return Some(timestamp);
        }
        if let Some(timestamp) =
            Self::extract_thread_dump_timestamp_after_marker(trimmed, "print time")
        {
            return Some(timestamp);
        }
        if let Some(timestamp) =
            Self::extract_thread_dump_timestamp_after_marker(trimmed, "dump time")
        {
            return Some(timestamp);
        }
        if let Some(timestamp) = Self::extract_leading_datetime_label(trimmed) {
            return Some(timestamp);
        }
        if trimmed.len() >= 24
            && trimmed
                .chars()
                .take(3)
                .all(|character| character.is_ascii_alphabetic())
            && trimmed.as_bytes().get(3) == Some(&b' ')
            && trimmed.as_bytes().get(7) == Some(&b' ')
            && trimmed.contains(':')
        {
            return Some(trimmed.chars().take(24).collect());
        }
        None
    }

    /// 从包含“打印时间”标记的日志行中提取时间部分。
    ///
    /// 业务意图：
    /// - 线程日志可能用 `线程日志打印时间：2026-...` 这类前缀描述 dump 生成时间，
    ///   横轴应展示真正的打印时间，而不是整行说明文字。
    fn extract_thread_dump_timestamp_after_marker(line: &str, marker: &str) -> Option<String> {
        let lower_line = line.to_ascii_lowercase();
        let marker_index = lower_line.find(&marker.to_ascii_lowercase())?;
        let after_marker = &line[marker_index + marker.len()..];
        let trimmed = after_marker
            .trim_start_matches(|character: char| {
                character.is_whitespace()
                    || matches!(character, ':' | '：' | '=' | '-' | '>' | '】' | ']')
            })
            .trim();
        Self::extract_leading_datetime_label(trimmed)
    }

    /// 提取行首常见时间标签。
    ///
    /// 边界条件：
    /// - 当前不做严格日期合法性校验，只识别日志中稳定的 `YYYY-MM-DD HH:MM:SS` 展示形态。
    fn extract_leading_datetime_label(text: &str) -> Option<String> {
        if text.len() >= 19
            && text.as_bytes().get(4) == Some(&b'-')
            && text.as_bytes().get(7) == Some(&b'-')
            && text.as_bytes().get(10) == Some(&b' ')
            && text.as_bytes().get(13) == Some(&b':')
            && text.as_bytes().get(16) == Some(&b':')
        {
            Some(text[..19].to_string())
        } else {
            None
        }
    }

    /// 从 Java thread dump 线程头中解析线程名和线程 ID。
    ///
    /// 边界条件：
    /// - 标准 HotSpot 线程头以 `"线程名"` 开头；不符合该形态的行直接忽略。
    /// - 线程 ID 优先使用 `#123` 形式，缺失时回退到 `tid=0x...`，保证不同 JVM 输出都能提供可核对标识。
    fn parse_thread_header_details(line: &str) -> Option<(String, Option<String>)> {
        let trimmed = line.trim_start();
        let rest = trimmed.strip_prefix('"')?;
        let end = rest.find('"')?;
        let thread_name = rest[..end].to_string();
        let metadata = rest[end + 1..].trim();
        let thread_id = metadata
            .split_whitespace()
            .find_map(|part| part.strip_prefix('#').map(|id| format!("#{id}")))
            .or_else(|| {
                metadata
                    .split_whitespace()
                    .find_map(|part| part.strip_prefix("tid=").map(|id| format!("tid={id}")))
            });
        Some((thread_name, thread_id))
    }

    /// 解析线程日志分析过滤配置文本。
    ///
    /// 业务意图：
    /// - 设置页允许用户用空行分隔多段堆栈；每段堆栈去除行首尾空白后形成一条连续片段匹配规则。
    /// - 空段和空行不形成规则，避免用户粘贴时多余空白导致所有线程都不匹配或产生无意义规则。
    pub(super) fn parse_thread_analysis_filter_rules(raw: &str) -> Vec<ThreadAnalysisFilterRule> {
        let normalized = normalize_thread_analysis_filter_text(raw);
        let mut rules = Vec::new();
        let mut current_lines = Vec::new();
        for line in normalized.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                if !current_lines.is_empty() {
                    rules.push(ThreadAnalysisFilterRule {
                        lines: std::mem::take(&mut current_lines),
                    });
                }
            } else {
                current_lines.push(trimmed.to_string());
            }
        }
        if !current_lines.is_empty() {
            rules.push(ThreadAnalysisFilterRule {
                lines: current_lines,
            });
        }
        rules
    }

    /// 判断某个线程完整堆栈是否命中过滤规则。
    ///
    /// 业务意图：
    /// - “无效线程”通常由一段稳定堆栈片段识别；要求规则行连续出现可以降低只凭单行类名误过滤其它线程的风险。
    pub(super) fn thread_stack_matches_filter_rule(
        stack_lines: &[String],
        rule: &ThreadAnalysisFilterRule,
    ) -> bool {
        if rule.lines.is_empty() || stack_lines.len() < rule.lines.len() {
            return false;
        }
        stack_lines.windows(rule.lines.len()).any(|window| {
            window
                .iter()
                .map(|line| line.trim())
                .eq(rule.lines.iter().map(String::as_str))
        })
    }

    /// 判断某个线程是否应被线程日志分析过滤规则移除。
    fn thread_sample_matches_filter_rules(
        sample: &ThreadStateSample,
        filter_rules: &[ThreadAnalysisFilterRule],
    ) -> bool {
        filter_rules
            .iter()
            .any(|rule| Self::thread_stack_matches_filter_rule(&sample.stack_lines, rule))
    }

    /// 构建线程分析窗口可直接渲染的数据矩阵。
    ///
    /// 业务意图：
    /// - 解析阶段按快照保存线程列表；渲染阶段需要按线程名聚合成二维矩阵，横轴为快照，纵轴为线程。
    /// - 用户要求只在单个线程日志中出现的线程默认不显示，因此多文件分析时只保留跨文件出现的线程。
    fn build_thread_analysis_data(
        source_count: usize,
        skipped_files: usize,
        mut snapshots: Vec<ThreadSnapshot>,
        filter_rules: &[ThreadAnalysisFilterRule],
    ) -> ThreadAnalysisData {
        let mut filtered_threads = 0usize;
        if !filter_rules.is_empty() {
            for snapshot in &mut snapshots {
                let before = snapshot.threads.len();
                snapshot.threads.retain(|sample| {
                    !Self::thread_sample_matches_filter_rules(sample, filter_rules)
                });
                filtered_threads += before.saturating_sub(snapshot.threads.len());
            }
        }
        let _has_snapshot_labels = snapshots.iter().any(|snapshot| !snapshot.label.is_empty());
        let visible_thread_name_set = Self::default_visible_thread_names(&snapshots, source_count);
        let mut thread_names = Vec::new();
        let mut seen_thread_names = BTreeSet::new();
        for snapshot in &snapshots {
            for sample in &snapshot.threads {
                if visible_thread_name_set.contains(&sample.name)
                    && seen_thread_names.insert(sample.name.clone())
                {
                    thread_names.push(sample.name.clone());
                }
            }
        }

        let thread_index_by_name = thread_names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index))
            .collect::<HashMap<_, _>>();
        let mut matrix = vec![vec![None; snapshots.len()]; thread_names.len()];
        for (snapshot_index, snapshot) in snapshots.iter().enumerate() {
            for sample in &snapshot.threads {
                if let Some(thread_index) = thread_index_by_name.get(&sample.name) {
                    matrix[*thread_index][snapshot_index] = Some(Arc::new(ThreadTimelineCell {
                        state: sample.state,
                        time_label: snapshot.label.clone(),
                        thread_name: sample.name.clone(),
                        thread_id: sample.thread_id.clone(),
                        source: snapshot.source.clone(),
                        line_index: sample.line_index,
                        preview_lines: sample.preview_lines.clone(),
                    }));
                }
            }
        }

        let summary = if snapshots.is_empty() {
            format!(
                "未识别到 Java thread dump 快照，已跳过 {} 个无法读取或解码的文件",
                skipped_files
            )
        } else {
            format!(
                "{} 个文件，{} 个快照，{} 个线程，跳过 {} 个文件，过滤 {} 个线程",
                source_count,
                snapshots.len(),
                thread_names.len(),
                skipped_files,
                filtered_threads
            )
        };
        ThreadAnalysisData {
            title: "线程日志分析".to_string(),
            summary,
            snapshots,
            thread_names,
            matrix,
        }
    }

    /// 返回线程分析默认可见线程名集合。
    ///
    /// 业务意图：
    /// - 多个线程日志一起分析时，默认隐藏只出现在单个日志文件中的线程，突出跨时间/跨文件持续存在的线程。
    /// - 单文件分析时没有“跨文件”可比较对象，因此保留该文件内所有线程，避免窗口空白。
    fn default_visible_thread_names(
        snapshots: &[ThreadSnapshot],
        source_count: usize,
    ) -> HashSet<String> {
        let mut sources_by_thread = HashMap::<String, HashSet<usize>>::new();
        for snapshot in snapshots {
            for sample in &snapshot.threads {
                sources_by_thread
                    .entry(sample.name.clone())
                    .or_default()
                    .insert(snapshot.source_index);
            }
        }

        sources_by_thread
            .into_iter()
            .filter_map(|(thread_name, source_indexes)| {
                (source_count <= 1 || source_indexes.len() > 1).then_some(thread_name)
            })
            .collect()
    }

    /// 在主视图更新租借结束后打开或更新线程分析独立窗口。
    ///
    /// 业务意图：
    /// - 分析窗口可重复使用；如果用户重新分析另一组文件，直接替换窗口内容并激活。
    /// - 窗口创建需要在 `App` 上下文中执行，避免在菜单点击的 `MainView` 更新栈里重入读取同一个视图。
    fn open_thread_analysis_window_after_main_update(
        main_view: Entity<MainView>,
        analysis: ThreadAnalysisData,
        app: &mut App,
    ) {
        let existing_window = main_view.update(app, |view, context| {
            view.log.log_tree_context_menu = None;
            context.notify();
            view.thread_analysis_window
        });
        if let Some(window_handle) = existing_window {
            if window_handle
                .update(app, |window_view, window, context| {
                    window_view.set_analysis(analysis.clone(), context);
                    window.activate_window();
                })
                .is_ok()
            {
                return;
            }
            main_view.update(app, |view, _| {
                view.thread_analysis_window = None;
            });
        }

        let main_view_for_window = main_view.clone();
        let main_view_for_close = main_view.clone();
        let analysis_for_window = analysis.clone();
        let window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("线程日志分析".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::centered(
                size(
                    px(THREAD_ANALYSIS_WINDOW_WIDTH),
                    px(THREAD_ANALYSIS_WINDOW_HEIGHT),
                ),
                app,
            )),
            is_resizable: true,
            is_minimizable: true,
            window_min_size: Some(size(px(720.0), px(420.0))),
            ..Default::default()
        };

        match app.open_window(window_options, move |window, app| {
            window.on_window_should_close(app, move |_, app| {
                main_view_for_close.update(app, |view, context| {
                    view.thread_analysis_window = None;
                    context.notify();
                });
                true
            });
            app.new(|context| {
                ThreadAnalysisWindowView::new(main_view_for_window, analysis_for_window, context)
            })
        }) {
            Ok(window_handle) => {
                main_view.update(app, |view, context| {
                    view.thread_analysis_window = Some(window_handle);
                    context.notify();
                });
            }
            Err(_) => {
                main_view.update(app, |view, context| {
                    view.thread_analysis_window = None;
                    context.notify();
                });
            }
        }
    }

    /// 判断某个日志目录树节点当前是否展开。
    ///
    /// 业务意图：
    /// - 渲染虚拟列表可见行时需要给文件夹选择正确箭头方向。
    ///
    /// 边界条件：
    /// - 非已加载状态下没有目录树，统一返回 `false`，避免旧事件或重绘路径访问不存在的树状态。
    fn is_log_tree_node_expanded(&self, node_id: usize) -> bool {
        match &self.log.load_state {
            LogTreeLoadState::Loaded(tree_state) => tree_state.is_expanded(node_id),
            LogTreeLoadState::Empty
            | LogTreeLoadState::Loading { .. }
            | LogTreeLoadState::Failed { .. } => false,
        }
    }

    /// 返回单文件压缩包根节点可以直接打开的内部文件来源。
    ///
    /// 业务意图：
    /// - 该方法把“压缩包只有一个文件时直接打开”的交互规则收口在主视图，避免渲染函数理解完整树扫描细节。
    /// - 非加载状态和非压缩包节点统一返回 `None`，调用方可以继续走普通展开/收起逻辑。
    fn single_file_archive_source(&self, node_id: usize) -> Option<LogFileSource> {
        match &self.log.load_state {
            LogTreeLoadState::Loaded(tree_state) => {
                tree_state.single_file_source_for_archive(node_id)
            }
            LogTreeLoadState::Empty
            | LogTreeLoadState::Loading { .. }
            | LogTreeLoadState::Failed { .. } => None,
        }
    }

    /// 切换日志目录树节点的展开状态。
    ///
    /// 业务意图：
    /// - 用户点击文件夹或压缩包行时，左侧树应立即展开或收起对应子树。
    /// - 切换后只重建可见行缓存，完整加载结果保持不变，避免重复扫描文件系统或压缩包。
    ///
    /// 边界条件：
    /// - 只有当前已加载状态会响应；加载中、失败或未加载状态下的旧点击事件会被忽略。
    fn toggle_log_tree_node(&mut self, node_id: usize, context: &mut Context<Self>) {
        if let LogTreeLoadState::Loaded(tree_state) = &mut self.log.load_state {
            tree_state.toggle_node(node_id);
            context.notify();
        }
    }

    /// 打开左侧目录树中的日志文件。
    ///
    /// 业务意图：
    /// - 普通文件和压缩包成员都通过 `LogFileSource` 统一进入右侧 tab 工作区。
    /// - 同一个来源重复点击时只激活已有 tab，避免用户误打开多个相同文件。
    ///
    /// 边界条件：
    /// - 读取和自动解码都放到后台执行器，避免大文件或压缩包流式读取阻塞 GPUI 主线程。
    /// - 如果 tab 在后台任务完成前被关闭，结果合并时会因找不到 ID 而被忽略。
    fn open_log_file(&mut self, source: LogFileSource, context: &mut Context<Self>) {
        let source_key = source.stable_key();
        if let Some(tab) = self
            .log
            .open_tabs
            .iter()
            .find(|tab| tab.source_key == source_key)
        {
            self.activate_tab(tab.id);
            context.notify();
            return;
        }

        let tab_id = self.log.next_tab_id;
        self.log.next_tab_id += 1;
        let title = source.display_name();
        if self.log.open_tabs.is_empty() {
            self.log.tab_bar_scroll_handle = ScrollHandle::new();
        }
        self.log.open_tabs.push(OpenLogTab {
            id: tab_id,
            source: source.clone(),
            source_key,
            title,
            encoding_choice: EncodingChoice::Auto,
            raw_bytes: None,
            state: LogTabState::Loading {
                message: "正在读取日志文件...".to_string(),
            },
            scroll_handle: UniformListScrollHandle::new(),
            paged_viewport_handle: ScrollHandle::new(),
            paged_scroll: PagedLogScrollState::default(),
            pending_scroll_to_line: None,
            highlighted_search_line: None,
            marked_lines: BTreeSet::new(),
            last_marker_jump_line: None,
            text_selection: None,
            selection_drag_anchor: None,
        });
        self.activate_tab(tab_id);
        context.notify();
        self.spawn_log_tab_load(tab_id, source, context);
    }

    /// 激活指定日志 tab 并确保 tab 页签在横向滚动区域内可见。
    ///
    /// 业务意图：
    /// - 打开、点击、右键和后台内容刷新都会改变用户关注的 tab，tab 栏需要自动滚动到能看到当前 tab 的位置。
    /// - 将激活逻辑集中到一个方法，避免不同入口遗漏关闭弹层或遗漏页签自动定位。
    ///
    /// 边界条件：
    /// - 如果 tab 已经被关闭或不存在，直接忽略，避免旧异步任务或过期点击事件激活不存在的页面。
    fn activate_tab(&mut self, tab_id: usize) {
        if !self.log.open_tabs.iter().any(|tab| tab.id == tab_id) {
            return;
        }

        self.log.active_tab_id = Some(tab_id);
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        self.clear_search_current_file_match_count();
        self.scroll_tab_bar_to_tab(tab_id);
    }

    /// 将指定 tab 页签滚动到可视范围内。
    ///
    /// 业务意图：
    /// - tab 标题完整展示后，页签总宽度可能超过右侧区域；激活 tab 时自动定位能减少用户手动点箭头的次数。
    /// - GPUI `ScrollHandle::scroll_to_item` 会在下一次布局时按 child 下标做最小滚动，适合这里的横向页签列表。
    fn scroll_tab_bar_to_tab(&self, tab_id: usize) {
        if let Some(index) = self.log.open_tabs.iter().position(|tab| tab.id == tab_id) {
            self.log.tab_bar_scroll_handle.scroll_to_item(index);
        }
    }

    /// 将指定 tab 的日志正文滚动到目标行。
    ///
    /// 业务意图：
    /// - 搜索结果点击后需要把命中行带到用户视野中央，而不是只打开文件让用户手动查找。
    /// - 使用 `UniformListScrollHandle::scroll_to_item_strict` 延迟到下一次布局执行，适合虚拟列表尚未完成测量的场景。
    ///
    /// 边界条件：
    /// - 只有已打开 tab 才能滚动；如果 tab 仍在加载，调用方应先把行号写入 `pending_scroll_to_line`。
    /// - 行号来自搜索时的解码结果，如果文件在搜索后被外部修改，滚动位置可能不再对应同一内容，这是当前未实现文件监听的已知边界。
    fn scroll_log_tab_to_line(&mut self, tab_id: usize, line_index: usize) {
        if let Some(tab) = self.log.open_tabs.iter_mut().find(|tab| tab.id == tab_id) {
            match &tab.state {
                LogTabState::Ready {
                    document: LogTabDocument::Paged(document),
                } => {
                    // 分页日志不再把完整行数交给 GPUI，因此跳转行号时直接更新应用侧的逻辑滚动位置。
                    // 首帧还没有真实视口高度时使用一屏常见行数估算，下一帧滚动条会按实际 bounds 修正比例。
                    let viewport_height = tab.paged_viewport_handle.bounds().size.height;
                    let fallback_height = px(LOG_VIEWER_ROW_HEIGHT * 24.0);
                    let viewport_height = if viewport_height > px(0.0) {
                        viewport_height
                    } else {
                        fallback_height
                    };
                    tab.paged_scroll.top_px = Self::paged_log_scroll_top_for_line(
                        line_index,
                        document.line_count(),
                        viewport_height,
                    );
                }
                LogTabState::Ready {
                    document: LogTabDocument::InMemory(_),
                } => {
                    tab.scroll_handle
                        .scroll_to_item_strict(line_index, ScrollStrategy::Center);
                }
                LogTabState::Loading { .. } | LogTabState::Failed { .. } => {}
            }
        }
    }

    /// 打开日志来源并定位到指定行。
    ///
    /// 业务意图：
    /// - 线程分析窗口和搜索结果都需要“打开文件并跳到某行”的行为；集中实现可以保证新建 tab、
    ///   已打开 tab、加载中 tab 的滚动和高亮状态一致。
    ///
    /// 边界条件：
    /// - 如果目标 tab 仍在后台读取，先记录 `pending_scroll_to_line`，等加载完成后再滚动。
    /// - 行号来自当前解析结果；如果文件在分析后被外部修改，定位可能落到相邻内容，这是无文件监听条件下的既有风险。
    fn open_log_source_at_line(
        &mut self,
        source: LogFileSource,
        line_index: usize,
        context: &mut Context<Self>,
    ) {
        let source_key = source.stable_key();
        if !self
            .log
            .open_tabs
            .iter()
            .any(|tab| tab.source_key == source_key)
        {
            self.open_log_file(source, context);
        }

        let Some(tab_index) = self
            .log
            .open_tabs
            .iter()
            .position(|tab| tab.source_key == source_key)
        else {
            return;
        };
        let tab_id = self.log.open_tabs[tab_index].id;
        let ready = matches!(
            self.log.open_tabs[tab_index].state,
            LogTabState::Ready { .. }
        );
        self.log.open_tabs[tab_index].pending_scroll_to_line = Some(line_index);
        self.log.open_tabs[tab_index].highlighted_search_line = Some(line_index);
        self.activate_tab(tab_id);
        if ready {
            self.log.open_tabs[tab_index].pending_scroll_to_line = None;
            self.scroll_log_tab_to_line(tab_id, line_index);
        }
        context.notify();
    }

    /// 启动日志 tab 的后台读取任务。
    ///
    /// 业务意图：
    /// - 读取压缩包成员和解码大文本都可能耗时，必须离开 UI 线程执行。
    /// - 后台任务返回时只按 tab ID 合并状态，避免持有过期引用。
    fn spawn_log_tab_load(
        &self,
        tab_id: usize,
        source: LogFileSource,
        context: &mut Context<Self>,
    ) {
        let source_name = source.display_name();
        context
            .spawn(async move |view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move {
                        match open_log_source_for_tab(source, EncodingChoice::Auto, &source_name) {
                            Ok(LargeLogOpenResult::InMemoryReady {
                                raw_bytes,
                                document,
                            }) => LogTabLoadResult::Ready {
                                raw_bytes: Some(raw_bytes),
                                document: LogTabDocument::InMemory(document),
                            },
                            Ok(LargeLogOpenResult::InMemoryDecodeFailed { raw_bytes, message }) => {
                                LogTabLoadResult::DecodeFailed { raw_bytes, message }
                            }
                            Ok(LargeLogOpenResult::PagedReady { document }) => {
                                LogTabLoadResult::Ready {
                                    raw_bytes: None,
                                    document: LogTabDocument::Paged(document),
                                }
                            }
                            Err(error) => LogTabLoadResult::ReadFailed {
                                message: error.to_string(),
                            },
                        }
                    })
                    .await;

                view.update(app, |view, context| {
                    view.apply_log_tab_load_result(tab_id, result);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 合并日志 tab 的后台读取结果。
    ///
    /// 业务意图：
    /// - 读取成功后保存原始字节；即使自动检测失败，也允许用户通过手动编码按钮重新解析。
    fn apply_log_tab_load_result(&mut self, tab_id: usize, result: LogTabLoadResult) {
        let mut pending_scroll_to_line = None;
        {
            let Some(tab) = self.log.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
                return;
            };

            tab.scroll_handle = UniformListScrollHandle::new();
            tab.paged_viewport_handle = ScrollHandle::new();
            tab.paged_scroll = PagedLogScrollState::default();
            tab.text_selection = None;
            tab.selection_drag_anchor = None;
            tab.marked_lines.clear();
            tab.last_marker_jump_line = None;
            match result {
                LogTabLoadResult::Ready {
                    raw_bytes,
                    document,
                } => {
                    tab.raw_bytes = raw_bytes;
                    tab.state = LogTabState::Ready { document };
                    pending_scroll_to_line = tab.pending_scroll_to_line.take();
                }
                LogTabLoadResult::DecodeFailed { raw_bytes, message } => {
                    tab.raw_bytes = Some(raw_bytes);
                    tab.state = LogTabState::Failed { message };
                    tab.pending_scroll_to_line = None;
                }
                LogTabLoadResult::ReadFailed { message } => {
                    tab.raw_bytes = None;
                    tab.state = LogTabState::Failed { message };
                    tab.pending_scroll_to_line = None;
                }
            }
        }

        if self.log.active_tab_id == Some(tab_id) {
            self.scroll_tab_bar_to_tab(tab_id);
            self.clear_search_current_file_match_count();
        }
        if let Some(line_index) = pending_scroll_to_line {
            self.scroll_log_tab_to_line(tab_id, line_index);
        }
    }

    /// 为指定 tab 切换编码并重新解码。
    ///
    /// 业务意图：
    /// - 编码切换必须复用 tab 内保存的原始字节，避免重新读取压缩包成员或普通文件。
    /// - 切换后重置该 tab 的正文滚动句柄，让用户从文件开头重新检查解码结果。
    fn select_tab_encoding(
        &mut self,
        tab_id: usize,
        encoding_choice: EncodingChoice,
        context: &mut Context<Self>,
    ) {
        let Some(tab) = self.log.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };

        let raw_bytes = tab.raw_bytes.clone();
        let paged_document = match &tab.state {
            LogTabState::Ready {
                document: LogTabDocument::Paged(document),
            } => Some(document.clone()),
            LogTabState::Ready {
                document: LogTabDocument::InMemory(_),
            }
            | LogTabState::Loading { .. }
            | LogTabState::Failed { .. } => None,
        };
        if raw_bytes.is_none() && paged_document.is_none() {
            // 原始字节或分页文档尚未读取完成时不能提前写入编码选择，否则后台自动加载完成后会出现
            // “下拉框显示手动编码、正文却来自自动识别”的状态不一致。
            self.log.tab_context_menu = None;
            self.log.encoding_dropdown_menu = None;
            self.log.log_viewer_context_menu = None;
            context.notify();
            return;
        }

        tab.encoding_choice = encoding_choice;
        tab.scroll_handle = UniformListScrollHandle::new();
        tab.pending_scroll_to_line = None;
        tab.highlighted_search_line = None;
        tab.last_marker_jump_line = None;
        tab.text_selection = None;
        tab.selection_drag_anchor = None;
        tab.state = LogTabState::Loading {
            message: format!("正在按 {} 重新解析...", encoding_choice.label()),
        };
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        let source_name = tab.title.clone();
        if self
            .log
            .log_scrollbar_drag
            .is_some_and(|drag| drag.tab_id == tab_id)
        {
            self.log.log_scrollbar_drag = None;
        }
        context.notify();
        if let Some(raw_bytes) = raw_bytes {
            self.spawn_log_tab_decode(tab_id, raw_bytes, encoding_choice, source_name, context);
        } else if let Some(document) = paged_document {
            self.spawn_paged_log_tab_decode(tab_id, document, encoding_choice, context);
        }
    }

    /// 启动日志 tab 的后台重新解码任务。
    ///
    /// 业务意图：
    /// - 内存模式日志重新解码仍可能耗时，放到后台执行可以避免界面短暂停顿。
    fn spawn_log_tab_decode(
        &self,
        tab_id: usize,
        raw_bytes: Arc<Vec<u8>>,
        encoding_choice: EncodingChoice,
        source_name: String,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move {
                        decode_log_bytes(&raw_bytes, encoding_choice, &source_name)
                            .map(|document| LogTabDecodeResult::Ready {
                                encoding_choice,
                                document: LogTabDocument::InMemory(document),
                            })
                            .unwrap_or_else(|error: LogContentError| LogTabDecodeResult::Failed {
                                encoding_choice,
                                message: error.to_string(),
                            })
                    })
                    .await;

                view.update(app, |view, context| {
                    view.apply_log_tab_decode_result(tab_id, result);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 启动分页日志 tab 的后台编码切换任务。
    ///
    /// 业务意图：
    /// - 超大日志切换编码时不能重新读取压缩包或重建行索引，只更新分页文档的解码策略并清理可见行缓存。
    /// - 该操作仍放到后台执行，避免自动检测编码样本读取影响 UI 响应。
    fn spawn_paged_log_tab_decode(
        &self,
        tab_id: usize,
        document: paged_document::PagedLogDocument,
        encoding_choice: EncodingChoice,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move {
                        document
                            .with_encoding(encoding_choice)
                            .map(|document| LogTabDecodeResult::Ready {
                                encoding_choice,
                                document: LogTabDocument::Paged(document),
                            })
                            .unwrap_or_else(|error| LogTabDecodeResult::Failed {
                                encoding_choice,
                                message: error.to_string(),
                            })
                    })
                    .await;

                view.update(app, |view, context| {
                    view.apply_log_tab_decode_result(tab_id, result);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 合并日志 tab 的重新解码结果。
    ///
    /// 边界条件：
    /// - 如果用户在解码完成前关闭了 tab，则结果会被忽略。
    /// - 如果用户在旧任务完成前再次切换编码，则旧结果会被丢弃，避免 UI 显示内容和编码按钮不一致。
    fn apply_log_tab_decode_result(&mut self, tab_id: usize, result: LogTabDecodeResult) {
        {
            let Some(tab) = self.log.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
                return;
            };

            let result_encoding_choice = match &result {
                LogTabDecodeResult::Ready {
                    encoding_choice, ..
                }
                | LogTabDecodeResult::Failed {
                    encoding_choice, ..
                } => *encoding_choice,
            };
            if tab.encoding_choice != result_encoding_choice {
                return;
            }

            tab.scroll_handle = UniformListScrollHandle::new();
            tab.paged_viewport_handle = ScrollHandle::new();
            tab.paged_scroll = PagedLogScrollState::default();
            tab.text_selection = None;
            tab.selection_drag_anchor = None;
            tab.last_marker_jump_line = None;
            match result {
                LogTabDecodeResult::Ready { document, .. } => {
                    tab.state = LogTabState::Ready { document };
                }
                LogTabDecodeResult::Failed { message, .. } => {
                    tab.state = LogTabState::Failed { message };
                }
            }
        }

        if self.log.active_tab_id == Some(tab_id) {
            self.scroll_tab_bar_to_tab(tab_id);
            self.clear_search_current_file_match_count();
        }
    }

    /// 处理全局键盘快捷键。
    ///
    /// 业务意图：
    /// - 搜索是日志查看器的核心工作流，需要即使焦点停在日志正文或目录树上也能通过 `Ctrl+F` 打开。
    /// - macOS 用户通常使用 `Cmd+F`，因此在保留用户要求的 `Ctrl+F` 同时支持平台键。
    /// - 该函数返回是否消费了快捷键，调用方据此阻止事件继续传给平台菜单或底层输入控件。
    ///
    /// 边界条件：
    /// - 这里不处理普通字符输入，避免全局监听截获搜索框或未来编辑控件的文本输入。
    /// - Enter 仅在搜索对话框打开时触发搜索，Esc 仅关闭搜索对话框。
    fn handle_global_keystroke(
        &mut self,
        keystroke: Keystroke,
        window: &mut Window,
        context: &mut Context<Self>,
        allow_keyboard_scroll: bool,
    ) -> bool {
        let editable_text_input_focused = self.editable_text_input_focused(window);
        if editable_text_input_focused
            && (Self::is_copy_keystroke(&keystroke) || Self::is_paste_keystroke(&keystroke))
        {
            return false;
        }

        if Self::is_copy_keystroke(&keystroke) && self.copy_selected_log_text(context) {
            return true;
        }

        if Self::is_paste_keystroke(&keystroke)
            && self.paste_clipboard_text_into_search_dialog(window, context)
        {
            return true;
        }

        if self.search.search_dialog.is_some()
            && let Some(control_key) = Self::search_dialog_control_key(&keystroke)
        {
            if self.settings_text_input_focused(window) {
                return false;
            }
            match control_key {
                SearchDialogControlKey::Submit => self.start_search(context),
                SearchDialogControlKey::Close => self.close_search_dialog(window, context),
            }
            return true;
        }

        if Self::marker_jump_keystroke_for_focus(&keystroke, editable_text_input_focused)
            && self.jump_to_next_marked_line(context)
        {
            return true;
        }

        if allow_keyboard_scroll
            && let Some(command) =
                Self::keyboard_scroll_command_for_focus(&keystroke, editable_text_input_focused)
        {
            return self.handle_keyboard_scroll(command, context);
        }

        false
    }

    /// 处理主窗口根节点收到的键盘事件。
    ///
    /// 业务意图：
    /// - macOS 的 `Cmd+C` 可能走应用级 key equivalent，Windows 或部分焦点状态下的 `Ctrl+C` 则更可能走普通
    ///   `on_key_down`；日志正文是自绘只读列表，不能依赖系统文本控件自动复制。
    /// - 根节点复用全局快捷键处理逻辑，作为 `intercept_keystrokes` 的兜底入口，保证日志正文选区复制在不同平台路径下都有效。
    fn handle_root_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.handle_global_keystroke(event.keystroke.clone(), window, context, true) {
            context.stop_propagation();
            context.notify();
        }
    }

    /// 判断当前焦点是否位于搜索窗口的任一文本输入框。
    ///
    /// 业务意图：
    /// - GPUI 的全局快捷键拦截可能早于输入框 `on_key_down` 触发；当关键字或目录输入框聚焦时，
    ///   `Ctrl+C` / `Ctrl+V` 必须优先交给输入框自身处理，不能被日志查看器的复制/粘贴搜索逻辑抢走。
    ///
    /// 边界条件：
    /// - 搜索窗口未打开时两个焦点句柄都不会命中，此时全局复制仍可服务日志正文选区。
    fn search_text_input_focused(&self, window: &Window) -> bool {
        self.search.search_input_focus.is_focused(window)
            || self.search.search_directory_focus.is_focused(window)
    }

    /// 判断焦点是否位于设置窗口内的自绘文本输入框。
    ///
    /// 业务意图：
    /// - 搜索窗口打开时，全局 `Enter`/`Escape` 仍会生效；如果用户正在设置页编辑线程过滤或快搜关键字，
    ///   这些按键必须优先交给设置输入框，不能误触发搜索或关闭搜索窗口。
    fn settings_text_input_focused(&self, window: &Window) -> bool {
        self.settings
            .thread_analysis_filter_focus
            .is_focused(window)
            || self.settings.quick_search_keywords_focus.is_focused(window)
            || self.active_model_config_input_kind(window).is_some()
    }

    /// 判断当前焦点是否位于应用内自绘的可编辑文本输入框。
    ///
    /// 业务意图：
    /// - GPUI 的应用级快捷键拦截会早于部分元素级 `on_key_down`，因此所有可编辑文本框聚焦时都要先放行
    ///   `Ctrl+C` / `Ctrl+V`，让对应输入框完成复制和粘贴。
    /// - 线程日志分析过滤框位于设置窗口，但状态保存在 `MainView`，这里统一判断焦点，避免粘贴堆栈时误触发
    ///   “粘贴到搜索框并打开搜索窗口”的只读日志兜底行为。
    fn editable_text_input_focused(&self, window: &Window) -> bool {
        self.search_text_input_focused(window)
            || self.settings_text_input_focused(window)
            || self.ai_chat_input_focus.is_focused(window)
    }

    /// 延迟打开搜索对话框。
    ///
    /// 业务意图：
    /// - macOS 会把 `Cmd+F`、`Ctrl+F` 这类快捷键先作为 key equivalent 分发；如果在该原生回调栈里直接创建窗口，
    ///   GPUI 或平台窗口系统内部一旦 panic，就会跨 `extern "C"` 边界触发不可恢复 abort。
    /// - 这里使用 `Window::defer` 而不是 `Context::defer_in`：前者回调拿到的是 `App`，不会自动重新租借
    ///   `MainView`；后者会在闭包外包一层 `MainView::update`，导致搜索窗口读取 `MainView` 时发生重复借用。
    ///
    /// 边界条件：
    /// - 延迟执行仍读取当前日志选区，因此用户按下快捷键时已有的选中文本会正常预填到搜索框。
    /// - 同一帧内重复触发只保留一个打开请求，避免按键重放创建多个搜索窗口。
    fn schedule_open_search_dialog(&mut self, window: &mut Window, context: &mut Context<Self>) {
        if self.search.search_dialog_open_pending {
            return;
        }

        self.search.search_dialog_open_pending = true;
        let main_view = context.entity();
        window.defer(context, move |window, app| {
            Self::open_search_dialog_after_main_update(main_view, window, app);
        });
        context.notify();
    }

    /// 判断是否为复制日志选中文本的快捷键。
    ///
    /// 业务意图：
    /// - macOS 用户习惯 `Cmd+C`，Windows 用户习惯 `Ctrl+C`，两者都应复制当前日志正文选区。
    /// - 当前只在日志正文有非空选择时消费该快捷键；没有选择时保持后续控件自己的复制行为空间。
    fn is_copy_keystroke(keystroke: &Keystroke) -> bool {
        Self::keystroke_matches_letter_or_control_code(keystroke, "c", CONTROL_C_CODE)
            && (keystroke.modifiers.control
                || keystroke.modifiers.platform
                || Self::keystroke_matches_control_code(keystroke, CONTROL_C_CODE))
    }

    /// 判断是否为粘贴快捷键。
    ///
    /// 业务意图：
    /// - 搜索输入框和日志查看器都需要识别 `Ctrl+V` / `Cmd+V`。
    /// - 日志查看器是只读区域，粘贴行为会把剪贴板文本填入搜索框，避免用户按键后没有任何可见结果。
    fn is_paste_keystroke(keystroke: &Keystroke) -> bool {
        Self::keystroke_matches_letter_or_control_code(keystroke, "v", CONTROL_V_CODE)
            && (keystroke.modifiers.control
                || keystroke.modifiers.platform
                || Self::keystroke_matches_control_code(keystroke, CONTROL_V_CODE))
    }

    /// 判断是否为剪切快捷键。
    ///
    /// 业务意图：
    /// - 设置页线程过滤输入区是可编辑文本，必须支持常见 `Ctrl+X` / `Cmd+X` 剪切行为。
    fn is_cut_keystroke(keystroke: &Keystroke) -> bool {
        Self::keystroke_matches_letter_or_control_code(keystroke, "x", CONTROL_X_CODE)
            && (keystroke.modifiers.control
                || keystroke.modifiers.platform
                || Self::keystroke_matches_control_code(keystroke, CONTROL_X_CODE))
    }

    /// 判断按键是否是搜索窗口级控制键。
    ///
    /// 边界条件：
    /// - 这里只识别无文本意义的控制键，不处理 `Ctrl+F` 等打开窗口快捷键，也不处理普通字符输入。
    fn search_dialog_control_key(keystroke: &Keystroke) -> Option<SearchDialogControlKey> {
        match keystroke.key.as_str() {
            "enter" => Some(SearchDialogControlKey::Submit),
            "escape" => Some(SearchDialogControlKey::Close),
            _ => None,
        }
    }

    /// 在鼠标进入或点击滚动区域时记录键盘滚动目标。
    ///
    /// 业务意图：
    /// - 用户可能先把鼠标移到搜索结果或目录树，再按 `PageDown`；此时滚动应落在鼠标关注的区域。
    /// - 该方法只更新内部路由状态，不触发重绘，避免高频 `mouse_move` 导致无意义刷新。
    fn note_keyboard_scroll_region(&mut self, region: KeyboardScrollRegion) {
        self.navigation.keyboard_scroll_region = Some(region);
    }

    /// 返回当前键盘滚动目标，未记录时默认使用日志正文。
    ///
    /// 边界条件：
    /// - 启动后还没有鼠标进入任何滚动区时，默认日志正文可以保持日志查看器最常见的使用路径。
    fn keyboard_scroll_region_or_default(
        region: Option<KeyboardScrollRegion>,
    ) -> KeyboardScrollRegion {
        region.unwrap_or(KeyboardScrollRegion::LogContent)
    }

    /// 根据焦点状态解析键盘滚动命令。
    ///
    /// 业务意图：
    /// - 可编辑文本框聚焦时，`PageUp`、`PageDown`、`Ctrl+Home` 和 `Ctrl+End` 属于编辑控件自己的导航行为，
    ///   主窗口不能拦截，否则会破坏搜索框、设置文本框和平台输入法体验。
    fn keyboard_scroll_command_for_focus(
        keystroke: &Keystroke,
        editable_text_input_focused: bool,
    ) -> Option<KeyboardScrollCommand> {
        if editable_text_input_focused {
            return None;
        }

        Self::keyboard_scroll_command(keystroke)
    }

    /// 解析主窗口支持的键盘滚动快捷键。
    ///
    /// 业务意图：
    /// - `PageUp` 和 `PageDown` 使用裸按键，符合日志查看器中的滚动习惯。
    /// - 顶部和底部跳转只接受用户明确要求的 `Ctrl+Home` / `Ctrl+End`，不识别 `Ctrl+Top`。
    ///
    /// 跨平台约束：
    /// - GPUI 在不同系统或键盘路径下可能把按键名写成 `pageup`、`page_up` 或包含大小写差异，因此先做轻量规范化。
    /// - `Home` / `End` 只要求 Control 修饰键，不把 macOS Command 键混入，避免和平台级文本导航语义冲突。
    fn keyboard_scroll_command(keystroke: &Keystroke) -> Option<KeyboardScrollCommand> {
        let key = Self::normalized_keyboard_key(&keystroke.key);
        let key_char = keystroke
            .key_char
            .as_deref()
            .map(Self::normalized_keyboard_key);
        let matches_key = |expected: &str| {
            key == expected || key_char.as_deref().is_some_and(|value| value == expected)
        };

        if !keystroke.modifiers.control && !keystroke.modifiers.platform && !keystroke.modifiers.alt
        {
            if matches_key("pageup") {
                return Some(KeyboardScrollCommand::PageUp);
            }
            if matches_key("pagedown") {
                return Some(KeyboardScrollCommand::PageDown);
            }
        }

        if keystroke.modifiers.control && !keystroke.modifiers.platform && !keystroke.modifiers.alt
        {
            if matches_key("home") {
                return Some(KeyboardScrollCommand::Top);
            }
            if matches_key("end") {
                return Some(KeyboardScrollCommand::Bottom);
            }
        }

        None
    }

    /// 根据焦点状态判断是否允许处理日志标记跳转快捷键。
    ///
    /// 业务意图：
    /// - `F2` 是日志正文的标记跳转入口，但用户在搜索框或设置输入框内编辑时，功能键应优先留给输入控件和系统。
    /// - 这里仅做按键解析，不检查是否真的存在标记；没有标记时由执行函数返回不消费，避免阻断其它默认行为。
    fn marker_jump_keystroke_for_focus(
        keystroke: &Keystroke,
        editable_text_input_focused: bool,
    ) -> bool {
        !editable_text_input_focused && Self::marker_jump_keystroke(keystroke)
    }

    /// 判断是否为普通 `F2` 标记跳转快捷键。
    ///
    /// 边界条件：
    /// - 本次需求只支持向后循环跳转，因此 `Shift+F2`、`Ctrl+F2`、`Cmd+F2` 等组合键都不被识别。
    /// - GPUI 可能把功能键名称放在 `key` 或 `key_char` 中，解析时同时兼容两种来源。
    fn marker_jump_keystroke(keystroke: &Keystroke) -> bool {
        if keystroke.modifiers.control
            || keystroke.modifiers.platform
            || keystroke.modifiers.alt
            || keystroke.modifiers.shift
        {
            return false;
        }

        let key = Self::normalized_keyboard_key(&keystroke.key);
        let key_char = keystroke
            .key_char
            .as_deref()
            .map(Self::normalized_keyboard_key);
        key == "f2" || key_char.as_deref().is_some_and(|value| value == "f2")
    }

    /// 切换指定行的标记状态。
    ///
    /// 业务意图：
    /// - 行号点击需要在添加和取消之间快速切换；返回值告诉调用方点击后该行是否仍处于标记状态。
    /// - 独立成纯状态函数后，UI 点击和单元测试可以复用同一套规则，避免行号交互和快捷键跳转使用不同语义。
    fn toggle_marked_line(marked_lines: &mut BTreeSet<usize>, line_index: usize) -> bool {
        if marked_lines.insert(line_index) {
            true
        } else {
            marked_lines.remove(&line_index);
            false
        }
    }

    /// 执行当前活动 tab 的下一处标记跳转。
    ///
    /// 业务意图：
    /// - `F2` 应围绕当前活动日志 tab 工作，避免用户在多 tab 场景中跳到后台文件。
    /// - 跳转复用搜索结果定位的滚动与临时高亮能力，让标记目标在密集日志中有一致的视觉提示。
    ///
    /// 边界条件：
    /// - 加载中、读取失败或当前 tab 没有任何标记时不消费按键。
    /// - 如果用户手动滚动离开上次跳转目标，下一次 `F2` 会从新的可视顶部重新寻找最近后续标记。
    fn jump_to_next_marked_line(&mut self, context: &mut Context<Self>) -> bool {
        let Some(active_tab_id) = self.log.active_tab_id else {
            return false;
        };
        let Some(tab_index) = self
            .log
            .open_tabs
            .iter()
            .position(|tab| tab.id == active_tab_id)
        else {
            return false;
        };

        let Some((target_line, tab_id)) = self.log.open_tabs.get(tab_index).and_then(|tab| {
            let (visible_start, visible_end) = Self::marker_visible_line_range(tab)?;
            let target_line = Self::next_marked_line(
                &tab.marked_lines,
                visible_start,
                visible_end,
                tab.last_marker_jump_line,
            )?;
            Some((target_line, tab.id))
        }) else {
            return false;
        };

        if let Some(tab) = self.log.open_tabs.get_mut(tab_index) {
            tab.last_marker_jump_line = Some(target_line);
            tab.highlighted_search_line = Some(target_line);
        }
        self.log.log_viewer_context_menu = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.search.search_results_context_menu = None;
        self.scroll_log_tab_to_line(tab_id, target_line);
        context.notify();
        true
    }

    /// 计算指定 tab 当前日志正文的可视行号范围。
    ///
    /// 业务意图：
    /// - 标记跳转需要判断“上次跳转的标记是否还在当前视口内”，否则用户滚动后继续按 `F2` 会从旧位置跳转，违背当前阅读上下文。
    /// - 内存日志读取 GPUI 虚拟列表滚动句柄，分页日志读取应用侧 `PagedLogScrollState`，保持两种渲染路径的行为一致。
    fn marker_visible_line_range(tab: &OpenLogTab) -> Option<(usize, usize)> {
        match &tab.state {
            LogTabState::Ready {
                document: LogTabDocument::InMemory(document),
            } => {
                let viewport_height =
                    Self::uniform_list_vertical_viewport_height(&tab.scroll_handle)
                        .unwrap_or(px(LOG_VIEWER_ROW_HEIGHT * 24.0));
                let scroll_top = {
                    let state = tab.scroll_handle.0.borrow();
                    f64::from((-state.base_handle.offset().y).max(px(0.0)))
                };
                Self::marker_visible_range_from_scroll(
                    document.lines.len(),
                    scroll_top,
                    viewport_height,
                )
            }
            LogTabState::Ready {
                document: LogTabDocument::Paged(document),
            } => {
                let viewport_height = tab.paged_viewport_handle.bounds().size.height;
                let viewport_height = if viewport_height > px(0.0) {
                    viewport_height
                } else {
                    px(LOG_VIEWER_ROW_HEIGHT * 24.0)
                };
                let (visible_start, _) =
                    Self::paged_log_visible_start(tab.paged_scroll.top_px, document.line_count());
                Self::marker_visible_range_from_first_line(
                    document.line_count(),
                    visible_start,
                    viewport_height,
                )
            }
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => None,
        }
    }

    /// 根据滚动像素计算标记跳转使用的可视行号范围。
    ///
    /// 边界条件：
    /// - 首帧尚未完成测量时调用方会传入 fallback 高度；这里仍处理空文档和异常负滚动值，避免快捷键路径 panic。
    fn marker_visible_range_from_scroll(
        line_count: usize,
        scroll_top: f64,
        viewport_height: Pixels,
    ) -> Option<(usize, usize)> {
        if line_count == 0 {
            return None;
        }

        let row_height = f64::from(px(LOG_VIEWER_ROW_HEIGHT)).max(1.0);
        let first_line = (scroll_top.max(0.0) / row_height).floor() as usize;
        Self::marker_visible_range_from_first_line(line_count, first_line, viewport_height)
    }

    /// 根据首个可见行和视口高度计算标记跳转使用的可视行号范围。
    ///
    /// 业务意图：
    /// - 范围末尾多包含一行缓冲，覆盖半行滚动或分页渲染中的小数偏移，避免刚好露出一点的标记被误判为不可见。
    fn marker_visible_range_from_first_line(
        line_count: usize,
        first_line: usize,
        viewport_height: Pixels,
    ) -> Option<(usize, usize)> {
        if line_count == 0 {
            return None;
        }

        let row_height = f64::from(px(LOG_VIEWER_ROW_HEIGHT)).max(1.0);
        let viewport_height = f64::from(viewport_height).max(row_height);
        let visible_rows = (viewport_height / row_height).ceil().max(1.0) as usize + 1;
        let visible_start = first_line.min(line_count.saturating_sub(1));
        let visible_end = visible_start
            .saturating_add(visible_rows.saturating_sub(1))
            .min(line_count.saturating_sub(1));
        Some((visible_start, visible_end))
    }

    /// 选择下一处应跳转的标记行。
    ///
    /// 业务意图：
    /// - 如果上次 `F2` 目标仍在视口内，就从它后一行继续找；否则从当前可视顶部重新找最近后续标记。
    /// - 使用 `BTreeSet::range` 直接查找有序集合中的后续标记，末尾没有结果时回到最小标记行。
    fn next_marked_line(
        marked_lines: &BTreeSet<usize>,
        visible_start: usize,
        visible_end: usize,
        last_marker_jump_line: Option<usize>,
    ) -> Option<usize> {
        let start_line = match last_marker_jump_line {
            Some(line) if (visible_start..=visible_end).contains(&line) => line.saturating_add(1),
            _ => visible_start,
        };

        marked_lines
            .range(start_line..)
            .next()
            .copied()
            .or_else(|| marked_lines.iter().next().copied())
    }

    /// 规范化 GPUI 按键名称。
    ///
    /// 业务意图：
    /// - 键盘滚动只关心少数功能键，去掉空格、下划线和短横线可以兼容 `PageUp` / `page_up` / `page-up` 等常见写法。
    fn normalized_keyboard_key(key: &str) -> String {
        key.chars()
            .filter(|character| {
                !character.is_ascii_whitespace() && *character != '_' && *character != '-'
            })
            .flat_map(|character| character.to_lowercase())
            .collect()
    }

    /// 执行当前目标区域的键盘滚动命令。
    ///
    /// 业务意图：
    /// - 全局按键处理只负责命令分发，具体滚动仍写入各区域已有滚动句柄，避免为键盘路径维护第二套状态。
    /// - 只有目标区域真实可滚动时才消费事件；例如搜索结果未打开或目录树内容不足时，按键继续交给默认路径。
    fn handle_keyboard_scroll(
        &mut self,
        command: KeyboardScrollCommand,
        context: &mut Context<Self>,
    ) -> bool {
        let handled =
            match Self::keyboard_scroll_region_or_default(self.navigation.keyboard_scroll_region) {
                KeyboardScrollRegion::LogContent => self.scroll_log_content_by_keyboard(command),
                KeyboardScrollRegion::SearchResults => {
                    self.scroll_search_results_by_keyboard(command)
                }
                KeyboardScrollRegion::LogTree => self.scroll_log_tree_by_keyboard(command),
            };

        if handled {
            context.notify();
        }

        handled
    }

    /// 按键盘命令滚动当前日志正文。
    ///
    /// 业务意图：
    /// - 内存日志继续使用 `UniformListScrollHandle`，分页日志写入 `PagedLogScrollState.top_px`。
    /// - 两种模式共享同一套页距计算，保证用户切换大文件分页路径后快捷键手感一致。
    fn scroll_log_content_by_keyboard(&mut self, command: KeyboardScrollCommand) -> bool {
        let Some(active_tab_id) = self.log.active_tab_id else {
            return false;
        };
        let Some(tab_index) = self
            .log
            .open_tabs
            .iter()
            .position(|tab| tab.id == active_tab_id)
        else {
            return false;
        };

        match &self.log.open_tabs[tab_index].state {
            LogTabState::Ready {
                document: LogTabDocument::InMemory(_),
            } => {
                let Some(metrics) = Self::log_vertical_scrollbar_metrics(
                    &self.log.open_tabs[tab_index].scroll_handle,
                ) else {
                    return false;
                };
                let Some(viewport_height) = Self::uniform_list_vertical_viewport_height(
                    &self.log.open_tabs[tab_index].scroll_handle,
                ) else {
                    return false;
                };
                Self::scroll_uniform_list_vertically_by_keyboard(
                    &self.log.open_tabs[tab_index].scroll_handle,
                    metrics.max_scroll,
                    viewport_height,
                    LOG_VIEWER_ROW_HEIGHT,
                    command,
                )
            }
            LogTabState::Ready {
                document: LogTabDocument::Paged(document),
            } => {
                let viewport_height = self.log.open_tabs[tab_index]
                    .paged_viewport_handle
                    .bounds()
                    .size
                    .height;
                if viewport_height <= px(0.0) {
                    return false;
                }

                let max_scroll =
                    Self::paged_log_vertical_max_scroll_px(document.line_count(), viewport_height);
                let Some(next_top) = Self::keyboard_scroll_position_px(
                    self.log.open_tabs[tab_index].paged_scroll.top_px,
                    max_scroll,
                    viewport_height,
                    LOG_VIEWER_ROW_HEIGHT,
                    command,
                ) else {
                    return false;
                };

                self.log.open_tabs[tab_index].paged_scroll.top_px = next_top;
                true
            }
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => false,
        }
    }

    /// 按键盘命令滚动搜索结果面板。
    ///
    /// 边界条件：
    /// - 面板关闭、结果不足一屏或首帧尚未完成滚动测量时不消费快捷键，避免用户按键后没有任何可见反馈。
    fn scroll_search_results_by_keyboard(&mut self, command: KeyboardScrollCommand) -> bool {
        let Some(panel) = self.search.search_results_panel.as_ref() else {
            return false;
        };
        let Some(metrics) = Self::search_results_scrollbar_metrics(&panel.scroll_handle) else {
            return false;
        };
        let Some(viewport_height) =
            Self::uniform_list_vertical_viewport_height(&panel.scroll_handle)
        else {
            return false;
        };

        Self::scroll_uniform_list_vertically_by_keyboard(
            &panel.scroll_handle,
            metrics.max_scroll,
            viewport_height,
            SEARCH_RESULT_ROW_HEIGHT,
            command,
        )
    }

    /// 按键盘命令滚动左侧目录树。
    ///
    /// 边界条件：
    /// - 只有真实内容高度超过视口时才滚动；临时 fallback 滚动条的 `max_scroll` 为 0，不会误消费快捷键。
    fn scroll_log_tree_by_keyboard(&mut self, command: KeyboardScrollCommand) -> bool {
        let Some(metrics) = Self::log_tree_scrollbar_metrics(&self.log.log_tree_scroll_handle)
        else {
            return false;
        };
        let Some(viewport_height) =
            Self::uniform_list_vertical_viewport_height(&self.log.log_tree_scroll_handle)
        else {
            return false;
        };

        Self::scroll_uniform_list_vertically_by_keyboard(
            &self.log.log_tree_scroll_handle,
            metrics.max_scroll,
            viewport_height,
            LOG_TREE_ROW_HEIGHT,
            command,
        )
    }

    /// 使用现有虚拟列表句柄执行一次纵向键盘滚动。
    ///
    /// 业务意图：
    /// - 日志正文、搜索结果和目录树都使用 `UniformListScrollHandle`，统一写入底层 `ScrollHandle` 可以保证滚轮、
    ///   自绘滚动条和键盘滚动共享同一个偏移来源。
    fn scroll_uniform_list_vertically_by_keyboard(
        scroll_handle: &UniformListScrollHandle,
        max_scroll: Pixels,
        viewport_height: Pixels,
        row_height: f32,
        command: KeyboardScrollCommand,
    ) -> bool {
        let current_top = Self::uniform_list_vertical_scroll_top(scroll_handle, max_scroll);
        let Some(next_top) = Self::keyboard_scroll_position(
            current_top,
            max_scroll,
            viewport_height,
            row_height,
            command,
        ) else {
            return false;
        };

        Self::set_uniform_list_vertical_scroll_top(scroll_handle, next_top);
        true
    }

    /// 读取虚拟列表当前纵向滚动位置。
    ///
    /// 边界条件：
    /// - GPUI 底层偏移使用负数表示向下滚动，这里统一转换成业务侧非负 `scroll_top` 并夹在合法范围内。
    fn uniform_list_vertical_scroll_top(
        scroll_handle: &UniformListScrollHandle,
        max_scroll: Pixels,
    ) -> Pixels {
        let state = scroll_handle.0.borrow();
        (-state.base_handle.offset().y).clamp(px(0.0), max_scroll)
    }

    /// 读取虚拟列表可见视口高度。
    ///
    /// 业务意图：
    /// - `PageUp` / `PageDown` 的滚动距离要按真实视口高度计算，不能写死固定行数，否则用户调整窗口或面板高度后手感会失真。
    fn uniform_list_vertical_viewport_height(
        scroll_handle: &UniformListScrollHandle,
    ) -> Option<Pixels> {
        let state = scroll_handle.0.borrow();
        let bounds_height = state.base_handle.bounds().size.height;
        if bounds_height > px(0.0) {
            return Some(bounds_height);
        }

        state
            .last_item_size
            .map(|size| size.item.height)
            .filter(|height| *height > px(0.0))
    }

    /// 写入虚拟列表纵向滚动位置。
    ///
    /// 边界条件：
    /// - 设置 Y 偏移时保留当前 X 偏移，避免用户横向滚动长日志后按 PageDown 导致横向位置被重置。
    fn set_uniform_list_vertical_scroll_top(
        scroll_handle: &UniformListScrollHandle,
        scroll_top: Pixels,
    ) {
        let base_scroll_handle = {
            // 先克隆底层句柄再释放 `RefCell` 借用，避免 `set_offset` 内部需要可变借用时产生嵌套借用。
            scroll_handle.0.borrow().base_handle.clone()
        };
        let current_offset = base_scroll_handle.offset();
        base_scroll_handle.set_offset(point(current_offset.x, -scroll_top));
    }

    /// 计算一次键盘滚动后的 `Pixels` 位置。
    ///
    /// 业务意图：
    /// - 页滚动距离按“视口高度减一行”计算，让用户翻页时保留一行上下文；视口很小时至少滚动一行。
    /// - 顶部、底部和翻页结果都统一 clamp，避免滚动条越界或出现负偏移。
    fn keyboard_scroll_position(
        current_top: Pixels,
        max_scroll: Pixels,
        viewport_height: Pixels,
        row_height: f32,
        command: KeyboardScrollCommand,
    ) -> Option<Pixels> {
        if max_scroll <= px(0.0) {
            return None;
        }

        let current_top = current_top.clamp(px(0.0), max_scroll);
        let page_delta = Self::keyboard_scroll_page_delta(viewport_height, row_height);
        let next_top = match command {
            KeyboardScrollCommand::PageUp => current_top - page_delta,
            KeyboardScrollCommand::PageDown => current_top + page_delta,
            KeyboardScrollCommand::Top => px(0.0),
            KeyboardScrollCommand::Bottom => max_scroll,
        };

        Some(next_top.clamp(px(0.0), max_scroll))
    }

    /// 计算分页日志使用的 `f64` 逻辑滚动位置。
    ///
    /// 业务意图：
    /// - 分页日志可能非常大，滚动位置保存在 `f64` 中避免深位置 `f32` 精度不足；这里只把页距从 `Pixels` 转成 `f64`。
    fn keyboard_scroll_position_px(
        current_top: f64,
        max_scroll: f64,
        viewport_height: Pixels,
        row_height: f32,
        command: KeyboardScrollCommand,
    ) -> Option<f64> {
        if max_scroll <= 0.0 {
            return None;
        }

        let current_top = current_top.clamp(0.0, max_scroll);
        let page_delta = f64::from(Self::keyboard_scroll_page_delta(
            viewport_height,
            row_height,
        ));
        let next_top = match command {
            KeyboardScrollCommand::PageUp => current_top - page_delta,
            KeyboardScrollCommand::PageDown => current_top + page_delta,
            KeyboardScrollCommand::Top => 0.0,
            KeyboardScrollCommand::Bottom => max_scroll,
        };

        Some(next_top.clamp(0.0, max_scroll))
    }

    /// 计算 PageUp/PageDown 的页距。
    ///
    /// 边界条件：
    /// - 如果视口高度尚小于一行或测量异常，仍至少移动一行，避免快捷键看起来失效。
    fn keyboard_scroll_page_delta(viewport_height: Pixels, row_height: f32) -> Pixels {
        let row_height = px(row_height.max(1.0));
        if viewport_height > row_height {
            viewport_height - row_height
        } else {
            row_height
        }
    }

    /// 判断按键是否匹配指定字母或该字母的 ASCII 控制字符。
    ///
    /// 业务意图：
    /// - GPUI 的 `Keystroke::key` 和 `key_char` 在不同平台输入路径下可能保存不同形态：
    ///   普通 `Cmd+F` 通常表现为 `key = "f"`，而 `Ctrl+F` 可能表现为 `"\u{6}"`。
    /// - 把匹配逻辑集中在这里，可以让搜索和复制快捷键都兼容真实键盘、AppleScript 自动化和不同键盘布局。
    ///
    /// 边界条件：
    /// - 该函数只用于带控制键语义的快捷键，不参与普通文本输入，避免把不可见控制字符误写入搜索框。
    fn keystroke_matches_letter_or_control_code(
        keystroke: &Keystroke,
        letter: &str,
        control_code: &str,
    ) -> bool {
        keystroke.key.eq_ignore_ascii_case(letter)
            || Self::keystroke_matches_control_code(keystroke, control_code)
            || keystroke
                .key_char
                .as_deref()
                .is_some_and(|key_char| key_char.eq_ignore_ascii_case(letter))
    }

    /// 判断按键是否是指定 ASCII 控制字符。
    fn keystroke_matches_control_code(keystroke: &Keystroke, control_code: &str) -> bool {
        keystroke.key == control_code || keystroke.key_char.as_deref() == Some(control_code)
    }

    /// 复制当前激活日志 tab 的选中文本。
    ///
    /// 业务意图：
    /// - 日志正文是只读查看器，复制操作应从已解码的真实文本行中提取，而不是从屏幕像素或渲染元素反推。
    /// - 只有存在非空选择时才写入剪贴板，避免用户在搜索框等其它控件内按复制键时被日志查看器误拦截。
    ///
    /// 边界条件：
    /// - 复制文本保留跨行选择中的换行符，满足从日志中截取堆栈片段或多行上下文的常见需求。
    /// - 选择范围使用字符列，截取前会转换为 UTF-8 字节边界，中文不会被截断为非法字符串。
    fn copy_selected_log_text(&self, context: &mut Context<Self>) -> bool {
        let Some(active_tab_id) = self.log.active_tab_id else {
            return false;
        };
        self.copy_selected_log_text_for_tab(active_tab_id, context)
    }

    /// 复制指定 tab 的日志正文选区。
    ///
    /// 业务意图：
    /// - 右键菜单打开时会绑定具体 tab，复制时不应受后续焦点或激活状态变化影响。
    fn copy_selected_log_text_for_tab(&self, tab_id: usize, context: &mut Context<Self>) -> bool {
        let Some(text) = self.selected_log_text_for_tab(tab_id) else {
            return false;
        };
        if text.is_empty() {
            return false;
        }

        context.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// 取得当前激活日志 tab 的选中文本。
    fn selected_log_text(&self) -> Option<String> {
        let active_tab_id = self.log.active_tab_id?;
        self.selected_log_text_for_tab(active_tab_id)
    }

    /// 取得指定日志 tab 的选中文本。
    ///
    /// 边界条件：
    /// - tab 不存在、尚未加载成功或选区为空时返回 `None`，用于禁用右键菜单“复制”。
    fn selected_log_text_for_tab(&self, tab_id: usize) -> Option<String> {
        let tab = self.log.open_tabs.iter().find(|tab| tab.id == tab_id)?;
        let selection = tab.text_selection.as_ref()?;
        if selection.is_empty() {
            return None;
        }
        let LogTabState::Ready { document } = &tab.state else {
            return None;
        };

        let (start, end) = selection.normalized();
        let line_count = document.line_count();
        if line_count == 0 || start.line_index >= line_count {
            return None;
        }
        let end_line_index = end.line_index.min(line_count.saturating_sub(1));
        if start.line_index > end_line_index {
            return None;
        }

        let mut selected_text = String::new();
        for line_index in start.line_index..=end_line_index {
            let Some(line) = Self::log_document_line_text(document, line_index) else {
                continue;
            };
            if line_index > start.line_index {
                selected_text.push('\n');
            }
            let Some((start_column, end_column)) =
                Self::selection_columns_for_line(selection, line_index, &line)
            else {
                continue;
            };
            let start_byte = Self::byte_index_for_char_column(&line, start_column);
            let end_byte = Self::byte_index_for_char_column(&line, end_column);
            if start_byte < end_byte {
                selected_text.push_str(&line[start_byte..end_byte]);
            }
        }

        Some(selected_text)
    }

    /// 取得可填入搜索框的日志选中文本。
    ///
    /// 业务意图：
    /// - 当前搜索引擎是逐行普通文本搜索，搜索框也是单行输入；当用户选中多行日志时，直接填入换行会让搜索条件不可用。
    /// - 因此这里取第一段非空行作为搜索关键字，复制功能仍保留完整多行内容。
    fn selected_log_text_for_search_query(&self) -> Option<String> {
        self.selected_log_text()
            .and_then(|text| {
                text.lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(ToOwned::to_owned)
            })
            .filter(|query| !query.is_empty())
    }

    /// 把剪贴板文本粘贴到搜索关键字输入框。
    ///
    /// 业务意图：
    /// - 日志正文是只读查看器，`Ctrl+V` 不能修改日志内容；用户通常是想把剪贴板里的关键字拿来搜索。
    /// - 如果搜索窗口尚未打开，则先创建搜索状态并排队打开窗口；如果已经打开，则替换当前关键字选区。
    ///
    /// 边界条件：
    /// - 非文本剪贴板、空文本或只有换行的文本不处理，避免清空用户当前搜索条件。
    /// - 粘贴文本按搜索框单行规则清理换行，和平台输入法提交路径保持一致。
    fn paste_clipboard_text_into_search_dialog(
        &mut self,
        window: &mut Window,
        context: &mut Context<Self>,
    ) -> bool {
        let Some(text) = context
            .read_from_clipboard()
            .and_then(|item| item.text())
            .map(|text| Self::sanitize_search_input_text(&text))
            .filter(|text| !text.trim().is_empty())
        else {
            return false;
        };

        self.prepare_search_dialog_state();
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            Self::replace_search_query_with_clipboard_text(dialog, text);
        }
        self.schedule_open_search_dialog(window, context);
        self.touch_search_text_cursor_activity();
        context.notify();
        true
    }

    /// 按统一文档模型读取指定行文本。
    ///
    /// 业务意图：
    /// - 小文件行文本来自内存 `Vec<String>`，超大文件行文本来自分页 seek 读取；复制、渲染和命中定位需要共享同一入口。
    /// - 分页读取失败时返回 `None`，避免复制或临时渲染路径因单行 I/O 错误直接崩溃；真正的打开错误仍在后台任务阶段展示。
    fn log_document_line_text(document: &LogTabDocument, line_index: usize) -> Option<String> {
        match document {
            LogTabDocument::InMemory(document) => document.lines.get(line_index).cloned(),
            LogTabDocument::Paged(document) => document
                .read_line(line_index)
                .ok()
                .flatten()
                .map(|line| line.text),
        }
    }

    /// 返回某一行被当前选择覆盖的字符列范围。
    ///
    /// 业务意图：
    /// - 渲染选区和复制文本都需要同一套范围规则，避免屏幕高亮和复制结果不一致。
    /// - 这里输出字符列而不是字节范围，由调用方按具体字符串转换为 UTF-8 安全字节边界。
    fn selection_columns_for_line(
        selection: &LogTextSelection,
        line_index: usize,
        line: &str,
    ) -> Option<(usize, usize)> {
        let (start, end) = selection.normalized();
        if line_index < start.line_index || line_index > end.line_index {
            return None;
        }

        let line_char_count = line.chars().count();
        let start_column = if line_index == start.line_index {
            start.column.min(line_char_count)
        } else {
            0
        };
        let end_column = if line_index == end.line_index {
            end.column.min(line_char_count)
        } else {
            line_char_count
        };

        Some((start_column, end_column))
    }

    /// 返回某一行被选择覆盖的 UTF-8 字节范围。
    ///
    /// 边界条件：
    /// - 空范围不参与渲染高亮，但复制跨行选择时仍会通过行间换行保留空行语义。
    fn selected_byte_range_for_line(
        selection: &LogTextSelection,
        line_index: usize,
        line: &str,
    ) -> Option<Range<usize>> {
        let (start_column, end_column) =
            Self::selection_columns_for_line(selection, line_index, line)?;
        let start_byte = Self::byte_index_for_char_column(line, start_column);
        let end_byte = Self::byte_index_for_char_column(line, end_column);
        (start_byte < end_byte).then_some(start_byte..end_byte)
    }

    /// 将字符列转换为字符串的 UTF-8 字节下标。
    ///
    /// 业务意图：
    /// - 鼠标选择以字符列表达，`String::replace_range`、切片和 `StyledText` 高亮范围都要求 UTF-8 字节边界。
    /// - 统一转换函数可以避免中文、全角字符或 emoji 出现在选区边缘时产生非法切片。
    fn byte_index_for_char_column(text: &str, column: usize) -> usize {
        if column == 0 {
            return 0;
        }
        text.char_indices()
            .nth(column)
            .map(|(index, _)| index)
            .unwrap_or(text.len())
    }

    /// 将 UTF-8 字节下标转换为字符列。
    ///
    /// 业务意图：
    /// - GPUI 文本 shaping 的命中结果以字节下标表达，而日志选区状态使用字符列保存。
    /// - 统一转换后，鼠标命中、选区高亮、复制和搜索预填可以继续共用字符列模型。
    ///
    /// 边界条件：
    /// - 正常情况下 GPUI 返回的字节下标位于字符边界；这里仍做边界回退，避免未来字体 shaping 或
    ///   组合字符场景返回中间下标时导致字符串切片 panic。
    fn char_column_for_byte_index(text: &str, byte_index: usize) -> usize {
        let byte_index = byte_index.min(text.len());
        let safe_byte_index = if text.is_char_boundary(byte_index) {
            byte_index
        } else {
            text.char_indices()
                .map(|(index, _)| index)
                .take_while(|index| *index < byte_index)
                .last()
                .unwrap_or(0)
        };

        text[..safe_byte_index].chars().count()
    }

    /// 将日志行中的制表符展开为显示用空格，并保留原始文本到显示文本的字节映射。
    ///
    /// 业务意图：
    /// - 日志字段常用 `\t` 分隔，直接渲染会导致不同平台或 GPUI 文本系统下间隔过窄，字段看起来粘连。
    /// - 展开时按固定 4 列 tab stop 计算空格数，而不是简单替换成 4 个空格，才能让后续列落在稳定边界。
    ///
    /// 边界条件：
    /// - 这里只处理单行文本，换行已经由日志解码阶段拆分。
    /// - UTF-8 多字节字符在 JetBrains Mono 下仍按一个字符列推进；中文全角宽度不在本次 tab 对齐规则内扩展。
    fn expanded_log_line_for_display(line: &str) -> ExpandedLogLine {
        let mut text = String::with_capacity(line.len());
        let mut original_to_display_bytes = vec![0; line.len() + 1];
        let mut display_to_original_bytes = vec![0];
        let mut display_column = 0usize;

        for (original_start, character) in line.char_indices() {
            let original_end = original_start + character.len_utf8();
            let display_start = text.len();

            if character == '\t' {
                let spaces = LOG_VIEWER_TAB_WIDTH - (display_column % LOG_VIEWER_TAB_WIDTH);
                for offset in 0..spaces {
                    text.push(' ');
                    display_to_original_bytes.push(if offset == 0 {
                        original_start
                    } else {
                        original_end
                    });
                }
                display_column += spaces;
            } else {
                text.push(character);
                for _ in display_start..text.len() {
                    display_to_original_bytes.push(original_start);
                }
                display_column += 1;
            }

            let display_end = text.len();
            for mapped_display_start in original_to_display_bytes
                .iter_mut()
                .take(original_end)
                .skip(original_start)
            {
                *mapped_display_start = display_start;
            }
            original_to_display_bytes[original_end] = display_end;
            display_to_original_bytes[display_end] = original_end;
        }

        original_to_display_bytes[line.len()] = text.len();
        display_to_original_bytes[text.len()] = line.len();

        ExpandedLogLine {
            text,
            original_to_display_bytes,
            display_to_original_bytes,
        }
    }

    /// 将原始日志字节下标换算为展开后的显示字节下标。
    ///
    /// 业务意图：
    /// - 语法高亮、搜索高亮和选区高亮都是基于原始日志文本计算的，渲染前必须同步平移到显示文本。
    fn display_byte_index_for_original_byte(
        expanded: &ExpandedLogLine,
        byte_index: usize,
    ) -> usize {
        expanded
            .original_to_display_bytes
            .get(byte_index)
            .copied()
            .unwrap_or(expanded.text.len())
    }

    /// 将显示文本字节下标换算回原始日志字节下标。
    ///
    /// 业务意图：
    /// - 鼠标命中测试发生在展开后的显示文本上，但选区状态保存原始文本字符列；这里负责把二者接回同一坐标系。
    fn original_byte_index_for_display_byte(
        expanded: &ExpandedLogLine,
        byte_index: usize,
    ) -> usize {
        expanded
            .display_to_original_bytes
            .get(byte_index)
            .copied()
            .unwrap_or_else(|| expanded.original_to_display_bytes.len().saturating_sub(1))
    }

    /// 将基于原始日志文本的高亮范围映射到展开后的显示文本。
    ///
    /// 业务意图：
    /// - tab 展开后显示文本长度变长，如果继续使用原始字节范围，搜索命中、语法高亮和选区背景都会向左错位。
    /// - 映射时保留原来的 `HighlightStyle`，只调整字节范围。
    ///
    /// 边界条件：
    /// - 空范围或越界范围会被压缩到安全边界后丢弃，避免传给 GPUI 非法高亮区间。
    fn map_log_highlights_to_display(
        highlights: Vec<(Range<usize>, gpui::HighlightStyle)>,
        expanded: &ExpandedLogLine,
    ) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
        let original_len = expanded.original_to_display_bytes.len().saturating_sub(1);
        highlights
            .into_iter()
            .filter_map(|(range, style)| {
                let start = range.start.min(original_len);
                let end = range.end.min(original_len);
                let display_start = Self::display_byte_index_for_original_byte(expanded, start);
                let display_end = Self::display_byte_index_for_original_byte(expanded, end);
                (display_start < display_end).then_some((display_start..display_end, style))
            })
            .collect()
    }

    /// 返回当前激活文件所在目录的展示标签。
    ///
    /// 业务意图：
    /// - 用户切换到“当前目录”搜索时，需要看到实际将被搜索的目录目标。
    /// - 这里仅从当前活动 tab 的 `LogFileSource` 派生展示文本，不访问磁盘，也不扩大已加载目录树的权限边界。
    fn active_search_directory_label(&self) -> Option<String> {
        let active_tab_id = self.log.active_tab_id?;
        let active_tab = self
            .log
            .open_tabs
            .iter()
            .find(|tab| tab.id == active_tab_id)?;
        Some(source_location_label(&active_tab.source))
    }

    /// 判断某个文件来源是否匹配用户编辑后的目录目标文本。
    ///
    /// 业务意图：
    /// - 当前目录搜索的基础范围仍由 `collect_current_directory_sources` 从加载树中递归收集。
    /// - 用户修改目录目标时，只在这批已授权来源内按展示路径做二次过滤，满足缩小范围或输入子目录片段的需求。
    ///
    /// 边界条件：
    /// - 目标为空时不额外过滤，保持“当前文件所在目录递归搜索”的默认行为。
    /// - 路径大小写在 Windows 上通常不敏感；这里统一使用小写包含匹配，优先保证跨平台用户输入的容错性。
    fn source_matches_directory_target(source: &LogFileSource, target: &str) -> bool {
        let target = target.trim();
        if target.is_empty() {
            return true;
        }

        source_location_label(source)
            .to_lowercase()
            .contains(&target.to_lowercase())
    }

    /// 记录一次搜索关键字。
    ///
    /// 业务意图：
    /// - 执行搜索后把关键字写入当前会话历史，让下一次没有正文选区时可以自动恢复最近关键字。
    /// - 历史不持久化到配置目录，避免日志关键字涉及业务数据或敏感信息时被长期保存。
    fn remember_search_query(&mut self, query: &str, match_mode: SearchMatchMode) {
        Self::remember_search_query_in_history(
            &mut self.search.search_query_history,
            query,
            match_mode,
        );
    }

    /// 更新搜索关键字历史集合。
    ///
    /// 边界条件：
    /// - 空白关键字不记录。
    /// - 重复的“关键字 + 匹配模式”先移除旧位置再插入首位，保证列表按最近使用排序。
    /// - 超过上限时删除最旧记录，避免会话状态无界增长。
    fn remember_search_query_in_history(
        history: &mut Vec<SearchQueryHistoryItem>,
        query: &str,
        match_mode: SearchMatchMode,
    ) {
        let Some(item) = SearchQueryHistoryItem::new(query, match_mode) else {
            return;
        };

        history.retain(|existing| {
            existing.query != item.query || existing.match_mode != item.match_mode
        });
        history.insert(0, item);
        history.truncate(SEARCH_QUERY_HISTORY_LIMIT);
    }

    /// 返回最近一次搜索关键字。
    ///
    /// 业务意图：
    /// - 打开搜索窗口时如果没有日志选区，就使用最近关键字预填，满足用户“显示上一次搜索关键字”的要求。
    fn last_search_query(&self) -> Option<SearchQueryHistoryItem> {
        self.search.search_query_history.first().cloned()
    }

    /// 把用户选择的历史关键字填入搜索对话框。
    ///
    /// 业务意图：
    /// - 历史下拉菜单只负责选择已有关键字，不立即启动搜索，用户仍可继续编辑范围、大小写和目录目标。
    /// - 选择后把光标放到关键字末尾，并清除组合文本和当前文件计数缓存，保证后续搜索使用完整新关键字。
    ///
    /// 边界条件：
    /// - 空白历史项不会由历史管理产生；这里仍做防御性忽略，避免未来调用方传入非法值时清空当前输入。
    fn apply_search_history_query(dialog: &mut SearchDialogState, item: &SearchQueryHistoryItem) {
        let query = item.query.trim();
        if query.is_empty() {
            dialog.query_history_menu_open = false;
            return;
        }

        dialog.query_input.text = query.to_string();
        dialog.match_mode = item.match_mode;
        let cursor = dialog.query_input.text.len();
        dialog.query_input.selection_range = cursor..cursor;
        dialog.query_input.marked_range = None;
        dialog.query_history_menu_open = false;
        dialog.current_file_match_count = None;
        dialog.message = "已选择历史关键字，按 Enter 或点击搜索".to_string();
    }

    /// 停止搜索对话框当前正在运行的搜索任务，并返回需要标记取消的任务 ID。
    ///
    /// 业务意图：
    /// - 搜索窗口中的“停止”按钮只中断当前后台任务，不关闭窗口，也不清空用户已经输入的关键字和范围。
    /// - 后台任务可能已经在读取或搜索大文件；这里通过任务 ID 失效和 `is_searching=false` 丢弃后续回调，
    ///   让 UI 立即恢复可编辑状态，同时保留取消前已经收集到的结果。
    ///
    /// 边界条件：
    /// - 如果当前没有运行中的任务，只关闭历史下拉菜单并返回 `None`，避免重复点击停止按钮错误标记旧记录。
    fn stop_search_dialog_task(dialog: &mut SearchDialogState) -> Option<usize> {
        dialog.query_history_menu_open = false;
        if !dialog.is_searching {
            return None;
        }

        dialog.is_searching = false;
        dialog.message = "搜索已停止，可修改条件后重新搜索".to_string();
        Some(dialog.job_id)
    }

    /// 从已加载目录树中收集匹配用户目录目标文本的来源。
    ///
    /// 业务意图：
    /// - “当前目录”搜索默认落在当前文件所在目录；当用户手动修改目标目录时，应允许定位到加载树中的其它目录或子目录片段。
    /// - 搜索仍只遍历已加载树中存在的 `LogFileSource`，不会因为用户输入路径而额外扫描磁盘或解压压缩包。
    ///
    /// 边界条件：
    /// - 同一来源可能因压缩包单文件快捷入口等原因重复出现在树中，必须按稳定键去重。
    fn collect_sources_matching_directory_target(
        tree: &LoadedLogTree,
        target: &str,
    ) -> Vec<LogFileSource> {
        let mut seen_keys = HashSet::new();
        let mut sources = Vec::new();

        for source in tree.rows.iter().filter_map(|row| row.source.as_ref()) {
            if !Self::source_matches_directory_target(source, target) {
                continue;
            }

            let key = source.stable_key();
            if seen_keys.insert(key) {
                sources.push(source.clone());
            }
        }

        sources
    }

    /// 标记指定搜索历史记录已取消。
    ///
    /// 业务意图：
    /// - 关闭搜索对话框会让后台任务回调失效，结果面板必须同步从“搜索中”切换为“已取消”。
    /// - 只修改对应任务的记录，避免影响面板中其它历史搜索。
    fn mark_search_record_canceled(&mut self, job_id: usize) {
        let Some(panel) = self.search.search_results_panel.as_mut() else {
            return;
        };
        Self::mark_search_record_canceled_in_records(&mut panel.records, job_id);
    }

    /// 在记录集合中标记指定搜索任务已取消。
    ///
    /// 业务意图：
    /// - 主窗口和单元测试都需要验证“旧搜索被新操作打断”时记录状态会从“搜索中”切换为“已取消”。
    /// - 该函数只修改数据，不触发重绘；调用方负责在 UI 上下文中 `notify`。
    fn mark_search_record_canceled_in_records(
        records: &mut [SearchHistoryRecord],
        job_id: usize,
    ) -> bool {
        if let Some(record) = records.iter_mut().find(|record| record.job_id == job_id) {
            record.canceled = true;
            return true;
        }
        false
    }

    /// 在 `MainView` 更新租借结束后打开设置窗口。
    ///
    /// 业务意图：
    /// - 工具栏设置按钮通过该入口打开独立窗口，保证重复点击只激活已有窗口而不是创建多个窗口。
    /// - 设置窗口只读写 `MainView` 中的设置状态，不访问文件系统、不请求网络，也不影响日志加载或搜索任务。
    ///
    /// 边界条件：
    /// - 如果旧窗口句柄失效，清空后重新创建。
    /// - 创建失败时仅清理 pending 状态；当前没有用户可见错误面板，避免把设置窗口失败混入日志内容区。
    fn open_settings_window_after_main_update(main_view: Entity<MainView>, app: &mut App) {
        let existing_settings_window = main_view.update(app, |view, context| {
            view.settings.settings_window_open_pending = false;
            view.log.tab_context_menu = None;
            view.log.encoding_dropdown_menu = None;
            view.search.search_results_context_menu = None;
            view.log.log_viewer_context_menu = None;
            context.notify();
            view.settings.settings_window
        });

        if let Some(settings_window) = existing_settings_window {
            if settings_window
                .update(app, |_, window, _| {
                    window.activate_window();
                })
                .is_ok()
            {
                return;
            }
            main_view.update(app, |view, context| {
                view.discard_thread_analysis_filter_edit(context);
                view.discard_quick_search_keywords_edit(context);
                view.settings.settings_window = None;
            });
        }

        let main_view_for_window = main_view.clone();
        let main_view_for_close = main_view.clone();
        let settings_window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("设置".into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::centered(
                size(px(SETTINGS_WINDOW_WIDTH), px(SETTINGS_WINDOW_HEIGHT)),
                app,
            )),
            is_resizable: false,
            is_minimizable: true,
            window_min_size: Some(size(px(SETTINGS_WINDOW_WIDTH), px(SETTINGS_WINDOW_HEIGHT))),
            ..Default::default()
        };

        match app.open_window(settings_window_options, move |window, app| {
            window.on_window_should_close(app, move |_, app| {
                main_view_for_close.update(app, |view, context| {
                    view.discard_thread_analysis_filter_edit(context);
                    view.discard_quick_search_keywords_edit(context);
                    view.settings.settings_window = None;
                    view.settings.settings_window_open_pending = false;
                    context.notify();
                });
                true
            });
            app.new(|context| SettingsWindowView::new(main_view_for_window, context))
        }) {
            Ok(settings_window) => {
                main_view.update(app, |view, context| {
                    view.settings.settings_window = Some(settings_window);
                    context.notify();
                });
            }
            Err(_error) => {
                main_view.update(app, |view, context| {
                    view.settings.settings_window = None;
                    view.settings.settings_window_open_pending = false;
                    context.notify();
                });
            }
        }
    }

    /// 在 `MainView` 更新租借结束后切换到 HPROF 页并启动解析。
    ///
    /// 业务意图：
    /// - HPROF 解析已经迁入主窗口大功能页，选择文件后应切到该页并复用内嵌分析实体。
    /// - 分析实体观察主视图主题，因此创建实体必须发生在主视图更新闭包内，并在实体内部启动后台解析。
    ///
    /// 边界条件：
    /// - 重复选择新文件时复用旧实体，旧后台任务由 `HprofAnalysisView::start_new_analysis` 通过取消标记和代次保护停止或丢弃结果。
    fn open_hprof_analysis_page_after_main_update(
        main_view: Entity<MainView>,
        file_path: PathBuf,
        app: &mut App,
    ) {
        main_view.update(app, |view, context| {
            view.navigation.active_main_feature = MainFeature::HprofAnalysis;
            view.log.tab_context_menu = None;
            view.log.encoding_dropdown_menu = None;
            view.search.search_results_context_menu = None;
            view.log.log_viewer_context_menu = None;
            view.log.log_tree_context_menu = None;
            view.log.load_source_menu = None;

            let hprof_view = if let Some(hprof_view) = view.hprof_analysis_view.clone() {
                hprof_view
            } else {
                let main_view_for_hprof = context.entity();
                let hprof_view =
                    context.new(|context| HprofAnalysisView::new(main_view_for_hprof, context));
                view.hprof_analysis_view = Some(hprof_view.clone());
                hprof_view
            };
            hprof_view.update(context, |hprof_view, context| {
                hprof_view.start_new_analysis(file_path, context);
            });
            context.notify();
        });
    }

    /// 返回 HPROF 页的内嵌分析视图，如果尚未创建则创建空态视图。
    ///
    /// 业务意图：
    /// - 用户第一次点击 HPROF 导航时应看到主窗口内的空态页，而不是打开独立窗口或立即读取磁盘。
    fn hprof_analysis_view(&mut self, context: &mut Context<Self>) -> Entity<HprofAnalysisView> {
        if let Some(hprof_view) = self.hprof_analysis_view.clone() {
            return hprof_view;
        }
        let main_view = context.entity();
        let hprof_view = context.new(|context| HprofAnalysisView::new(main_view, context));
        self.hprof_analysis_view = Some(hprof_view.clone());
        hprof_view
    }

    /// 准备搜索对话框状态。
    ///
    /// 业务意图：
    /// - 如果日志正文当前存在选区，打开或再次唤起搜索框时用选中文本预填关键字，减少复制再搜索的重复操作。
    /// - 如果没有日志选区，首次打开使用最近一次搜索关键字；已有对话框只在查询词为空时恢复最近关键字，避免覆盖用户正在编辑的输入。
    ///
    /// 实现原因：
    /// - 这里只修改 `MainView` 自身状态，不创建窗口；独立搜索窗口会在 `MainView::update` 返回后再创建。
    /// - 这样可以避免搜索窗口根视图初始化或渲染时读取 `MainView`，和当前 `MainView` 更新租借发生重叠。
    fn prepare_search_dialog_state(&mut self) {
        let selected_query = self.selected_log_text_for_search_query();
        if self.search.search_dialog.is_none() {
            let directory_target = self.active_search_directory_label().unwrap_or_default();
            let last_query = self.last_search_query();
            let (query, match_mode) = if let Some(selected_query) = selected_query.clone() {
                // 正文选区是普通文本片段，预填时默认使用普通文本模式，避免选中内容中的正则元字符改变含义。
                (selected_query, SearchMatchMode::Literal)
            } else if let Some(last_query) = last_query.clone() {
                (last_query.query, last_query.match_mode)
            } else {
                (String::new(), SearchMatchMode::Literal)
            };
            self.search.search_dialog = Some(SearchDialogState {
                query_input: SingleLineTextInputState::from_text(query),
                query_history_menu_open: false,
                scope: SearchScope::CurrentFile,
                directory_input: SingleLineTextInputState::from_text(directory_target),
                case_sensitive: false,
                match_mode,
                current_file_match_count: None,
                is_searching: false,
                progress: SearchProgress::default(),
                message: if selected_query.is_some() {
                    "已填入选中文本，按 Enter 或点击搜索".to_string()
                } else if last_query.is_some() {
                    "已填入上次搜索关键字，按 Enter 或点击搜索".to_string()
                } else {
                    "输入关键字后按 Enter 或点击搜索".to_string()
                },
                job_id: 0,
            });
        } else if let Some(selected_query) = selected_query
            && let Some(dialog) = self.search.search_dialog.as_mut()
        {
            let cursor = selected_query.len();
            dialog.query_input.text = selected_query;
            dialog.query_input.selection_range = cursor..cursor;
            dialog.query_input.marked_range = None;
            dialog.query_history_menu_open = false;
            dialog.current_file_match_count = None;
            dialog.match_mode = SearchMatchMode::Literal;
            dialog.message = "已填入选中文本，按 Enter 或点击搜索".to_string();
        } else if let Some(last_query) = self.last_search_query()
            && let Some(dialog) = self.search.search_dialog.as_mut()
            && dialog.query_input.text.trim().is_empty()
        {
            let cursor = last_query.query.len();
            dialog.query_input.text = last_query.query;
            dialog.match_mode = last_query.match_mode;
            dialog.query_input.selection_range = cursor..cursor;
            dialog.query_input.marked_range = None;
            dialog.query_history_menu_open = false;
            dialog.current_file_match_count = None;
            dialog.message = "已填入上次搜索关键字，按 Enter 或点击搜索".to_string();
        }
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
    }

    /// 在 `MainView` 更新租借结束后打开搜索对话框。
    ///
    /// 业务意图：
    /// - 工具栏按钮、`Ctrl+F` 和 `Cmd+F` 都通过该入口打开搜索窗口，避免不同入口出现不同状态规则。
    /// - 搜索窗口是独立 GPUI 窗口，它的根视图会读取并观察 `MainView`；因此窗口创建必须发生在 `MainView::update` 闭包外。
    ///
    /// 边界条件：
    /// - 如果已有搜索窗口仍有效，则只激活并聚焦输入框。
    /// - 如果旧句柄已经失效，则清空后重新创建；创建失败时把错误写回搜索对话框状态。
    fn open_search_dialog_after_main_update(
        main_view: Entity<MainView>,
        _current_window: &mut Window,
        app: &mut App,
    ) {
        let (search_input_focus, existing_search_window) =
            main_view.update(app, |view, context| {
                view.search.search_dialog_open_pending = false;
                view.prepare_search_dialog_state();
                context.notify();
                (
                    view.search.search_input_focus.clone(),
                    view.search.search_dialog_window,
                )
            });

        if let Some(search_window) = existing_search_window {
            if search_window
                .update(app, |_, window, _| {
                    window.activate_window();
                    window.focus(&search_input_focus);
                })
                .is_ok()
            {
                return;
            }
            main_view.update(app, |view, _| {
                view.search.search_dialog_window = None;
            });
        }

        let main_view_for_window = main_view.clone();
        let main_view_for_close = main_view.clone();
        let search_window_options = WindowOptions {
            titlebar: None,
            window_bounds: Some(WindowBounds::centered(
                size(px(SEARCH_DIALOG_WIDTH), px(SEARCH_DIALOG_WINDOW_HEIGHT)),
                app,
            )),
            kind: WindowKind::Floating,
            is_resizable: false,
            is_minimizable: false,
            window_min_size: Some(size(
                px(SEARCH_DIALOG_WIDTH),
                px(SEARCH_DIALOG_WINDOW_HEIGHT),
            )),
            ..Default::default()
        };

        match app.open_window(search_window_options, move |window, app| {
            window.focus(&search_input_focus);
            window.on_window_should_close(app, move |_, app| {
                main_view_for_close.update(app, |view, context| {
                    view.search.search_dialog_window = None;
                    view.clear_search_dialog_state(true, context);
                });
                true
            });
            app.new(|context| SearchDialogWindowView::new(main_view_for_window, context))
        }) {
            Ok(search_window) => {
                main_view.update(app, |view, context| {
                    view.search.search_dialog_window = Some(search_window);
                    context.notify();
                });
            }
            Err(error) => {
                main_view.update(app, |view, context| {
                    view.search.search_dialog_window = None;
                    if let Some(dialog) = view.search.search_dialog.as_mut() {
                        dialog.message = format!("打开搜索窗口失败：{error}");
                        dialog.is_searching = false;
                    }
                    context.notify();
                });
            }
        }
    }

    /// 关闭搜索对话框并让当前后台搜索任务失效。
    ///
    /// 业务意图：
    /// - 用户关闭对话框时表示不再关注当前搜索过程；旧任务即使稍后返回，也不应继续更新进度或结果。
    /// - 结果面板不在这里清空，方便用户保留已完成的结果上下文；如果任务仍在运行，则标记为已取消，避免面板永远停留在进行中。
    fn close_search_dialog(&mut self, window: &mut Window, context: &mut Context<Self>) {
        let search_window = self.search.search_dialog_window.take();
        let current_window_is_search = search_window
            .map(AnyWindowHandle::from)
            .is_some_and(|search_window| search_window == window.window_handle());
        self.clear_search_dialog_state(true, context);
        if current_window_is_search {
            window.remove_window();
        } else if let Some(search_window) = search_window {
            let _ = search_window.update(context, |_, window, _| {
                window.remove_window();
            });
            window.focus(&self.root_focus_handle);
        } else {
            window.focus(&self.root_focus_handle);
        }
        context.notify();
    }

    /// 清理搜索对话框状态。
    ///
    /// 业务意图：
    /// - 关闭搜索窗口、搜索完成自动收起和系统窗口关闭都需要同一套状态清理规则。
    /// - 取消关闭时需要标记正在运行的记录为已取消；正常完成时只清空对话框，不改变结果记录终态。
    fn clear_search_dialog_state(&mut self, cancel_running: bool, context: &mut Context<Self>) {
        let running_job_id = self
            .search
            .search_dialog
            .as_ref()
            .filter(|dialog| cancel_running && dialog.is_searching)
            .map(|dialog| dialog.job_id);
        if self.search.search_dialog.is_none() {
            return;
        }
        self.search.search_dialog = None;
        self.search.next_search_job_id += 1;
        if let Some(job_id) = running_job_id {
            self.mark_search_record_canceled(job_id);
        }
        context.notify();
    }

    /// 停止当前搜索任务但保留搜索窗口。
    ///
    /// 业务意图：
    /// - 用户点击“停止”表示只中断本轮搜索，不应丢失已输入关键字、范围、大小写和目录目标。
    /// - 通过推进 `next_search_job_id` 并清理 `is_searching`，后续后台回调会被 `is_current_search_job` 丢弃，
    ///   避免已经取消的任务继续更新进度或在完成时关闭搜索窗口。
    fn stop_current_search(&mut self, context: &mut Context<Self>) {
        let canceled_job_id = self
            .search
            .search_dialog
            .as_mut()
            .and_then(Self::stop_search_dialog_task);
        let Some(job_id) = canceled_job_id else {
            context.notify();
            return;
        };

        self.search.next_search_job_id += 1;
        self.mark_search_record_canceled(job_id);
        context.notify();
    }

    /// 启动一次搜索。
    ///
    /// 业务意图：
    /// - 从搜索对话框读取当前查询词、范围和大小写规则，统一分发到当前文件或当前目录搜索。
    /// - 新搜索会保留历史记录，但如果上一轮搜索仍在运行，必须先标记为已取消，避免旧任务回调被丢弃后面板长期显示“搜索中”。
    fn start_search(&mut self, context: &mut Context<Self>) {
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.query_history_menu_open = false;
        }
        let Some((
            query,
            scope,
            case_sensitive,
            match_mode,
            directory_target,
            previous_running_job_id,
        )) = ({
            self.search.search_dialog.as_ref().map(|dialog| {
                (
                    dialog.query_input.text.trim().to_string(),
                    dialog.scope,
                    dialog.case_sensitive,
                    dialog.match_mode,
                    dialog.directory_input.text.trim().to_string(),
                    dialog.is_searching.then_some(dialog.job_id),
                )
            })
        })
        else {
            return;
        };
        let options = SearchOptions::single_with_mode(query.clone(), case_sensitive, match_mode);
        if options.is_empty_query() {
            self.update_search_start_failure_message(
                previous_running_job_id,
                "请输入要搜索的关键字",
                context,
            );
            return;
        }
        if let Err(error) = options.validate() {
            self.update_search_start_failure_message(
                previous_running_job_id,
                error.to_string(),
                context,
            );
            return;
        }
        self.remember_search_query(&query, match_mode);

        self.start_search_job(
            query,
            options,
            scope,
            case_sensitive,
            match_mode,
            directory_target,
            previous_running_job_id,
            context,
        );
    }

    /// 启动一次快搜。
    ///
    /// 业务意图：
    /// - 快搜使用“设置-日志”中已保存的英文逗号分隔关键字，按 OR 语义搜索任一关键字命中的行。
    /// - 快搜复用搜索窗口当前范围、目录目标和大小写选项，但不修改搜索框文本，也不写入普通搜索历史。
    fn start_quick_search(&mut self, context: &mut Context<Self>) {
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.query_history_menu_open = false;
        }
        let previous_running_job_id = self
            .search
            .search_dialog
            .as_ref()
            .and_then(|dialog| dialog.is_searching.then_some(dialog.job_id));
        let keywords = self.effective_quick_search_keywords();
        if keywords.is_empty() {
            self.update_search_start_failure_message(
                previous_running_job_id,
                "请先在设置-日志中配置快搜关键字",
                context,
            );
            return;
        }
        let Some((scope, case_sensitive, directory_target, previous_running_job_id)) = ({
            self.search.search_dialog.as_ref().map(|dialog| {
                (
                    dialog.scope,
                    dialog.case_sensitive,
                    dialog.directory_input.text.trim().to_string(),
                    dialog.is_searching.then_some(dialog.job_id),
                )
            })
        }) else {
            return;
        };
        let record_query = format!("快搜：{}", keywords.join(", "));
        let options = SearchOptions::any(keywords, case_sensitive);

        self.start_search_job(
            record_query,
            options,
            scope,
            case_sensitive,
            SearchMatchMode::Literal,
            directory_target,
            previous_running_job_id,
            context,
        );
    }

    /// 按给定选项创建搜索任务。
    ///
    /// 业务意图：
    /// - 普通搜索和快搜只在关键字来源和结果记录标题上不同，真正的范围校验、后台任务、进度和结果面板应复用同一条路径。
    /// - 该函数是搜索任务编排边界，参数来自 UI 表单快照；此阶段不拆参数对象，避免改变搜索语义。
    #[allow(clippy::too_many_arguments)]
    fn start_search_job(
        &mut self,
        record_query: String,
        options: SearchOptions,
        scope: SearchScope,
        case_sensitive: bool,
        match_mode: SearchMatchMode,
        directory_target: String,
        previous_running_job_id: Option<usize>,
        context: &mut Context<Self>,
    ) {
        if options.is_empty_query() {
            self.update_search_start_failure_message(
                previous_running_job_id,
                "请输入要搜索的关键字",
                context,
            );
            return;
        }
        if let Err(error) = options.validate() {
            self.update_search_start_failure_message(
                previous_running_job_id,
                error.to_string(),
                context,
            );
            return;
        }

        let Some(active_tab_id) = self.log.active_tab_id else {
            self.update_search_start_failure_message(
                previous_running_job_id,
                "请先从左侧打开一个日志文件",
                context,
            );
            return;
        };
        let Some(active_tab) = self
            .log
            .open_tabs
            .iter()
            .find(|tab| tab.id == active_tab_id)
        else {
            self.update_search_start_failure_message(
                previous_running_job_id,
                "当前日志 tab 不存在，请重新选择文件",
                context,
            );
            return;
        };

        let search_target = match scope {
            SearchScope::CurrentFile => match &active_tab.state {
                LogTabState::Ready { document } => SearchTarget::CurrentFile {
                    source: active_tab.source.clone(),
                    document: document.clone(),
                },
                LogTabState::Loading { .. } => {
                    self.update_search_start_failure_message(
                        previous_running_job_id,
                        "当前文件仍在加载，完成后再搜索",
                        context,
                    );
                    return;
                }
                LogTabState::Failed { .. } => {
                    self.update_search_start_failure_message(
                        previous_running_job_id,
                        "当前文件打开失败，无法搜索正文",
                        context,
                    );
                    return;
                }
            },
            SearchScope::CurrentDirectory => {
                let LogTreeLoadState::Loaded(tree_state) = &self.log.load_state else {
                    self.update_search_start_failure_message(
                        previous_running_job_id,
                        "请先加载日志目录后再搜索当前目录",
                        context,
                    );
                    return;
                };
                let sources = if directory_target.is_empty() {
                    collect_current_directory_sources(&tree_state.tree, &active_tab.source)
                } else {
                    Self::collect_sources_matching_directory_target(
                        &tree_state.tree,
                        &directory_target,
                    )
                };
                if sources.is_empty() {
                    self.update_search_start_failure_message(
                        previous_running_job_id,
                        "当前目录中没有可搜索的日志文件",
                        context,
                    );
                    return;
                }
                SearchTarget::CurrentDirectory { sources }
            }
        };

        let job_id = self.search.next_search_job_id;
        self.search.next_search_job_id += 1;

        let total_files = match &search_target {
            SearchTarget::CurrentFile { .. } => 1,
            SearchTarget::CurrentDirectory { sources } => sources.len(),
        };
        let panel_height = self
            .search
            .search_results_panel
            .as_ref()
            .map(|panel| panel.height)
            .unwrap_or(SEARCH_RESULTS_PANEL_DEFAULT_HEIGHT);
        let new_record = SearchHistoryRecord {
            job_id,
            query: record_query,
            scope,
            directory_target: (scope == SearchScope::CurrentDirectory)
                .then(|| directory_target.clone())
                .filter(|target| !target.is_empty()),
            case_sensitive,
            match_mode,
            progress: SearchProgress {
                searched_files: 0,
                total_files,
                matched_lines: 0,
            },
            results: Vec::new(),
            errors: Vec::new(),
            canceled: false,
            expanded: true,
            expanded_file_keys: HashSet::new(),
        };
        if let Some(previous_job_id) = previous_running_job_id {
            self.mark_search_record_canceled(previous_job_id);
        }
        if let Some(panel) = self.search.search_results_panel.as_mut() {
            for record in &mut panel.records {
                record.expanded = false;
            }
            panel.records.push(new_record);
            panel.rows = Self::search_results_panel_rows_from_records(&panel.records);
            panel.scroll_handle = UniformListScrollHandle::new();
        } else {
            let records = vec![new_record];
            let rows = Self::search_results_panel_rows_from_records(&records);
            self.search.search_results_panel = Some(SearchResultsPanelState {
                records,
                rows,
                height: panel_height,
                scroll_handle: UniformListScrollHandle::new(),
            });
        }
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.job_id = job_id;
            dialog.is_searching = true;
            dialog.progress = SearchProgress {
                searched_files: 0,
                total_files,
                matched_lines: 0,
            };
            dialog.message = format!("正在搜索 0/{total_files} 个文件...");
        }
        context.notify();

        match search_target {
            SearchTarget::CurrentFile { source, document } => {
                self.spawn_current_file_search(job_id, source, document, options, context);
            }
            SearchTarget::CurrentDirectory { sources } => {
                self.spawn_current_directory_search(job_id, sources, options, context);
            }
        }
    }

    /// 处理新搜索启动失败时的旧任务收尾。
    ///
    /// 业务意图：
    /// - 用户可能在上一轮搜索仍运行时按 Enter 发起新搜索；如果新条件校验失败，旧后台回调会因为对话框停止搜索而失效。
    /// - 这种情况下必须同步把旧结果记录标记为已取消，否则底部面板会长期显示“搜索中”。
    fn update_search_start_failure_message(
        &mut self,
        previous_running_job_id: Option<usize>,
        message: impl Into<String>,
        context: &mut Context<Self>,
    ) {
        if let Some(job_id) = previous_running_job_id {
            self.search.next_search_job_id += 1;
            self.mark_search_record_canceled(job_id);
        }
        self.update_search_dialog_message(message, context);
    }

    /// 更新搜索对话框提示文案。
    fn update_search_dialog_message(
        &mut self,
        message: impl Into<String>,
        context: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.message = message.into();
            dialog.is_searching = false;
        }
        context.notify();
    }

    /// 启动当前文件后台搜索。
    ///
    /// 业务意图：
    /// - 当前文件虽然已经解码，但逐行搜索大日志仍可能耗时，因此放到后台执行器运行。
    /// - 这里传入 `Arc<Vec<String>>`，只共享已解码行集合，不在 UI 线程复制整份日志文本。
    fn spawn_current_file_search(
        &self,
        job_id: usize,
        source: LogFileSource,
        document: LogTabDocument,
        options: SearchOptions,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |view, app| {
                let results = app
                    .background_executor()
                    .spawn(async move {
                        match document {
                            LogTabDocument::InMemory(document) => {
                                search_lines(&source, document.lines.as_ref(), &options)
                            }
                            LogTabDocument::Paged(document) => {
                                let outcome = search_paged_document(&document, &options);
                                let _ = outcome.truncated;
                                outcome.results
                            }
                        }
                    })
                    .await;

                view.update(app, |view, context| {
                    view.apply_search_file_result(job_id, Ok(results), 1);
                    view.finish_search_job(job_id, context);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 启动当前目录递归后台搜索。
    ///
    /// 业务意图：
    /// - 目录搜索可能涉及多个本地文件或压缩包成员，必须逐文件放到后台读取和解码。
    /// - 每个文件完成后立即回传进度，让搜索对话框显示真实进展，而不是等全部完成才更新。
    /// - 串行搜索能避免同一压缩包被多个后台任务并发打开和解压，适合日志包内大量成员的常见场景。
    fn spawn_current_directory_search(
        &self,
        job_id: usize,
        sources: Vec<LogFileSource>,
        options: SearchOptions,
        context: &mut Context<Self>,
    ) {
        let total_files = sources.len();
        context
            .spawn(async move |view, app| {
                for source in sources {
                    let search_options = options.clone();
                    let result = app
                        .background_executor()
                        .spawn(async move { Self::search_one_source(source, search_options) })
                        .await;

                    let should_continue = view
                        .update(app, |view, context| {
                            let current =
                                view.apply_search_file_result(job_id, result, total_files);
                            context.notify();
                            current
                        })
                        .unwrap_or(false);

                    if !should_continue {
                        return;
                    }
                }

                view.update(app, |view, context| {
                    view.finish_search_job(job_id, context);
                    context.notify();
                })
                .ok();
            })
            .detach();
    }

    /// 搜索单个文件来源。
    ///
    /// 业务意图：
    /// - 目录搜索中的每个文件独立读取、自动解码和搜索；失败时转换为文件级错误，调用方继续处理其它文件。
    /// - 这里复用现有大小上限和编码检测逻辑，避免搜索路径绕过日志打开边界。
    fn search_one_source(
        source: LogFileSource,
        options: SearchOptions,
    ) -> Result<Vec<SearchResultItem>, SearchFileError> {
        let file_name = source.display_name();
        let opened = open_log_source_for_tab(source.clone(), EncodingChoice::Auto, &file_name)
            .map_err(|error| SearchFileError {
                file_name: file_name.clone(),
                message: error.to_string(),
            })?;

        match opened {
            LargeLogOpenResult::InMemoryReady { document, .. } => {
                Ok(search_lines(&source, &document.lines, &options))
            }
            LargeLogOpenResult::InMemoryDecodeFailed { message, .. } => {
                Err(SearchFileError { file_name, message })
            }
            LargeLogOpenResult::PagedReady { document } => {
                let outcome = search_paged_document(&document, &options);
                let _ = outcome.truncated;
                if let Some(temp_path) = document.materialized_temp_path.as_deref() {
                    cleanup_materialized_file(temp_path);
                }
                Ok(outcome.results)
            }
        }
    }

    /// 合并单个文件的搜索结果并返回任务是否仍然有效。
    ///
    /// 业务意图：
    /// - 后台任务回到 UI 线程时必须校验任务 ID，避免旧任务覆盖新搜索状态。
    /// - 文件级错误只累积到结果面板，不中断当前任务。
    fn apply_search_file_result(
        &mut self,
        job_id: usize,
        result: Result<Vec<SearchResultItem>, SearchFileError>,
        total_files: usize,
    ) -> bool {
        if !self.is_current_search_job(job_id) {
            return false;
        }

        let mut matched_lines = 0usize;
        if let Some(panel) = self.search.search_results_panel.as_mut() {
            {
                let Some(record) = panel
                    .records
                    .iter_mut()
                    .find(|record| record.job_id == job_id)
                else {
                    return false;
                };
                match result {
                    Ok(mut results) => {
                        matched_lines = results.len();
                        for result in &results {
                            record.expanded_file_keys.insert(result.source_key.clone());
                        }
                        record.results.append(&mut results);
                    }
                    Err(error) => {
                        record.errors.push(error);
                    }
                }
                record.progress.searched_files += 1;
                record.progress.total_files = total_files;
                record.progress.matched_lines = record.results.len();
            }
            panel.rows = Self::search_results_panel_rows_from_records(&panel.records);
        }

        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.progress.searched_files += 1;
            dialog.progress.total_files = total_files;
            dialog.progress.matched_lines += matched_lines;
            dialog.message = format!(
                "正在搜索 {}/{} 个文件，已命中 {} 行",
                dialog.progress.searched_files,
                dialog.progress.total_files,
                dialog.progress.matched_lines
            );
        }

        true
    }

    /// 标记搜索任务完成并关闭搜索对话框。
    ///
    /// 业务意图：
    /// - 搜索对话框只负责输入条件和展示进行中进度；任务完成后应自动收起，把空间让给正文和底部结果面板。
    /// - 结果面板保留历史记录和明细，用户可以继续查看、展开和点击定位。
    fn finish_search_job(&mut self, job_id: usize, context: &mut Context<Self>) {
        if !self.is_current_search_job(job_id) {
            return;
        }

        if let Some(panel) = self.search.search_results_panel.as_mut()
            && let Some(record) = panel
                .records
                .iter_mut()
                .find(|record| record.job_id == job_id)
        {
            record.canceled = false;
        }
        self.search.search_dialog = None;
        if let Some(search_window) = self.search.search_dialog_window.take() {
            let _ = search_window.update(context, |_, window, _| {
                window.remove_window();
            });
        }
    }

    /// 判断后台回调是否属于当前仍有效的搜索任务。
    fn is_current_search_job(&self, job_id: usize) -> bool {
        self.search
            .search_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.job_id == job_id && dialog.is_searching)
    }

    /// 点击搜索结果后打开文件并滚动到命中行。
    ///
    /// 业务意图：
    /// - 搜索结果不仅用于查看，还应成为跨文件定位入口。
    /// - 如果目标文件尚未打开，则新建 tab；如果已经打开，则直接激活并滚动。
    fn open_search_result(&mut self, result: SearchResultItem, context: &mut Context<Self>) {
        let source_key = result.source_key.clone();
        let line_index = result.line_index;

        if !self
            .log
            .open_tabs
            .iter()
            .any(|tab| tab.source_key == source_key)
        {
            self.open_log_file(result.source.clone(), context);
        }

        let Some(tab_index) = self
            .log
            .open_tabs
            .iter()
            .position(|tab| tab.source_key == source_key)
        else {
            return;
        };
        let tab_id = self.log.open_tabs[tab_index].id;
        let ready = matches!(
            self.log.open_tabs[tab_index].state,
            LogTabState::Ready { .. }
        );
        self.log.open_tabs[tab_index].pending_scroll_to_line = Some(line_index);
        self.log.open_tabs[tab_index].highlighted_search_line = Some(line_index);
        self.activate_tab(tab_id);
        if ready {
            self.log.open_tabs[tab_index].pending_scroll_to_line = None;
            self.scroll_log_tab_to_line(tab_id, line_index);
        }
        context.notify();
    }

    /// 处理根视图鼠标移动。
    ///
    /// 业务意图：
    /// - 结果面板高度调整和结果面板滚动条拖动都可能跨过右侧正文、左侧树或工具栏区域，因此放到根视图统一处理。
    /// - 搜索对话框已经改为独立窗口，不再需要主窗口接管拖动过程。
    /// - 现有左右分割线和滚动条拖动仍在内容区处理，避免扩大它们的鼠标命中范围。
    fn handle_root_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.update_search_results_resize_drag(event, window, context);
        self.update_search_results_scrollbar_drag(event, context);
    }

    /// 处理根视图鼠标释放。
    ///
    /// 业务意图：
    /// - 结果面板 resize 和结果面板滚动条拖动都依赖鼠标释放清理临时状态。
    fn handle_root_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let had_drag = self.search.search_results_resize_drag.take().is_some();
        let had_scrollbar_drag = self.search.search_results_scrollbar_drag.take().is_some();
        self.stop_log_text_selection(context);
        if had_drag || had_scrollbar_drag {
            context.notify();
        }
    }

    /// 开始调整搜索结果面板高度。
    fn start_search_results_resize(&mut self, event: &MouseDownEvent) {
        let Some(panel) = &self.search.search_results_panel else {
            return;
        };
        self.search.search_results_resize_drag = Some(SearchResultsResizeDrag {
            start_y: event.position.y,
            start_height: panel.height,
        });
        self.search.search_results_scrollbar_drag = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
    }

    /// 根据鼠标拖动更新搜索结果面板高度。
    fn update_search_results_resize_drag(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.search.search_results_resize_drag else {
            return;
        };
        if !event.dragging() {
            self.search.search_results_resize_drag = None;
            context.notify();
            return;
        }

        let Some(panel) = self.search.search_results_panel.as_mut() else {
            self.search.search_results_resize_drag = None;
            context.notify();
            return;
        };
        let content_height = (f32::from(window.viewport_size().height) - TOOLBAR_HEIGHT).max(1.0);
        let max_height =
            (content_height * SEARCH_RESULTS_PANEL_MAX_RATIO).max(SEARCH_RESULTS_PANEL_MIN_HEIGHT);
        let next_height = drag.start_height + f32::from(drag.start_y - event.position.y);
        panel.height = next_height.clamp(SEARCH_RESULTS_PANEL_MIN_HEIGHT, max_height);
        context.notify();
    }

    /// 开始拖动搜索结果面板滚动条滑块。
    ///
    /// 业务意图：
    /// - 搜索结果面板可能包含大量历史记录和命中行，需要支持直接拖动滚动条快速定位。
    /// - 拖动开始时记录鼠标在滑块内的偏移，避免滑块突然跳到鼠标中心。
    ///
    /// 边界条件：
    /// - 如果结果面板未打开、列表尚未完成测量或内容不足以滚动，则忽略本次按下。
    fn start_search_results_scrollbar_drag(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        self.note_keyboard_scroll_region(KeyboardScrollRegion::SearchResults);
        let Some(panel) = &self.search.search_results_panel else {
            return;
        };
        let Some(metrics) = Self::search_results_scrollbar_metrics(&panel.scroll_handle) else {
            return;
        };
        if metrics.max_scroll <= px(0.0) {
            return;
        }
        let Some(viewport_top) = Self::uniform_list_viewport_axis_origin(
            &panel.scroll_handle,
            LogScrollbarAxis::Vertical,
        ) else {
            return;
        };

        self.search.search_results_scrollbar_drag = Some(SearchResultsScrollbarDrag {
            cursor_offset: event.position.y - viewport_top - metrics.thumb_start,
        });
        self.search.search_results_resize_drag = None;
        self.search.search_results_context_menu = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        self.stop_log_text_selection(context);
    }

    /// 根据鼠标移动更新搜索结果面板滚动条拖动结果。
    ///
    /// 业务意图：
    /// - 自绘滚动条拖动必须写回搜索结果虚拟列表的底层滚动偏移，才能和滚轮滚动、虚拟渲染保持一致。
    /// - 只在鼠标左键仍按下时更新；如果释放发生在其它区域，也会在下一次移动时清理拖动状态。
    fn update_search_results_scrollbar_drag(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.search.search_results_scrollbar_drag else {
            return;
        };
        if !event.dragging() {
            self.search.search_results_scrollbar_drag = None;
            context.notify();
            return;
        }
        let Some(panel) = self.search.search_results_panel.as_ref() else {
            self.search.search_results_scrollbar_drag = None;
            context.notify();
            return;
        };
        let Some(metrics) = Self::search_results_scrollbar_metrics(&panel.scroll_handle) else {
            self.search.search_results_scrollbar_drag = None;
            context.notify();
            return;
        };
        let Some(viewport_top) = Self::uniform_list_viewport_axis_origin(
            &panel.scroll_handle,
            LogScrollbarAxis::Vertical,
        ) else {
            self.search.search_results_scrollbar_drag = None;
            context.notify();
            return;
        };

        let movable_length = (metrics.track_length - metrics.thumb_length).max(px(0.0));
        if metrics.max_scroll <= px(0.0) || movable_length <= px(0.0) {
            return;
        }

        let requested_thumb_start = event.position.y - viewport_top - drag.cursor_offset;
        let thumb_start =
            requested_thumb_start.clamp(metrics.track_start, metrics.track_start + movable_length);
        let scroll_offset =
            metrics.max_scroll * ((thumb_start - metrics.track_start) / movable_length);
        let base_scroll_handle = {
            // `UniformListScrollHandle` 包装了真正的 `ScrollHandle`；克隆后再写入，避免持有 RefCell 借用时触发嵌套借用。
            panel.scroll_handle.0.borrow().base_handle.clone()
        };
        let current_offset = base_scroll_handle.offset();
        base_scroll_handle.set_offset(point(current_offset.x, -scroll_offset));
        context.notify();
    }

    /// 根据真实加载节点类型返回目录树图标和颜色。
    ///
    /// 业务意图：
    /// - 将类型到视觉符号的映射集中管理，后续新增节点类型时不会散落在多个渲染分支里。
    fn loaded_log_tree_icon(kind: LogTreeEntryKind, palette: AppThemePalette) -> (Icon, u32) {
        match kind {
            LogTreeEntryKind::Directory => (Icon::FolderOpen, palette.accent),
            LogTreeEntryKind::File => (Icon::FileText, palette.muted_text),
            LogTreeEntryKind::Archive => (Icon::FileArchive, 0x8250df),
            LogTreeEntryKind::Symlink => (Icon::FolderSymlink, palette.muted_text),
            LogTreeEntryKind::Error => (Icon::FileX, palette.error),
        }
    }

    /// 渲染目录树节点右侧的补充信息。
    ///
    /// 业务意图：
    /// - 元信息用于展示文件大小或错误类别，让目录树信息更便于扫描。
    /// - 没有元信息的节点仍通过统一函数返回空占位，保持行模板简单稳定。
    ///
    /// 边界条件：
    /// - 当前元信息不参与排序、过滤或诊断，只是短文本展示。
    /// - 后续若元信息可能很长，需要定义截断、悬浮提示和优先级规则。
    fn render_log_tree_meta(meta: Option<&str>, palette: AppThemePalette) -> gpui::Div {
        let meta_element = div()
            .flex_none()
            .text_size(px(LOG_TREE_FONT_SIZE))
            .text_color(rgb(palette.muted_text))
            .child(meta.unwrap_or_default().to_string());

        if meta.is_some() {
            meta_element
        } else {
            meta_element.hidden()
        }
    }

    /// 开始拖动左右分栏分割线。
    ///
    /// 业务意图：
    /// - 用户按下分割线后进入调整模式，后续鼠标移动将改变左侧区域宽度。
    ///
    /// 边界条件：
    /// - 只响应鼠标左键，避免右键菜单或其它鼠标键误触发布局变化。
    /// - 按下瞬间不立即改变宽度，实际宽度在移动事件中按当前位置计算。
    fn start_resizing_splitter(
        &mut self,
        _event: &MouseDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.is_resizing_splitter = true;
        // 分栏拖拽条覆盖在左右内容之间，按下后必须截断，避免底层目录树或日志区同步收到鼠标事件。
        context.stop_propagation();
    }

    /// 开始拖动左侧目录树滚动条滑块。
    ///
    /// 业务意图：
    /// - 目录树节点较多时，用户需要可以直接拖动滚动条快速定位，而不是只能依赖滚轮。
    /// - 记录鼠标在滑块内的偏移，避免拖动开始时滑块位置突变。
    ///
    /// 边界条件：
    /// - 如果列表尚未完成测量或内容不足以滚动，则忽略本次按下。
    fn start_log_tree_scrollbar_drag(&mut self, event: &MouseDownEvent) {
        self.note_keyboard_scroll_region(KeyboardScrollRegion::LogTree);
        let Some(metrics) = Self::log_tree_scrollbar_metrics(&self.log.log_tree_scroll_handle)
        else {
            return;
        };
        if metrics.max_scroll <= px(0.0) {
            return;
        }
        let Some(viewport_top) = Self::uniform_list_viewport_axis_origin(
            &self.log.log_tree_scroll_handle,
            LogScrollbarAxis::Vertical,
        ) else {
            return;
        };

        self.log.log_tree_scrollbar_drag = Some(LogTreeScrollbarDrag {
            cursor_offset: event.position.y - viewport_top - metrics.thumb_start,
        });
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
    }

    /// 处理内容区鼠标移动事件。
    ///
    /// 业务意图：
    /// - 左右分割线拖动、左侧树滚动条拖动和日志滚动条拖动都依赖窗口级鼠标移动；统一入口可以保证这些交互在同一帧内得到更新。
    /// - 该方法只分发交互，不直接渲染 UI，便于后续继续追加其它拖动类操作。
    fn handle_content_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.resize_splitter(event, window, context);
        self.update_log_tree_scrollbar_drag(event, context);
        self.update_log_scrollbar_drag(event, context);
    }

    /// 处理内容区鼠标左键释放事件。
    ///
    /// 业务意图：
    /// - 用户释放鼠标时需要同时结束分割线拖动、左侧树滚动条拖动和日志滚动条拖动，避免后续移动继续改变布局或滚动位置。
    fn handle_content_mouse_up(
        &mut self,
        event: &MouseUpEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.stop_resizing_splitter(event, window, context);
        self.stop_log_tree_scrollbar_drag(context);
        self.stop_log_scrollbar_drag(context);
        self.stop_log_text_selection(context);
    }

    /// 根据鼠标移动更新左侧目录树滚动条拖动结果。
    ///
    /// 业务意图：
    /// - 自绘滚动条拖动必须写回目录树虚拟列表底层滚动偏移，才能和滚轮滚动、虚拟行渲染保持一致。
    /// - 只在鼠标左键仍按下时更新；如果释放发生在其它元素上，也会在下一次移动时清理拖动状态。
    fn update_log_tree_scrollbar_drag(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.log.log_tree_scrollbar_drag else {
            return;
        };
        if !event.dragging() {
            self.log.log_tree_scrollbar_drag = None;
            context.notify();
            return;
        }
        let Some(metrics) = Self::log_tree_scrollbar_metrics(&self.log.log_tree_scroll_handle)
        else {
            self.log.log_tree_scrollbar_drag = None;
            context.notify();
            return;
        };
        let Some(viewport_top) = Self::uniform_list_viewport_axis_origin(
            &self.log.log_tree_scroll_handle,
            LogScrollbarAxis::Vertical,
        ) else {
            self.log.log_tree_scrollbar_drag = None;
            context.notify();
            return;
        };

        let movable_length = (metrics.track_length - metrics.thumb_length).max(px(0.0));
        if metrics.max_scroll <= px(0.0) || movable_length <= px(0.0) {
            return;
        }

        let requested_thumb_start = event.position.y - viewport_top - drag.cursor_offset;
        let thumb_start =
            requested_thumb_start.clamp(metrics.track_start, metrics.track_start + movable_length);
        let scroll_offset =
            metrics.max_scroll * ((thumb_start - metrics.track_start) / movable_length);
        let base_scroll_handle = {
            // 克隆底层滚动句柄后释放 `RefCell` 借用，再写入偏移，避免测量状态借用和滚动状态更新交叉。
            self.log
                .log_tree_scroll_handle
                .0
                .borrow()
                .base_handle
                .clone()
        };
        let current_offset = base_scroll_handle.offset();
        base_scroll_handle.set_offset(point(current_offset.x, -scroll_offset));
        context.notify();
    }

    /// 结束左侧目录树滚动条拖动。
    ///
    /// 业务意图：
    /// - 鼠标释放后清理拖动状态，避免普通鼠标移动继续改变目录树滚动位置。
    fn stop_log_tree_scrollbar_drag(&mut self, context: &mut Context<Self>) {
        if self.log.log_tree_scrollbar_drag.is_some() {
            self.log.log_tree_scrollbar_drag = None;
            context.notify();
        }
    }

    /// 拖动分割线时更新左侧内容区域宽度。
    ///
    /// 业务意图：
    /// - 将鼠标横坐标映射为左侧区域宽度，从而提供直接、可预期的分栏拖动体验。
    ///
    /// 边界条件：
    /// - 只有处于拖动状态时才更新宽度。
    /// - 宽度会被限制在左侧最小宽度和右侧最小可用空间之间，避免任一面板不可用。
    fn resize_splitter(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if !self.is_resizing_splitter {
            return;
        }

        let log_page_x = (f32::from(event.position.x) - MAIN_NAV_WIDTH).max(0.0);
        let log_page_width = (f32::from(window.bounds().size.width) - MAIN_NAV_WIDTH).max(0.0);
        self.left_panel_width = Self::clamp_left_panel_width(log_page_x, log_page_width);
        context.notify();
    }

    /// 结束拖动左右分栏分割线。
    ///
    /// 业务意图：
    /// - 鼠标左键释放后退出调整模式，后续鼠标移动不再改变分栏宽度。
    ///
    /// 边界条件：
    /// - 当前不做宽度持久化，因此释放动作只影响内存状态。
    fn stop_resizing_splitter(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        _context: &mut Context<Self>,
    ) {
        self.is_resizing_splitter = false;
    }

    /// 将左侧区域宽度限制在可用范围内。
    ///
    /// 业务意图：
    /// - 分割线拖动必须保留左右两栏的基本可用空间。
    /// - 独立函数便于后续为该边界逻辑补充单元测试。
    ///
    /// 边界条件：
    /// - 如果窗口极窄导致右侧最小宽度无法满足，仍优先保证左侧不低于最小宽度。
    /// - 当前只使用窗口总宽度计算，工具栏高度不影响水平分割。
    fn clamp_left_panel_width(requested_width: f32, window_width: f32) -> f32 {
        let max_left_width = (window_width - SPLITTER_VISIBLE_WIDTH - RIGHT_PANEL_MIN_WIDTH)
            .max(LEFT_PANEL_MIN_WIDTH);
        requested_width.clamp(LEFT_PANEL_MIN_WIDTH, max_left_width)
    }

    /// 渲染未显示左侧目录树时的主内容提示。
    ///
    /// 业务意图：
    /// - 用户明确要求未加载日志时不显示左侧区域，因此主内容区需要占满窗口剩余空间。
    /// - 提示文案直接说明下一步动作，避免空白内容区让用户误以为界面卡住。
    ///
    /// 边界条件：
    /// - 加载中和加载失败也使用同一块主内容区提示，不提前显示左侧栏。
    /// - 当前提示不承载按钮，避免和顶部“加载日志”入口形成重复操作路径。
    fn render_primary_content_message(&self) -> impl IntoElement {
        let palette = self.palette();
        if let LogTreeLoadState::Loading { message } = &self.log.load_state {
            return div()
                .id("primary-content-loading-message")
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .size_full()
                .px_4()
                .bg(rgb(palette.background))
                .child(Self::render_loading_spinner(palette.accent))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(palette.muted_text))
                        .text_center()
                        .child(message.clone()),
                );
        }

        let (icon, icon_color, title, description) = match &self.log.load_state {
            LogTreeLoadState::Empty => (
                Icon::FileText,
                palette.muted_text,
                "请先加载日志".to_string(),
                "点击日志页顶部“加载日志”，选择日志文件、目录或压缩包。".to_string(),
            ),
            LogTreeLoadState::Failed { message } => (
                Icon::FileX,
                palette.error,
                "加载日志失败".to_string(),
                message.clone(),
            ),
            LogTreeLoadState::Loaded(_) => (
                Icon::FileText,
                palette.muted_text,
                String::new(),
                String::new(),
            ),
            LogTreeLoadState::Loading { .. } => unreachable!("加载态已在上方提前返回"),
        };

        div()
            .id("primary-content-message")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .size_full()
            .px_4()
            .bg(rgb(palette.background))
            .child(Self::render_lucide_icon(Some(icon), 32.0, 28.0, icon_color))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child(title),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_text))
                    .child(description),
            )
    }

    /// 渲染右侧日志工作区。
    ///
    /// 业务意图：
    /// - 加载日志后但尚未打开任何文件时，右侧给出“点击左侧日志文件查看内容”的友好提示。
    /// - 打开文件后显示 tab 栏、编码切换工具条和只读日志正文；单日志模式隐藏 tab 栏，把空间留给正文。
    /// - 搜索结果面板打开后作为底部分栏参与布局，日志正文和滚动条高度会自动扣除面板高度。
    ///
    /// 边界条件：
    /// - 当前不持久化 tab，不支持拖拽重排，也不实现复制菜单。
    fn render_right_log_panel(&self, context: &mut Context<Self>) -> impl IntoElement {
        let content = if self.log.open_tabs.is_empty() {
            div()
                .id("right-log-panel-empty")
                .flex()
                .flex_1()
                .size_full()
                .child(self.render_loaded_right_empty_message())
        } else if self.should_hide_log_tree_panel() {
            div()
                .id("right-log-panel-single-tab")
                .flex()
                .flex_col()
                .flex_1()
                .size_full()
                .child(self.render_active_log_tab(context))
        } else {
            div()
                .id("right-log-panel-tabs")
                .flex()
                .flex_col()
                .flex_1()
                .size_full()
                .child(self.render_log_tab_bar(context))
                .child(self.render_active_log_tab(context))
        };

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .child(content)
            .child(self.render_search_results_panel(context))
            .child(self.render_popup_dismiss_overlay(context))
            .child(self.render_tab_context_menu(context))
            .child(self.render_log_viewer_context_menu(context))
            .child(self.render_search_results_context_menu(context))
            .child(self.render_encoding_dropdown_menu(context))
    }

    /// 读取搜索输入框当前文本、选择范围和组合文本范围的快照。
    ///
    /// 业务意图：
    /// - `SearchTextInputElement` 在 GPUI 绘制阶段只拿到 `App` 上下文，不能直接借用搜索窗口视图状态。
    /// - 通过主视图提供只读快照，保证绘制、命中测试和 IME 状态都来自同一份业务状态。
    fn search_text_snapshot(
        &self,
        input_kind: SearchTextInputKind,
    ) -> Option<(String, Range<usize>, Option<Range<usize>>)> {
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, selection_range, marked_range) = Self::search_text_state(dialog, input_kind);
        Some((text.to_string(), selection_range, marked_range))
    }

    /// 返回当前应参与线程日志分析过滤的已保存文本。
    ///
    /// 业务意图：
    /// - 设置页编辑态中的内容属于草稿，必须等用户点击保存后才影响下一次线程分析。
    /// - 如果用户一边编辑设置一边从主窗口启动线程分析，这里仍使用进入编辑前的快照，避免半成品堆栈过滤掉真实线程。
    fn thread_analysis_filter_effective_text(&self) -> &str {
        self.settings
            .thread_analysis_filter_saved_text_before_edit
            .as_deref()
            .unwrap_or(&self.settings.thread_analysis_filter_text)
    }

    /// 返回当前应参与快搜的已保存关键字文本。
    ///
    /// 业务意图：
    /// - 设置页编辑态中的快搜关键字属于草稿，必须等用户点击保存后才影响搜索对话框的快搜按钮。
    /// - 如果用户一边编辑设置一边执行快搜，这里继续使用进入编辑前的快照，避免半成品关键字触发大量无关命中。
    fn quick_search_keywords_effective_text(&self) -> &str {
        self.settings
            .quick_search_keywords_saved_text_before_edit
            .as_deref()
            .unwrap_or(&self.settings.quick_search_keywords_input.text)
    }

    /// 返回当前已保存且可用于快搜的关键字列表。
    fn effective_quick_search_keywords(&self) -> Vec<String> {
        parse_quick_search_keywords(self.quick_search_keywords_effective_text())
    }

    /// 返回当前模型配置持久化结构快照。
    ///
    /// 业务意图：
    /// - 保存、删除和设为默认都通过同一个结构写入 JSON，避免列表和默认 ID 分别落盘导致状态不一致。
    fn model_configs_snapshot(&self) -> ModelConfigs {
        normalize_model_configs(ModelConfigs {
            profiles: self.model_config.model_config_profiles.clone(),
            default_profile_id: self.model_config.model_config_default_profile_id.clone(),
        })
    }

    /// 返回模型配置输入框状态。
    fn model_config_input_state(&self, kind: ModelConfigInputKind) -> &ModelConfigTextFieldState {
        match kind {
            ModelConfigInputKind::Name => &self.model_config.model_config_name_input,
            ModelConfigInputKind::BaseUrl => &self.model_config.model_config_base_url_input,
            ModelConfigInputKind::ApiKey => &self.model_config.model_config_api_key_input,
            ModelConfigInputKind::Model => &self.model_config.model_config_model_input,
        }
    }

    /// 返回模型配置输入框可变状态。
    fn model_config_input_state_mut(
        &mut self,
        kind: ModelConfigInputKind,
    ) -> &mut ModelConfigTextFieldState {
        match kind {
            ModelConfigInputKind::Name => &mut self.model_config.model_config_name_input,
            ModelConfigInputKind::BaseUrl => &mut self.model_config.model_config_base_url_input,
            ModelConfigInputKind::ApiKey => &mut self.model_config.model_config_api_key_input,
            ModelConfigInputKind::Model => &mut self.model_config.model_config_model_input,
        }
    }

    /// 返回模型配置输入框焦点句柄。
    fn model_config_input_focus(&self, kind: ModelConfigInputKind) -> gpui::FocusHandle {
        self.model_config_input_state(kind).focus.clone()
    }

    /// 根据窗口焦点判断当前平台输入应写入哪个模型配置字段。
    fn active_model_config_input_kind(&self, window: &Window) -> Option<ModelConfigInputKind> {
        [
            ModelConfigInputKind::Name,
            ModelConfigInputKind::BaseUrl,
            ModelConfigInputKind::ApiKey,
            ModelConfigInputKind::Model,
        ]
        .into_iter()
        .find(|kind| {
            self.model_config_input_state(*kind)
                .focus
                .is_focused(window)
        })
    }

    /// 返回模型配置输入框的可见文本。
    ///
    /// 安全边界：
    /// - API Key 默认使用同等 UTF-8 字节长度的星号掩码，既避免界面明文展示，也让选区和光标索引仍能映射到可见文本。
    fn model_config_input_display_text(&self, kind: ModelConfigInputKind) -> String {
        let state = self.model_config_input_state(kind);
        if kind == ModelConfigInputKind::ApiKey
            && !self.model_config.model_config_api_key_visible
            && !state.text.is_empty()
        {
            "*".repeat(state.text.len())
        } else {
            state.text.clone()
        }
    }

    /// 读取模型配置输入框绘制快照。
    fn model_config_input_text_snapshot(
        &self,
        kind: ModelConfigInputKind,
    ) -> (String, String, Range<usize>, Option<Range<usize>>) {
        let state = self.model_config_input_state(kind);
        (
            state.text.clone(),
            self.model_config_input_display_text(kind),
            Self::clamp_search_text_range(&state.text, state.selection_range.clone()),
            state.marked_range.clone(),
        )
    }

    /// 保存模型配置输入框最近一次单行排版结果。
    fn store_model_config_input_layout(
        &mut self,
        kind: ModelConfigInputKind,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
    ) {
        let state = self.model_config_input_state_mut(kind);
        state.last_layout = Some(line);
        state.last_bounds = Some(bounds);
    }

    /// 根据鼠标窗口坐标返回模型配置输入框中的 UTF-8 字节下标。
    fn model_config_input_index_for_point(
        &self,
        kind: ModelConfigInputKind,
        position: Point<Pixels>,
    ) -> usize {
        let state = self.model_config_input_state(kind);
        let (Some(layout), Some(bounds)) = (state.last_layout.as_ref(), state.last_bounds.as_ref())
        else {
            return state.text.len();
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return state.text.len();
        }
        layout
            .closest_index_for_x(position.x - bounds.left())
            .min(state.text.len())
    }

    /// 开始模型配置输入框鼠标选择。
    fn start_model_config_input_mouse_selection(
        &mut self,
        kind: ModelConfigInputKind,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let index = self.model_config_input_index_for_point(kind, event.position);
        let state = self.model_config_input_state_mut(kind);
        state.marked_range = None;
        match event.click_count {
            0 | 1 => {
                if event.modifiers.shift {
                    state.selection_range.end = index;
                    state.selection_range =
                        Self::clamp_search_text_range(&state.text, state.selection_range.clone());
                } else {
                    state.selection_range = index..index;
                }
                state.selection_drag = Some(state.selection_range.start);
            }
            2 => {
                state.selection_range = Self::search_text_word_range_for_index(&state.text, index);
                state.selection_drag = None;
            }
            _ => {
                state.selection_range = 0..state.text.len();
                state.selection_drag = None;
            }
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 鼠标拖拽时更新模型配置输入框选区。
    fn update_model_config_input_mouse_selection(
        &mut self,
        kind: ModelConfigInputKind,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) {
        let index = self.model_config_input_index_for_point(kind, position);
        let state = self.model_config_input_state_mut(kind);
        let Some(anchor) = state.selection_drag else {
            return;
        };
        state.marked_range = None;
        state.selection_range = Self::clamp_search_text_range(&state.text, anchor..index);
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 结束模型配置输入框鼠标拖拽选择。
    fn finish_model_config_input_mouse_selection(
        &mut self,
        kind: ModelConfigInputKind,
        context: &mut Context<Self>,
    ) {
        if self
            .model_config_input_state_mut(kind)
            .selection_drag
            .take()
            .is_some()
        {
            context.notify();
        }
    }

    /// 返回模型配置输入框当前选中文本。
    fn selected_model_config_input_text(&self, kind: ModelConfigInputKind) -> Option<String> {
        let state = self.model_config_input_state(kind);
        let range = Self::clamp_search_text_range(&state.text, state.selection_range.clone());
        (range.start < range.end).then(|| state.text[range].to_string())
    }

    /// 用给定文本替换模型配置输入框当前选区。
    ///
    /// 边界条件：
    /// - 四个字段都是单行输入，粘贴或 IME 提交中的换行会被移除，避免保存 JSON 时出现不可见跨行配置。
    fn replace_model_config_input_selection(
        &mut self,
        kind: ModelConfigInputKind,
        replacement: &str,
    ) {
        let replacement = Self::sanitize_search_input_text(replacement);
        let state = self.model_config_input_state_mut(kind);
        let range = state.marked_range.take().unwrap_or_else(|| {
            Self::clamp_search_text_range(&state.text, state.selection_range.clone())
        });
        state.text.replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        state.selection_range = cursor..cursor;
        state.clear_layout();
    }

    /// 处理模型配置单行输入框的基础编辑按键。
    ///
    /// 业务意图：
    /// - 模型设置页字段需要支持复制、粘贴、剪切、全选、删除和方向键，普通字符输入继续交给平台 IME 回调。
    fn handle_model_config_input_key_down(
        &mut self,
        kind: ModelConfigInputKind,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_paste_keystroke(&event.keystroke) {
            if let Some(text) = context.read_from_clipboard().and_then(|item| item.text()) {
                self.replace_model_config_input_selection(kind, &text);
                self.model_config.model_test_status = ModelTestStatus::Idle;
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if Self::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_model_config_input_text(kind) {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return;
        }

        if Self::is_cut_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_model_config_input_text(kind) {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                self.replace_model_config_input_selection(kind, "");
                self.model_config.model_test_status = ModelTestStatus::Idle;
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if Self::is_select_all_keystroke(&event.keystroke) {
            let state = self.model_config_input_state_mut(kind);
            state.marked_range = None;
            state.selection_range = 0..state.text.len();
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return;
        }

        match event.keystroke.key.as_str() {
            "left" => {
                let state = self.model_config_input_state_mut(kind);
                state.marked_range = None;
                if event.keystroke.modifiers.shift {
                    state.selection_range.end =
                        Self::previous_search_text_boundary(&state.text, state.selection_range.end);
                    state.selection_range =
                        Self::clamp_search_text_range(&state.text, state.selection_range.clone());
                } else if state.selection_range.start != state.selection_range.end {
                    state.selection_range =
                        state.selection_range.start..state.selection_range.start;
                } else {
                    let cursor =
                        Self::previous_search_text_boundary(&state.text, state.selection_range.end);
                    state.selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "right" => {
                let state = self.model_config_input_state_mut(kind);
                state.marked_range = None;
                if event.keystroke.modifiers.shift {
                    state.selection_range.end =
                        Self::next_search_text_boundary(&state.text, state.selection_range.end);
                    state.selection_range =
                        Self::clamp_search_text_range(&state.text, state.selection_range.clone());
                } else if state.selection_range.start != state.selection_range.end {
                    state.selection_range = state.selection_range.end..state.selection_range.end;
                } else {
                    let cursor =
                        Self::next_search_text_boundary(&state.text, state.selection_range.end);
                    state.selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "up" => {
                let state = self.model_config_input_state_mut(kind);
                state.marked_range = None;
                state.selection_range = 0..0;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "down" => {
                let state = self.model_config_input_state_mut(kind);
                state.marked_range = None;
                let cursor = state.text.len();
                state.selection_range = cursor..cursor;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "backspace" => {
                let should_replace_selection = {
                    let state = self.model_config_input_state(kind);
                    state.selection_range.start != state.selection_range.end
                        || state.marked_range.is_some()
                };
                if should_replace_selection {
                    self.replace_model_config_input_selection(kind, "");
                } else {
                    let state = self.model_config_input_state_mut(kind);
                    if let Some((previous_index, _)) = state.text[..state.selection_range.end]
                        .char_indices()
                        .next_back()
                    {
                        let cursor = state.selection_range.end;
                        state.text.replace_range(previous_index..cursor, "");
                        state.selection_range = previous_index..previous_index;
                        state.marked_range = None;
                        state.clear_layout();
                    }
                }
                self.model_config.model_test_status = ModelTestStatus::Idle;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "delete" => {
                let should_replace_selection = {
                    let state = self.model_config_input_state(kind);
                    state.selection_range.start != state.selection_range.end
                        || state.marked_range.is_some()
                };
                if should_replace_selection {
                    self.replace_model_config_input_selection(kind, "");
                } else {
                    let state = self.model_config_input_state_mut(kind);
                    if let Some((next_index, next_character)) = state.text
                        [state.selection_range.end..]
                        .char_indices()
                        .next()
                    {
                        let start = state.selection_range.end + next_index;
                        let end = start + next_character.len_utf8();
                        state.text.replace_range(start..end, "");
                        state.selection_range = start..start;
                        state.marked_range = None;
                        state.clear_layout();
                    }
                }
                self.model_config.model_test_status = ModelTestStatus::Idle;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "enter" => {
                context.stop_propagation();
            }
            "escape" => {}
            _ => {}
        }
    }

    /// 清空模型配置表单，进入新增未保存状态。
    fn clear_model_config_form(&mut self) {
        self.model_config.model_config_selected_profile_id = None;
        self.model_config.model_config_form_profile_id = None;
        self.model_config
            .model_config_name_input
            .set_text(String::new());
        self.model_config
            .model_config_base_url_input
            .set_text(String::new());
        self.model_config
            .model_config_api_key_input
            .set_text(String::new());
        self.model_config
            .model_config_model_input
            .set_text(String::new());
        self.model_config.model_config_api_key_visible = false;
        self.model_config.model_test_status = ModelTestStatus::Idle;
    }

    /// 把已保存模型配置加载到表单。
    fn load_model_profile_into_form(&mut self, profile: &ModelProfile) {
        self.model_config.model_config_selected_profile_id = Some(profile.id.clone());
        self.model_config.model_config_form_profile_id = Some(profile.id.clone());
        self.model_config
            .model_config_name_input
            .set_text(profile.name.clone());
        self.model_config
            .model_config_base_url_input
            .set_text(profile.base_url.clone());
        self.model_config
            .model_config_api_key_input
            .set_text(profile.api_key.clone());
        self.model_config
            .model_config_model_input
            .set_text(profile.model.clone());
        self.model_config.model_config_api_key_visible = false;
        self.model_config.model_test_status = ModelTestStatus::Idle;
    }

    /// 进入新增模型配置状态。
    fn begin_new_model_profile(&mut self, context: &mut Context<Self>) {
        self.clear_model_config_form();
        context.notify();
    }

    /// 选择已保存模型配置。
    fn select_model_profile(&mut self, profile_id: &str, context: &mut Context<Self>) {
        let Some(profile) = self
            .model_config
            .model_config_profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
        else {
            return;
        };
        self.load_model_profile_into_form(&profile);
        context.notify();
    }

    /// 返回当前表单字段构造的模型配置。
    fn model_profile_from_form(&self, id: String) -> Result<ModelProfile, String> {
        let name = self
            .model_config
            .model_config_name_input
            .text
            .trim()
            .to_string();
        let base_url = self
            .model_config
            .model_config_base_url_input
            .text
            .trim()
            .to_string();
        let api_key = self
            .model_config
            .model_config_api_key_input
            .text
            .trim()
            .to_string();
        let model = self
            .model_config
            .model_config_model_input
            .text
            .trim()
            .to_string();
        validate_model_profile_fields(&name, &base_url, &model)?;
        Ok(ModelProfile {
            id,
            name,
            base_url,
            api_key,
            model,
        })
    }

    /// 生成新的模型配置 ID。
    ///
    /// 边界条件：
    /// - 使用系统时间纳秒作为主干，若极端情况下撞到已有 ID，则追加序号直到唯一。
    fn new_model_profile_id(&self) -> String {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let mut suffix = 0usize;
        loop {
            let candidate = if suffix == 0 {
                format!("model-profile-{seed}")
            } else {
                format!("model-profile-{seed}-{suffix}")
            };
            if !self
                .model_config
                .model_config_profiles
                .iter()
                .any(|profile| profile.id == candidate)
            {
                return candidate;
            }
            suffix += 1;
        }
    }

    /// 保存当前模型配置表单。
    ///
    /// 业务意图：
    /// - 已保存配置走覆盖更新，新增配置分配稳定 ID 后追加到列表；保存成功后同步落盘并选中新配置。
    fn save_current_model_profile(&mut self, context: &mut Context<Self>) {
        let id = self
            .model_config
            .model_config_form_profile_id
            .clone()
            .unwrap_or_else(|| self.new_model_profile_id());
        let profile = match self.model_profile_from_form(id) {
            Ok(profile) => profile,
            Err(message) => {
                self.model_config.model_test_status = ModelTestStatus::Failed(message);
                context.notify();
                return;
            }
        };

        if let Some(existing) = self
            .model_config
            .model_config_profiles
            .iter_mut()
            .find(|existing| existing.id == profile.id)
        {
            *existing = profile.clone();
        } else {
            self.model_config
                .model_config_profiles
                .push(profile.clone());
        }
        self.model_config.model_config_form_profile_id = Some(profile.id.clone());
        self.model_config.model_config_selected_profile_id = Some(profile.id.clone());
        self.model_config.model_test_status =
            ModelTestStatus::Success("模型配置已保存".to_string());
        save_model_configs_preference(&self.model_configs_snapshot());
        context.notify();
    }

    /// 删除当前选中的模型配置。
    ///
    /// 边界条件：
    /// - 未保存的新配置没有 ID，删除时只清空表单。
    /// - 删除默认配置必须同步清空默认 ID，避免后续功能引用悬空配置。
    fn delete_current_model_profile(&mut self, context: &mut Context<Self>) {
        let Some(profile_id) = self.model_config.model_config_form_profile_id.clone() else {
            self.clear_model_config_form();
            context.notify();
            return;
        };
        self.model_config
            .model_config_profiles
            .retain(|profile| profile.id != profile_id);
        if self.model_config.model_config_default_profile_id.as_deref() == Some(profile_id.as_str())
        {
            self.model_config.model_config_default_profile_id = None;
        }

        let next_profile = self.model_config.model_config_profiles.first().cloned();
        if let Some(profile) = next_profile {
            self.load_model_profile_into_form(&profile);
            self.model_config.model_test_status =
                ModelTestStatus::Success("模型配置已删除".to_string());
        } else {
            self.clear_model_config_form();
            self.model_config.model_test_status =
                ModelTestStatus::Success("模型配置已删除".to_string());
        }
        save_model_configs_preference(&self.model_configs_snapshot());
        context.notify();
    }

    /// 将当前已保存配置设为默认模型。
    fn set_current_model_profile_default(&mut self, context: &mut Context<Self>) {
        let Some(profile_id) = self.model_config.model_config_form_profile_id.clone() else {
            self.model_config.model_test_status =
                ModelTestStatus::Failed("请先保存模型配置，再设为默认".to_string());
            context.notify();
            return;
        };
        if !self
            .model_config
            .model_config_profiles
            .iter()
            .any(|profile| profile.id == profile_id)
        {
            self.model_config.model_test_status =
                ModelTestStatus::Failed("默认模型必须指向已保存配置".to_string());
            context.notify();
            return;
        }
        self.model_config.model_config_default_profile_id = Some(profile_id);
        self.model_config.model_test_status =
            ModelTestStatus::Success("已设为默认模型".to_string());
        save_model_configs_preference(&self.model_configs_snapshot());
        context.notify();
    }

    /// 切换 API Key 明文/掩码显示。
    fn toggle_model_api_key_visibility(&mut self, context: &mut Context<Self>) {
        self.model_config.model_config_api_key_visible =
            !self.model_config.model_config_api_key_visible;
        self.model_config.model_config_api_key_input.clear_layout();
        context.notify();
    }

    /// 使用当前表单值测试 OpenAI 兼容模型接口。
    ///
    /// 业务意图：
    /// - 测试按钮使用未保存表单值，便于用户粘贴后先验证再决定是否保存。
    /// - 请求在后台执行，完成时用任务 ID 判断是否仍是最新测试，避免旧结果覆盖新表单状态。
    fn start_model_profile_test(&mut self, context: &mut Context<Self>) {
        let id = self
            .model_config
            .model_config_form_profile_id
            .clone()
            .unwrap_or_else(|| "unsaved-model-profile".to_string());
        let profile = match self.model_profile_from_form(id) {
            Ok(profile) => profile,
            Err(message) => {
                self.model_config.model_test_status = ModelTestStatus::Failed(message);
                context.notify();
                return;
            }
        };
        let job_id = self.model_config.next_model_test_job_id;
        self.model_config.next_model_test_job_id =
            self.model_config.next_model_test_job_id.saturating_add(1);
        self.model_config.model_test_status = ModelTestStatus::Testing { job_id };
        context.notify();

        context
            .spawn(async move |view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move { test_openai_compatible_model(profile) })
                    .await;
                view.update(app, |view, context| {
                    if view.model_config.model_test_status == (ModelTestStatus::Testing { job_id })
                    {
                        view.model_config.model_test_status = match result {
                            Ok(message) => ModelTestStatus::Success(message),
                            Err(message) => ModelTestStatus::Failed(message),
                        };
                        context.notify();
                    }
                })
                .ok();
            })
            .detach();
    }

    /// 读取快搜关键字输入区当前文本、选择范围和组合文本范围的快照。
    fn quick_search_keywords_text_snapshot(&self) -> (String, Range<usize>, Option<Range<usize>>) {
        (
            self.settings.quick_search_keywords_input.text.clone(),
            Self::clamp_search_text_range(
                &self.settings.quick_search_keywords_input.text,
                self.settings
                    .quick_search_keywords_input
                    .selection_range
                    .clone(),
            ),
            self.settings
                .quick_search_keywords_input
                .marked_range
                .clone(),
        )
    }

    /// 保存快搜关键字输入区最近一次单行排版结果。
    fn store_quick_search_keywords_layout(&mut self, line: ShapedLine, bounds: Bounds<Pixels>) {
        self.settings.quick_search_keywords_last_layout = Some(line);
        self.settings.quick_search_keywords_last_bounds = Some(bounds);
    }

    /// 根据鼠标窗口坐标返回快搜关键字输入区中的 UTF-8 字节下标。
    fn quick_search_keywords_index_for_point(&self, position: Point<Pixels>) -> usize {
        let text = &self.settings.quick_search_keywords_input.text;
        let (Some(layout), Some(bounds)) = (
            self.settings.quick_search_keywords_last_layout.as_ref(),
            self.settings.quick_search_keywords_last_bounds.as_ref(),
        ) else {
            return text.len();
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return text.len();
        }
        layout
            .closest_index_for_x(position.x - bounds.left())
            .min(text.len())
    }

    /// 开始快搜关键字输入区的鼠标选择。
    fn start_quick_search_keywords_mouse_selection(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let index = self.quick_search_keywords_index_for_point(event.position);
        self.settings.quick_search_keywords_input.marked_range = None;
        match event.click_count {
            0 | 1 => {
                if event.modifiers.shift {
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .end = index;
                    self.settings.quick_search_keywords_input.selection_range =
                        Self::clamp_search_text_range(
                            &self.settings.quick_search_keywords_input.text,
                            self.settings
                                .quick_search_keywords_input
                                .selection_range
                                .clone(),
                        );
                } else {
                    self.settings.quick_search_keywords_input.selection_range = index..index;
                }
                self.settings.quick_search_keywords_selection_drag = Some(
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .start,
                );
            }
            2 => {
                self.settings.quick_search_keywords_input.selection_range =
                    Self::search_text_word_range_for_index(
                        &self.settings.quick_search_keywords_input.text,
                        index,
                    );
                self.settings.quick_search_keywords_selection_drag = None;
            }
            _ => {
                self.settings.quick_search_keywords_input.selection_range =
                    0..self.settings.quick_search_keywords_input.text.len();
                self.settings.quick_search_keywords_selection_drag = None;
            }
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 鼠标拖拽时更新快搜关键字输入区选区终点。
    fn update_quick_search_keywords_mouse_selection(
        &mut self,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) {
        let Some(anchor) = self.settings.quick_search_keywords_selection_drag else {
            return;
        };
        let index = self.quick_search_keywords_index_for_point(position);
        self.settings.quick_search_keywords_input.marked_range = None;
        self.settings.quick_search_keywords_input.selection_range = Self::clamp_search_text_range(
            &self.settings.quick_search_keywords_input.text,
            anchor..index,
        );
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 结束快搜关键字输入区鼠标拖拽选择。
    fn finish_quick_search_keywords_mouse_selection(&mut self, context: &mut Context<Self>) {
        if self
            .settings
            .quick_search_keywords_selection_drag
            .take()
            .is_some()
        {
            context.notify();
        }
    }

    /// 读取线程日志分析过滤输入区当前文本、选择范围和组合文本范围的快照。
    ///
    /// 业务意图：
    /// - 设置窗口的多行输入元素在绘制阶段只持有 `MainView` 实体，需要通过只读快照拿到稳定文本状态。
    /// - 快照使用规范化后的 LF 文本，确保绘制行数、鼠标命中和后续过滤规则拆分一致。
    fn thread_analysis_filter_text_snapshot(&self) -> (String, Range<usize>, Option<Range<usize>>) {
        (
            self.settings.thread_analysis_filter_text.clone(),
            Self::clamp_search_text_range(
                &self.settings.thread_analysis_filter_text,
                self.settings.thread_analysis_filter_selection_range.clone(),
            ),
            self.settings.thread_analysis_filter_marked_range.clone(),
        )
    }

    /// 保存线程日志分析过滤输入区最近一次多行排版结果。
    ///
    /// 业务意图：
    /// - 鼠标点击和拖拽需要用上一帧真实字形位置换算文本下标；该缓存只服务当前会话，不参与持久化。
    fn store_thread_analysis_filter_text_layouts(
        &mut self,
        layouts: Vec<ThreadAnalysisFilterLineLayout>,
        bounds: Bounds<Pixels>,
    ) {
        self.settings.thread_analysis_filter_last_layouts = layouts;
        self.settings.thread_analysis_filter_last_bounds = Some(bounds);
    }

    /// 返回线程日志分析过滤文本的可视行范围。
    ///
    /// 业务意图：
    /// - 输入区按原始换行展示堆栈；空行也必须占一行，因为空行同时用于分隔多条过滤规则。
    /// - 返回范围不包含换行符本身，便于每行单独排版和命中。
    fn thread_analysis_filter_line_ranges(text: &str) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        let mut start = 0usize;
        for (index, character) in text.char_indices() {
            if character == '\n' {
                ranges.push(start..index);
                start = index + character.len_utf8();
            }
        }
        ranges.push(start..text.len());
        ranges
    }

    /// 返回线程日志分析过滤输入区当前内容需要的可视行数。
    fn thread_analysis_filter_visual_line_count(&self) -> usize {
        Self::thread_analysis_filter_line_ranges(&self.settings.thread_analysis_filter_text)
            .len()
            .max(1)
    }

    /// 开始线程日志分析过滤输入区的鼠标选择。
    ///
    /// 业务意图：
    /// - 单击定位光标，Shift+单击扩展选择，双击选中连续非空白片段，三连击全选，保持和搜索输入框一致的基础文本习惯。
    fn start_thread_analysis_filter_mouse_selection(
        &mut self,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let index = self.thread_analysis_filter_index_for_point(event.position);
        self.settings.thread_analysis_filter_marked_range = None;
        match event.click_count {
            0 | 1 => {
                if event.modifiers.shift {
                    self.settings.thread_analysis_filter_selection_range.end = index;
                    self.settings.thread_analysis_filter_selection_range =
                        Self::clamp_search_text_range(
                            &self.settings.thread_analysis_filter_text,
                            self.settings.thread_analysis_filter_selection_range.clone(),
                        );
                } else {
                    self.settings.thread_analysis_filter_selection_range = index..index;
                }
                self.settings.thread_analysis_filter_selection_drag =
                    Some(self.settings.thread_analysis_filter_selection_range.start);
            }
            2 => {
                self.settings.thread_analysis_filter_selection_range =
                    Self::search_text_word_range_for_index(
                        &self.settings.thread_analysis_filter_text,
                        index,
                    );
                self.settings.thread_analysis_filter_selection_drag = None;
            }
            _ => {
                self.settings.thread_analysis_filter_selection_range =
                    0..self.settings.thread_analysis_filter_text.len();
                self.settings.thread_analysis_filter_selection_drag = None;
            }
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 鼠标拖拽时更新线程日志分析过滤输入区选区终点。
    fn update_thread_analysis_filter_mouse_selection(
        &mut self,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) {
        let Some(anchor) = self.settings.thread_analysis_filter_selection_drag else {
            return;
        };
        let index = self.thread_analysis_filter_index_for_point(position);
        self.settings.thread_analysis_filter_marked_range = None;
        self.settings.thread_analysis_filter_selection_range = Self::clamp_search_text_range(
            &self.settings.thread_analysis_filter_text,
            anchor..index,
        );
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 结束线程日志分析过滤输入区鼠标拖拽选择。
    fn finish_thread_analysis_filter_mouse_selection(&mut self, context: &mut Context<Self>) {
        if self
            .settings
            .thread_analysis_filter_selection_drag
            .take()
            .is_some()
        {
            context.notify();
        }
    }

    /// 根据鼠标窗口坐标返回线程日志分析过滤输入区中的 UTF-8 字节下标。
    ///
    /// 边界条件：
    /// - 首帧尚未完成排版时回退到文本末尾，避免点击空布局导致越界。
    /// - 点击在整体输入区上方或下方时，分别夹到开头或末尾，符合多行文本框的常见行为。
    fn thread_analysis_filter_index_for_point(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.settings.thread_analysis_filter_last_bounds.as_ref() else {
            return self.settings.thread_analysis_filter_text.len();
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.settings.thread_analysis_filter_text.len();
        }
        for layout in &self.settings.thread_analysis_filter_last_layouts {
            if position.y >= layout.bounds.top() && position.y <= layout.bounds.bottom() {
                let local_index = layout
                    .line
                    .closest_index_for_x(position.x - layout.bounds.left())
                    .min(
                        layout
                            .byte_range
                            .end
                            .saturating_sub(layout.byte_range.start),
                    );
                return layout.byte_range.start + local_index;
            }
        }
        self.settings.thread_analysis_filter_text.len()
    }

    /// 进入快搜关键字编辑状态。
    ///
    /// 业务意图：
    /// - 快搜配置默认只读展示，用户明确点击编辑后才允许修改，避免误触键盘或粘贴导致常用关键字被改写。
    /// - 光标放到文本末尾，便于用户继续追加英文逗号和新关键字。
    fn begin_quick_search_keywords_edit(&mut self, context: &mut Context<Self>) {
        if self.settings.quick_search_keywords_is_editing {
            return;
        }
        self.settings.quick_search_keywords_saved_text_before_edit =
            Some(self.settings.quick_search_keywords_input.text.clone());
        self.settings.quick_search_keywords_is_editing = true;
        let cursor = self.settings.quick_search_keywords_input.text.len();
        self.settings.quick_search_keywords_input.selection_range = cursor..cursor;
        self.settings.quick_search_keywords_input.marked_range = None;
        self.settings.quick_search_keywords_selection_drag = None;
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 保存快搜关键字配置并退出编辑状态。
    ///
    /// 业务意图：
    /// - 点击保存后才把单行关键字配置写入配置目录，保证快搜行为只受明确保存过的规则影响。
    /// - 保存前移除换行但保留中文逗号等其它字符，因为用户已确认只有英文逗号具备分隔语义。
    fn save_quick_search_keywords_edit(&mut self, context: &mut Context<Self>) {
        let normalized =
            normalize_quick_search_keywords_text(&self.settings.quick_search_keywords_input.text);
        if normalized != self.settings.quick_search_keywords_input.text {
            self.settings.quick_search_keywords_input.text = normalized;
        }
        self.settings.quick_search_keywords_input.selection_range = Self::clamp_search_text_range(
            &self.settings.quick_search_keywords_input.text,
            self.settings
                .quick_search_keywords_input
                .selection_range
                .clone(),
        );
        self.settings.quick_search_keywords_input.marked_range = None;
        self.settings.quick_search_keywords_selection_drag = None;
        self.settings.quick_search_keywords_is_editing = false;
        self.settings.quick_search_keywords_saved_text_before_edit = None;
        save_quick_search_keywords_preference(&self.settings.quick_search_keywords_input.text);
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 放弃快搜关键字草稿并恢复只读展示。
    ///
    /// 业务意图：
    /// - 设置窗口关闭时如果用户没有点击保存，本次编辑应视为未完成草稿，不能悄悄改变下一次快搜行为。
    fn discard_quick_search_keywords_edit(&mut self, context: &mut Context<Self>) {
        if let Some(saved_text) = self
            .settings
            .quick_search_keywords_saved_text_before_edit
            .take()
        {
            self.settings.quick_search_keywords_input.text = saved_text;
        }
        self.settings.quick_search_keywords_is_editing = false;
        self.settings.quick_search_keywords_input.selection_range = Self::clamp_search_text_range(
            &self.settings.quick_search_keywords_input.text,
            self.settings
                .quick_search_keywords_input
                .selection_range
                .clone(),
        );
        self.settings.quick_search_keywords_input.marked_range = None;
        self.settings.quick_search_keywords_selection_drag = None;
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 进入线程日志分析过滤编辑状态。
    ///
    /// 业务意图：
    /// - 设置页默认只读展示过滤堆栈，用户明确点击编辑后才把多行文本框切换为可写，降低误粘贴和输入法误提交风险。
    /// - 光标放到文本末尾，便于用户继续追加新的过滤堆栈；已有选择和组合文本会被清理，避免从只读态遗留不可见编辑上下文。
    fn begin_thread_analysis_filter_edit(&mut self, context: &mut Context<Self>) {
        if self.settings.thread_analysis_filter_is_editing {
            return;
        }
        self.settings.thread_analysis_filter_saved_text_before_edit =
            Some(self.settings.thread_analysis_filter_text.clone());
        self.settings.thread_analysis_filter_is_editing = true;
        let cursor = self.settings.thread_analysis_filter_text.len();
        self.settings.thread_analysis_filter_selection_range = cursor..cursor;
        self.settings.thread_analysis_filter_marked_range = None;
        self.settings.thread_analysis_filter_selection_drag = None;
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 保存线程日志分析过滤配置并退出编辑状态。
    ///
    /// 业务意图：
    /// - 用户点击保存时才把当前多行文本写入配置文件，符合“默认只读、显式编辑、显式保存”的设置语义。
    /// - 保存前再次规范化换行，确保从 Windows 粘贴的 CRLF 不会影响后续规则拆分和线程堆栈连续匹配。
    fn save_thread_analysis_filter_edit(&mut self, context: &mut Context<Self>) {
        let normalized =
            normalize_thread_analysis_filter_text(&self.settings.thread_analysis_filter_text);
        if normalized != self.settings.thread_analysis_filter_text {
            self.settings.thread_analysis_filter_text = normalized;
        }
        self.settings.thread_analysis_filter_selection_range = Self::clamp_search_text_range(
            &self.settings.thread_analysis_filter_text,
            self.settings.thread_analysis_filter_selection_range.clone(),
        );
        self.settings.thread_analysis_filter_marked_range = None;
        self.settings.thread_analysis_filter_selection_drag = None;
        self.settings.thread_analysis_filter_is_editing = false;
        self.settings.thread_analysis_filter_saved_text_before_edit = None;
        save_thread_analysis_filter_preference(&self.settings.thread_analysis_filter_text);
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 放弃线程日志分析过滤草稿并恢复只读展示。
    ///
    /// 业务意图：
    /// - 设置窗口关闭时如果用户没有点击保存，本次编辑应视为未完成草稿，不能悄悄改变线程分析行为。
    /// - 恢复进入编辑前的快照，同时清理输入法组合文本和拖拽选择，避免下次打开设置窗口残留编辑态。
    fn discard_thread_analysis_filter_edit(&mut self, context: &mut Context<Self>) {
        if let Some(saved_text) = self
            .settings
            .thread_analysis_filter_saved_text_before_edit
            .take()
        {
            self.settings.thread_analysis_filter_text = saved_text;
        }
        self.settings.thread_analysis_filter_is_editing = false;
        self.settings.thread_analysis_filter_selection_range = Self::clamp_search_text_range(
            &self.settings.thread_analysis_filter_text,
            self.settings.thread_analysis_filter_selection_range.clone(),
        );
        self.settings.thread_analysis_filter_marked_range = None;
        self.settings.thread_analysis_filter_selection_drag = None;
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 保存搜索输入框最近一次 GPUI 文本排版结果。
    ///
    /// 业务意图：
    /// - 鼠标点击和拖拽必须根据真实字形宽度转换成文本下标；缓存 `ShapedLine` 后可以复用 GPUI 的命中算法。
    /// - 分开保存关键字和目录输入框，避免两个输入框在同一帧绘制后互相覆盖命中数据。
    fn store_search_text_layout(
        &mut self,
        input_kind: SearchTextInputKind,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
    ) {
        match input_kind {
            SearchTextInputKind::Query => {
                self.search.search_query_last_layout = Some(line);
                self.search.search_query_last_bounds = Some(bounds);
            }
            SearchTextInputKind::DirectoryTarget => {
                self.search.search_directory_last_layout = Some(line);
                self.search.search_directory_last_bounds = Some(bounds);
            }
        }
    }

    /// 开始搜索输入框的鼠标选择。
    ///
    /// 业务意图：
    /// - 单击定位光标，Shift+单击扩展当前选择，双击选择当前词，三连击选中整段输入。
    /// - 选择逻辑写在 `MainView` 中，保证搜索关键字和目录目标输入框行为一致。
    fn start_search_text_mouse_selection(
        &mut self,
        input_kind: SearchTextInputKind,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let index = self.search_text_index_for_point(input_kind, event.position);
        let Some(dialog) = self.search.search_dialog.as_mut() else {
            return;
        };
        let (text, selection_range, marked_range) = Self::search_text_state_mut(dialog, input_kind);
        *marked_range = None;
        match event.click_count {
            0 | 1 => {
                if event.modifiers.shift {
                    selection_range.end = index;
                    *selection_range = Self::clamp_search_text_range(text, selection_range.clone());
                } else {
                    *selection_range = index..index;
                }
                self.search.search_text_selection_drag = Some((input_kind, selection_range.start));
            }
            2 => {
                *selection_range = Self::search_text_word_range_for_index(text, index);
                self.search.search_text_selection_drag = None;
            }
            _ => {
                *selection_range = 0..text.len();
                self.search.search_text_selection_drag = None;
            }
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 鼠标拖拽时更新搜索输入框选区终点。
    fn update_search_text_mouse_selection(
        &mut self,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) {
        let Some((input_kind, anchor)) = self.search.search_text_selection_drag else {
            return;
        };
        let index = self.search_text_index_for_point(input_kind, position);
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            let (text, selection_range, marked_range) =
                Self::search_text_state_mut(dialog, input_kind);
            *marked_range = None;
            *selection_range = Self::clamp_search_text_range(text, anchor..index);
            self.touch_search_text_cursor_activity();
            context.notify();
        }
    }

    /// 结束搜索输入框鼠标拖拽选择。
    fn finish_search_text_mouse_selection(&mut self, context: &mut Context<Self>) {
        if self.search.search_text_selection_drag.take().is_some() {
            context.notify();
        }
    }

    /// 根据鼠标窗口坐标返回搜索输入框中的 UTF-8 字节下标。
    ///
    /// 边界条件：
    /// - 如果输入框尚未完成首次绘制，没有可用字形布局，则回退到文本末尾，避免点击导致越界。
    /// - 如果点击发生在文本区域上下之外，分别夹到开头和末尾，符合单行系统输入框的常见行为。
    fn search_text_index_for_point(
        &self,
        input_kind: SearchTextInputKind,
        position: Point<Pixels>,
    ) -> usize {
        let Some((text, _, _)) = self.search_text_snapshot(input_kind) else {
            return 0;
        };
        let (layout, bounds) = match input_kind {
            SearchTextInputKind::Query => (
                self.search.search_query_last_layout.as_ref(),
                self.search.search_query_last_bounds.as_ref(),
            ),
            SearchTextInputKind::DirectoryTarget => (
                self.search.search_directory_last_layout.as_ref(),
                self.search.search_directory_last_bounds.as_ref(),
            ),
        };
        let (Some(layout), Some(bounds)) = (layout, bounds) else {
            return text.len();
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return text.len();
        }
        layout
            .closest_index_for_x(position.x - bounds.left())
            .min(text.len())
    }

    /// 返回双击时应选择的输入词范围。
    ///
    /// 业务意图：
    /// - 搜索关键字和目录路径中都可能出现中文、英文、数字和路径分隔符；双击选择连续非空白片段更符合搜索场景。
    /// - 如果点击在空白上，则选择连续空白，和常见文本输入框行为保持一致。
    fn search_text_word_range_for_index(text: &str, index: usize) -> Range<usize> {
        if text.is_empty() {
            return 0..0;
        }
        let index = Self::clamp_search_text_range(text, index..index).start;
        let current = text[index..]
            .chars()
            .next()
            .or_else(|| text[..index].chars().next_back());
        let Some(current) = current else {
            return 0..0;
        };
        let select_whitespace = current.is_whitespace();
        let mut start = 0usize;
        for (byte_index, character) in text[..index].char_indices().rev() {
            if character.is_whitespace() != select_whitespace {
                start = byte_index + character.len_utf8();
                break;
            }
        }
        let mut end = text.len();
        for (relative_index, character) in text[index..].char_indices() {
            if character.is_whitespace() != select_whitespace {
                end = index + relative_index;
                break;
            }
        }
        start..end
    }

    /// 标记搜索输入框光标刚发生用户活动。
    ///
    /// 业务意图：
    /// - 输入、点击定位、拖拽和方向键移动都会改变用户对光标位置的关注点；这些动作之后光标需要保持常亮 1 秒。
    /// - 集中更新时间戳，避免键盘、鼠标和 IME 提交路径出现不同的闪烁节奏。
    fn touch_search_text_cursor_activity(&mut self) {
        self.search.search_text_cursor_last_activity = Instant::now();
    }

    /// 判断当前输入框光标在本帧是否应显示。
    ///
    /// 业务意图：
    /// - 搜索输入框现在由 GPUI 文本元素绘制，不再使用普通 `div().with_animation(...)` 光标。
    /// - 输入或移动发生后的极短时间内保持可见，随后立即按 500ms 亮、500ms 灭的节奏闪烁。
    ///
    /// 边界条件：
    /// - `Instant` 是单调时间，适合处理系统时间调整、时区变化或休眠恢复后的相对时间判断。
    fn search_text_cursor_visible(&self) -> bool {
        let elapsed = self.search.search_text_cursor_last_activity.elapsed();
        Self::search_text_cursor_visible_for_elapsed(elapsed)
    }

    /// 根据距离最近一次光标活动的时间计算光标可见性。
    ///
    /// 业务意图：
    /// - 将时间规则拆成纯函数，便于单元测试锁定“活动后 1 秒常亮，之后闪烁”的产品行为。
    fn search_text_cursor_visible_for_elapsed(elapsed: Duration) -> bool {
        if elapsed < Duration::from_millis(120) {
            return true;
        }
        elapsed.as_millis() % 1000 < 500
    }

    /// 处理搜索对话框中任一文本输入框的基础编辑按键。
    ///
    /// 业务意图：
    /// - 查询词和目录目标都只需要单行文本能力，复用同一套退格、删除和组合文本清理逻辑可避免两处行为不一致。
    /// - 普通字符输入交给 `EntityInputHandler`，这里不处理 `key_char`，从而保留中文 IME 的平台提交路径。
    fn handle_search_text_key_down(
        &mut self,
        input_kind: SearchTextInputKind,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_paste_keystroke(&event.keystroke) {
            let clipboard_text = context
                .read_from_clipboard()
                .and_then(|item| item.text())
                .map(|text| Self::sanitize_search_input_text(&text));
            if let Some(text) = clipboard_text {
                let text_is_not_empty = !text.is_empty();
                if text_is_not_empty {
                    if let Some(dialog) = self.search.search_dialog.as_mut() {
                        if input_kind == SearchTextInputKind::Query {
                            dialog.query_history_menu_open = false;
                        }
                        let (input_text, selection_range, marked_range) =
                            Self::search_text_state_mut(dialog, input_kind);
                        Self::replace_search_text_selection(
                            input_text,
                            selection_range,
                            marked_range,
                            &text,
                        );
                        if input_kind == SearchTextInputKind::Query {
                            dialog.current_file_match_count = None;
                        }
                    }
                    self.touch_search_text_cursor_activity();
                    context.stop_propagation();
                    context.notify();
                    return;
                }
            }
            context.stop_propagation();
            return;
        }

        let Some(dialog) = self.search.search_dialog.as_mut() else {
            return;
        };
        if input_kind == SearchTextInputKind::Query {
            dialog.query_history_menu_open = false;
        }
        let (text, selection_range, marked_range) = Self::search_text_state_mut(dialog, input_kind);

        if Self::is_copy_keystroke(&event.keystroke) {
            if selection_range.start != selection_range.end {
                let range = Self::clamp_search_text_range(text, selection_range.clone());
                if range.start < range.end {
                    context.write_to_clipboard(ClipboardItem::new_string(text[range].to_string()));
                }
            }
            context.stop_propagation();
            return;
        }

        if Self::is_select_all_keystroke(&event.keystroke) {
            *marked_range = None;
            *selection_range = 0..text.len();
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return;
        }

        match event.keystroke.key.as_str() {
            "left" => {
                *marked_range = None;
                if event.keystroke.modifiers.shift {
                    selection_range.end =
                        Self::previous_search_text_boundary(text, selection_range.end);
                    *selection_range = Self::clamp_search_text_range(text, selection_range.clone());
                } else if selection_range.start != selection_range.end {
                    *selection_range = selection_range.start..selection_range.start;
                } else {
                    let cursor = Self::previous_search_text_boundary(text, selection_range.end);
                    *selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "right" => {
                *marked_range = None;
                if event.keystroke.modifiers.shift {
                    selection_range.end =
                        Self::next_search_text_boundary(text, selection_range.end);
                    *selection_range = Self::clamp_search_text_range(text, selection_range.clone());
                } else if selection_range.start != selection_range.end {
                    *selection_range = selection_range.end..selection_range.end;
                } else {
                    let cursor = Self::next_search_text_boundary(text, selection_range.end);
                    *selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "up" => {
                *marked_range = None;
                *selection_range = 0..0;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "down" => {
                *marked_range = None;
                let cursor = text.len();
                *selection_range = cursor..cursor;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "backspace" => {
                if let Some(range) = marked_range.take().or_else(|| {
                    (selection_range.start != selection_range.end).then(|| selection_range.clone())
                }) {
                    text.replace_range(range.clone(), "");
                    *selection_range = range.start..range.start;
                } else if let Some((previous_index, _)) =
                    text[..selection_range.end].char_indices().next_back()
                {
                    text.replace_range(previous_index..selection_range.end, "");
                    *selection_range = previous_index..previous_index;
                }
                if input_kind == SearchTextInputKind::Query {
                    self.clear_search_current_file_match_count();
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "delete" => {
                if let Some(range) = marked_range.take().or_else(|| {
                    (selection_range.start != selection_range.end).then(|| selection_range.clone())
                }) {
                    text.replace_range(range.clone(), "");
                    *selection_range = range.start..range.start;
                } else if let Some((next_index, next_character)) =
                    text[selection_range.end..].char_indices().next()
                {
                    let start = selection_range.end + next_index;
                    let end = start + next_character.len_utf8();
                    text.replace_range(start..end, "");
                    *selection_range = start..start;
                }
                if input_kind == SearchTextInputKind::Query {
                    self.clear_search_current_file_match_count();
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "enter" | "escape" => {
                // Enter 和 Escape 由全局快捷键处理，保持搜索框只负责文本编辑。
            }
            _ => {}
        }
    }

    /// 处理快搜关键字输入区的基础编辑按键。
    ///
    /// 业务意图：
    /// - 快搜配置是单行英文逗号分隔文本，默认只读；编辑态才允许粘贴、剪切、删除和普通 IME 提交。
    /// - 只读态仍允许复制、全选和方向键移动，方便用户核对当前已保存关键字。
    fn handle_quick_search_keywords_key_down(
        &mut self,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_paste_keystroke(&event.keystroke) {
            if self.settings.quick_search_keywords_is_editing
                && let Some(text) = context.read_from_clipboard().and_then(|item| item.text())
            {
                let replacement = Self::sanitize_search_input_text(&text);
                if !replacement.is_empty() {
                    self.replace_quick_search_keywords_selection(&replacement);
                    self.touch_search_text_cursor_activity();
                    context.notify();
                }
            }
            context.stop_propagation();
            return;
        }

        if Self::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_quick_search_keywords_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return;
        }

        if Self::is_cut_keystroke(&event.keystroke) {
            if self.settings.quick_search_keywords_is_editing
                && let Some(text) = self.selected_quick_search_keywords_text()
            {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                self.replace_quick_search_keywords_selection("");
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if Self::is_select_all_keystroke(&event.keystroke) {
            self.settings.quick_search_keywords_input.marked_range = None;
            self.settings.quick_search_keywords_input.selection_range =
                0..self.settings.quick_search_keywords_input.text.len();
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return;
        }

        match event.keystroke.key.as_str() {
            "left" => {
                self.settings.quick_search_keywords_input.marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .end = Self::previous_search_text_boundary(
                        &self.settings.quick_search_keywords_input.text,
                        self.settings
                            .quick_search_keywords_input
                            .selection_range
                            .end,
                    );
                    self.settings.quick_search_keywords_input.selection_range =
                        Self::clamp_search_text_range(
                            &self.settings.quick_search_keywords_input.text,
                            self.settings
                                .quick_search_keywords_input
                                .selection_range
                                .clone(),
                        );
                } else if self
                    .settings
                    .quick_search_keywords_input
                    .selection_range
                    .start
                    != self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end
                {
                    self.settings.quick_search_keywords_input.selection_range = self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .start
                        ..self
                            .settings
                            .quick_search_keywords_input
                            .selection_range
                            .start;
                } else {
                    let cursor = Self::previous_search_text_boundary(
                        &self.settings.quick_search_keywords_input.text,
                        self.settings
                            .quick_search_keywords_input
                            .selection_range
                            .end,
                    );
                    self.settings.quick_search_keywords_input.selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "right" => {
                self.settings.quick_search_keywords_input.marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .end = Self::next_search_text_boundary(
                        &self.settings.quick_search_keywords_input.text,
                        self.settings
                            .quick_search_keywords_input
                            .selection_range
                            .end,
                    );
                    self.settings.quick_search_keywords_input.selection_range =
                        Self::clamp_search_text_range(
                            &self.settings.quick_search_keywords_input.text,
                            self.settings
                                .quick_search_keywords_input
                                .selection_range
                                .clone(),
                        );
                } else if self
                    .settings
                    .quick_search_keywords_input
                    .selection_range
                    .start
                    != self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end
                {
                    self.settings.quick_search_keywords_input.selection_range = self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end
                        ..self
                            .settings
                            .quick_search_keywords_input
                            .selection_range
                            .end;
                } else {
                    let cursor = Self::next_search_text_boundary(
                        &self.settings.quick_search_keywords_input.text,
                        self.settings
                            .quick_search_keywords_input
                            .selection_range
                            .end,
                    );
                    self.settings.quick_search_keywords_input.selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "up" => {
                self.settings.quick_search_keywords_input.marked_range = None;
                self.settings.quick_search_keywords_input.selection_range = 0..0;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "down" => {
                self.settings.quick_search_keywords_input.marked_range = None;
                let cursor = self.settings.quick_search_keywords_input.text.len();
                self.settings.quick_search_keywords_input.selection_range = cursor..cursor;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "backspace" => {
                if !self.settings.quick_search_keywords_is_editing {
                    context.stop_propagation();
                    return;
                }
                if self
                    .settings
                    .quick_search_keywords_input
                    .selection_range
                    .start
                    != self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end
                    || self
                        .settings
                        .quick_search_keywords_input
                        .marked_range
                        .is_some()
                {
                    self.replace_quick_search_keywords_selection("");
                } else if let Some((previous_index, _)) =
                    self.settings.quick_search_keywords_input.text[..self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end]
                        .char_indices()
                        .next_back()
                {
                    let cursor = self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end;
                    self.settings
                        .quick_search_keywords_input
                        .text
                        .replace_range(previous_index..cursor, "");
                    self.settings.quick_search_keywords_input.selection_range =
                        previous_index..previous_index;
                    self.settings.quick_search_keywords_input.marked_range = None;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "delete" => {
                if !self.settings.quick_search_keywords_is_editing {
                    context.stop_propagation();
                    return;
                }
                if self
                    .settings
                    .quick_search_keywords_input
                    .selection_range
                    .start
                    != self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end
                    || self
                        .settings
                        .quick_search_keywords_input
                        .marked_range
                        .is_some()
                {
                    self.replace_quick_search_keywords_selection("");
                } else if let Some((next_index, next_character)) =
                    self.settings.quick_search_keywords_input.text[self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end..]
                        .char_indices()
                        .next()
                {
                    let start = self
                        .settings
                        .quick_search_keywords_input
                        .selection_range
                        .end
                        + next_index;
                    let end = start + next_character.len_utf8();
                    self.settings
                        .quick_search_keywords_input
                        .text
                        .replace_range(start..end, "");
                    self.settings.quick_search_keywords_input.selection_range = start..start;
                    self.settings.quick_search_keywords_input.marked_range = None;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "enter" => {
                context.stop_propagation();
            }
            "escape" => {}
            _ => {}
        }
    }

    /// 返回快搜关键字输入区当前选中文本。
    fn selected_quick_search_keywords_text(&self) -> Option<String> {
        let range = Self::clamp_search_text_range(
            &self.settings.quick_search_keywords_input.text,
            self.settings
                .quick_search_keywords_input
                .selection_range
                .clone(),
        );
        (range.start < range.end)
            .then(|| self.settings.quick_search_keywords_input.text[range].to_string())
    }

    /// 用给定文本替换快搜关键字输入区当前选区。
    fn replace_quick_search_keywords_selection(&mut self, replacement: &str) {
        let replacement = normalize_quick_search_keywords_text(replacement);
        let range = self
            .settings
            .quick_search_keywords_input
            .marked_range
            .take()
            .unwrap_or_else(|| {
                Self::clamp_search_text_range(
                    &self.settings.quick_search_keywords_input.text,
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .clone(),
                )
            });
        self.settings
            .quick_search_keywords_input
            .text
            .replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        self.settings.quick_search_keywords_input.selection_range = cursor..cursor;
    }

    /// 处理线程日志分析过滤多行输入区的基础编辑按键。
    ///
    /// 业务意图：
    /// - 该输入区用于粘贴完整线程堆栈，必须保留换行，并支持复制、剪切、粘贴、删除、全选和回车换行。
    /// - 设置页默认只读展示过滤规则，因此只有编辑态才允许修改文本；只读态仍允许选中和复制，方便用户核对内置规则。
    /// - 普通字符输入交给 `EntityInputHandler`，这里不处理 `key_char`，从而保留中文 IME 的平台提交路径。
    fn handle_thread_analysis_filter_key_down(
        &mut self,
        event: &KeyDownEvent,
        context: &mut Context<Self>,
    ) {
        if Self::is_paste_keystroke(&event.keystroke) {
            if self.settings.thread_analysis_filter_is_editing
                && let Some(text) = context.read_from_clipboard().and_then(|item| item.text())
            {
                let replacement = normalize_thread_analysis_filter_text(&text);
                self.replace_thread_analysis_filter_selection(&replacement);
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
                return;
            }
            context.stop_propagation();
            return;
        }

        if Self::is_copy_keystroke(&event.keystroke) {
            if let Some(text) = self.selected_thread_analysis_filter_text() {
                context.write_to_clipboard(ClipboardItem::new_string(text));
            }
            context.stop_propagation();
            return;
        }

        if Self::is_cut_keystroke(&event.keystroke) {
            if self.settings.thread_analysis_filter_is_editing
                && let Some(text) = self.selected_thread_analysis_filter_text()
            {
                context.write_to_clipboard(ClipboardItem::new_string(text));
                self.replace_thread_analysis_filter_selection("");
                self.touch_search_text_cursor_activity();
                context.notify();
            }
            context.stop_propagation();
            return;
        }

        if Self::is_select_all_keystroke(&event.keystroke) {
            self.settings.thread_analysis_filter_marked_range = None;
            self.settings.thread_analysis_filter_selection_range =
                0..self.settings.thread_analysis_filter_text.len();
            self.touch_search_text_cursor_activity();
            context.stop_propagation();
            context.notify();
            return;
        }

        match event.keystroke.key.as_str() {
            "left" => {
                self.settings.thread_analysis_filter_marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.settings.thread_analysis_filter_selection_range.end =
                        Self::previous_search_text_boundary(
                            &self.settings.thread_analysis_filter_text,
                            self.settings.thread_analysis_filter_selection_range.end,
                        );
                    self.settings.thread_analysis_filter_selection_range =
                        Self::clamp_search_text_range(
                            &self.settings.thread_analysis_filter_text,
                            self.settings.thread_analysis_filter_selection_range.clone(),
                        );
                } else if self.settings.thread_analysis_filter_selection_range.start
                    != self.settings.thread_analysis_filter_selection_range.end
                {
                    self.settings.thread_analysis_filter_selection_range =
                        self.settings.thread_analysis_filter_selection_range.start
                            ..self.settings.thread_analysis_filter_selection_range.start;
                } else {
                    let cursor = Self::previous_search_text_boundary(
                        &self.settings.thread_analysis_filter_text,
                        self.settings.thread_analysis_filter_selection_range.end,
                    );
                    self.settings.thread_analysis_filter_selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "right" => {
                self.settings.thread_analysis_filter_marked_range = None;
                if event.keystroke.modifiers.shift {
                    self.settings.thread_analysis_filter_selection_range.end =
                        Self::next_search_text_boundary(
                            &self.settings.thread_analysis_filter_text,
                            self.settings.thread_analysis_filter_selection_range.end,
                        );
                    self.settings.thread_analysis_filter_selection_range =
                        Self::clamp_search_text_range(
                            &self.settings.thread_analysis_filter_text,
                            self.settings.thread_analysis_filter_selection_range.clone(),
                        );
                } else if self.settings.thread_analysis_filter_selection_range.start
                    != self.settings.thread_analysis_filter_selection_range.end
                {
                    self.settings.thread_analysis_filter_selection_range =
                        self.settings.thread_analysis_filter_selection_range.end
                            ..self.settings.thread_analysis_filter_selection_range.end;
                } else {
                    let cursor = Self::next_search_text_boundary(
                        &self.settings.thread_analysis_filter_text,
                        self.settings.thread_analysis_filter_selection_range.end,
                    );
                    self.settings.thread_analysis_filter_selection_range = cursor..cursor;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "up" => {
                self.settings.thread_analysis_filter_marked_range = None;
                self.settings.thread_analysis_filter_selection_range = 0..0;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "down" => {
                self.settings.thread_analysis_filter_marked_range = None;
                let cursor = self.settings.thread_analysis_filter_text.len();
                self.settings.thread_analysis_filter_selection_range = cursor..cursor;
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "backspace" => {
                if !self.settings.thread_analysis_filter_is_editing {
                    context.stop_propagation();
                    return;
                }
                if self.settings.thread_analysis_filter_selection_range.start
                    != self.settings.thread_analysis_filter_selection_range.end
                    || self.settings.thread_analysis_filter_marked_range.is_some()
                {
                    self.replace_thread_analysis_filter_selection("");
                } else if let Some((previous_index, _)) = self.settings.thread_analysis_filter_text
                    [..self.settings.thread_analysis_filter_selection_range.end]
                    .char_indices()
                    .next_back()
                {
                    let cursor = self.settings.thread_analysis_filter_selection_range.end;
                    self.settings
                        .thread_analysis_filter_text
                        .replace_range(previous_index..cursor, "");
                    self.settings.thread_analysis_filter_selection_range =
                        previous_index..previous_index;
                    self.settings.thread_analysis_filter_marked_range = None;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "delete" => {
                if !self.settings.thread_analysis_filter_is_editing {
                    context.stop_propagation();
                    return;
                }
                if self.settings.thread_analysis_filter_selection_range.start
                    != self.settings.thread_analysis_filter_selection_range.end
                    || self.settings.thread_analysis_filter_marked_range.is_some()
                {
                    self.replace_thread_analysis_filter_selection("");
                } else if let Some((next_index, next_character)) =
                    self.settings.thread_analysis_filter_text
                        [self.settings.thread_analysis_filter_selection_range.end..]
                        .char_indices()
                        .next()
                {
                    let start =
                        self.settings.thread_analysis_filter_selection_range.end + next_index;
                    let end = start + next_character.len_utf8();
                    self.settings
                        .thread_analysis_filter_text
                        .replace_range(start..end, "");
                    self.settings.thread_analysis_filter_selection_range = start..start;
                    self.settings.thread_analysis_filter_marked_range = None;
                }
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "enter" => {
                if !self.settings.thread_analysis_filter_is_editing {
                    context.stop_propagation();
                    return;
                }
                self.replace_thread_analysis_filter_selection("\n");
                self.touch_search_text_cursor_activity();
                context.stop_propagation();
                context.notify();
            }
            "escape" => {}
            _ => {}
        }
    }

    /// 返回线程日志分析过滤输入区当前选中文本。
    fn selected_thread_analysis_filter_text(&self) -> Option<String> {
        let range = Self::clamp_search_text_range(
            &self.settings.thread_analysis_filter_text,
            self.settings.thread_analysis_filter_selection_range.clone(),
        );
        (range.start < range.end)
            .then(|| self.settings.thread_analysis_filter_text[range].to_string())
    }

    /// 用给定文本替换线程日志分析过滤输入区当前选区。
    ///
    /// 业务意图：
    /// - 平台 IME、快捷键粘贴和普通编辑都通过同一函数更新文本、组合范围和光标，保证多行输入状态一致。
    fn replace_thread_analysis_filter_selection(&mut self, replacement: &str) {
        let replacement = normalize_thread_analysis_filter_text(replacement);
        let range = self
            .settings
            .thread_analysis_filter_marked_range
            .take()
            .unwrap_or_else(|| {
                Self::clamp_search_text_range(
                    &self.settings.thread_analysis_filter_text,
                    self.settings.thread_analysis_filter_selection_range.clone(),
                )
            });
        self.settings
            .thread_analysis_filter_text
            .replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        self.settings.thread_analysis_filter_selection_range = cursor..cursor;
    }

    /// 返回指定输入槽位的可变文本、选择范围和组合范围。
    ///
    /// 实现原因：
    /// - Rust 需要在同一分支中同时借用三个字段，封装后可以让按键处理和 IME 回调共享同一套字段选择逻辑。
    fn search_text_state_mut(
        dialog: &mut SearchDialogState,
        input_kind: SearchTextInputKind,
    ) -> (&mut String, &mut Range<usize>, &mut Option<Range<usize>>) {
        match input_kind {
            SearchTextInputKind::Query => (
                &mut dialog.query_input.text,
                &mut dialog.query_input.selection_range,
                &mut dialog.query_input.marked_range,
            ),
            SearchTextInputKind::DirectoryTarget => (
                &mut dialog.directory_input.text,
                &mut dialog.directory_input.selection_range,
                &mut dialog.directory_input.marked_range,
            ),
        }
    }

    /// 返回指定输入槽位的只读文本、选择范围和组合范围。
    fn search_text_state(
        dialog: &SearchDialogState,
        input_kind: SearchTextInputKind,
    ) -> (&str, Range<usize>, Option<Range<usize>>) {
        match input_kind {
            SearchTextInputKind::Query => (
                &dialog.query_input.text,
                dialog.query_input.selection_range.clone(),
                dialog.query_input.marked_range.clone(),
            ),
            SearchTextInputKind::DirectoryTarget => (
                &dialog.directory_input.text,
                dialog.directory_input.selection_range.clone(),
                dialog.directory_input.marked_range.clone(),
            ),
        }
    }

    /// 用给定文本替换搜索输入框当前选区。
    ///
    /// 业务意图：
    /// - 平台 IME 提交、快捷键粘贴和日志查看器粘贴到搜索框都需要同一套替换规则。
    /// - 统一处理组合文本、选区和光标位置，可以避免关键字输入框与目录输入框行为不一致。
    fn replace_search_text_selection(
        text: &mut String,
        selection_range: &mut Range<usize>,
        marked_range: &mut Option<Range<usize>>,
        replacement: &str,
    ) {
        let range = marked_range
            .take()
            .unwrap_or_else(|| Self::clamp_search_text_range(text, selection_range.clone()));
        text.replace_range(range.clone(), replacement);
        let cursor = range.start + replacement.len();
        *selection_range = cursor..cursor;
    }

    /// 用剪贴板文本覆盖搜索关键字。
    ///
    /// 业务意图：
    /// - 日志查看器是只读区域，`Ctrl+V` 的产品语义是把剪贴板内容作为新的搜索关键字，而不是继续编辑
    ///   `prepare_search_dialog_state` 可能根据日志选区预填出来的旧文本。
    /// - 封装为纯状态辅助函数，便于测试“覆盖而非追加”的关键边界。
    fn replace_search_query_with_clipboard_text(dialog: &mut SearchDialogState, text: String) {
        let cursor = text.len();
        dialog.query_input.text = text;
        dialog.query_input.selection_range = cursor..cursor;
        dialog.query_input.marked_range = None;
        dialog.query_history_menu_open = false;
        dialog.current_file_match_count = None;
        dialog.message = "已粘贴剪贴板文本，按 Enter 或点击搜索".to_string();
    }

    /// 判断是否为搜索输入框全选快捷键。
    ///
    /// 业务意图：
    /// - macOS 使用 `Cmd+A`，Windows 使用 `Ctrl+A`，部分输入路径会把 `Ctrl+A` 编码成 ASCII 控制字符。
    /// - 搜索关键字和目录输入框必须统一识别这些形态，避免只能输入却不能选择已有内容。
    fn is_select_all_keystroke(keystroke: &Keystroke) -> bool {
        Self::keystroke_matches_letter_or_control_code(keystroke, "a", CONTROL_A_CODE)
            && (keystroke.modifiers.control
                || keystroke.modifiers.platform
                || Self::keystroke_matches_control_code(keystroke, CONTROL_A_CODE))
    }

    /// 将搜索输入框内部 UTF-8 范围夹到合法字符边界。
    ///
    /// 边界条件：
    /// - 鼠标命中、平台输入和快捷键都可能给出超过文本长度或反向的范围。
    /// - Rust `String::replace_range` 只能接受 UTF-8 字符边界，因此这里统一转换为最近的安全边界。
    fn clamp_search_text_range(text: &str, range: Range<usize>) -> Range<usize> {
        let start = Self::search_input_clamp_byte_index(text, range.start);
        let end = Self::search_input_clamp_byte_index(text, range.end);
        start.min(end)..start.max(end)
    }

    /// 将任意字节下标夹到搜索输入文本的 UTF-8 字符边界。
    fn search_input_clamp_byte_index(text: &str, index: usize) -> usize {
        if index >= text.len() {
            return text.len();
        }
        if text.is_char_boundary(index) {
            return index;
        }
        text.char_indices()
            .map(|(byte_index, _)| byte_index)
            .take_while(|byte_index| *byte_index < index)
            .last()
            .unwrap_or(0)
    }

    /// 返回当前光标左侧的前一个 UTF-8 字符边界。
    ///
    /// 业务意图：
    /// - 方向键移动必须按用户可见字符边界前进，不能把中文、emoji 或其它多字节字符切成非法 `String` 范围。
    fn previous_search_text_boundary(text: &str, offset: usize) -> usize {
        let offset = Self::search_input_clamp_byte_index(text, offset);
        text[..offset]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    /// 返回当前光标右侧的下一个 UTF-8 字符边界。
    fn next_search_text_boundary(text: &str, offset: usize) -> usize {
        let offset = Self::search_input_clamp_byte_index(text, offset);
        text[offset..]
            .char_indices()
            .nth(1)
            .map(|(index, _)| offset + index)
            .unwrap_or(text.len())
    }

    /// 根据当前窗口焦点判断平台输入应写入哪个搜索文本框。
    ///
    /// 边界条件：
    /// - 如果两个输入框都未聚焦，默认写入查询词，保证 `Ctrl+F` 打开后立即输入仍符合用户预期。
    fn active_search_text_input_kind(&self, window: &Window) -> SearchTextInputKind {
        if self.search.search_directory_focus.is_focused(window) {
            SearchTextInputKind::DirectoryTarget
        } else {
            SearchTextInputKind::Query
        }
    }

    /// 将 UTF-16 范围转换为搜索查询词内部可安全切片的 UTF-8 字节范围。
    ///
    /// 业务意图：
    /// - 平台输入协议按 UTF-16 字符计数，Rust `String` 必须按 UTF-8 字节边界切片。
    /// - 所有中文输入、组合文本替换和候选词提交都必须通过该转换，避免把多字节字符切坏。
    fn search_input_range_from_utf16(query: &str, range_utf16: Range<usize>) -> Range<usize> {
        single_line_range_from_utf16(query, range_utf16)
    }

    /// 将搜索查询词内部 UTF-8 字节范围转换为平台输入协议需要的 UTF-16 范围。
    fn search_input_range_to_utf16(query: &str, range: Range<usize>) -> Range<usize> {
        single_line_range_to_utf16(query, range)
    }

    /// 把 UTF-8 字节边界映射到 UTF-16 偏移。
    fn search_input_utf16_offset_from_byte(query: &str, byte_index: usize) -> usize {
        single_line_utf16_offset_from_byte(query, byte_index)
    }

    /// 清理平台输入文本，确保搜索框保持单行普通文本。
    fn sanitize_search_input_text(text: &str) -> String {
        text.replace(['\n', '\r'], "")
    }

    /// 渲染搜索选项复选框。
    fn render_checkbox(checked: bool, palette: AppThemePalette) -> gpui::Stateful<gpui::Div> {
        let checkbox = div()
            .id("search-checkbox")
            .flex()
            .items_center()
            .justify_center()
            .w(px(14.0))
            .h(px(14.0))
            .rounded(px(3.0))
            .border_1()
            .border_color(rgb(if checked {
                palette.accent
            } else {
                palette.border
            }))
            .bg(rgb(if checked {
                palette.accent
            } else {
                palette.input
            }));

        if checked {
            checkbox.child(Self::render_lucide_icon(
                Some(Icon::Check),
                10.0,
                10.0,
                palette.on_accent,
            ))
        } else {
            checkbox
        }
    }

    /// 判断搜索按钮是否具备基本启动条件。
    fn search_can_start(&self, dialog: &SearchDialogState) -> bool {
        !dialog.query_input.text.trim().is_empty() && self.log.active_tab_id.is_some()
    }

    /// 判断快搜按钮是否具备基本启动条件。
    ///
    /// 业务意图：
    /// - 快搜不依赖搜索输入框内容，但必须存在已保存的有效关键字，并且当前不能已有搜索任务运行。
    /// - 当前 tab 的加载状态仍在启动时给出具体提示，按钮这里只做轻量可用性判断。
    fn quick_search_can_start(&self, dialog: &SearchDialogState) -> bool {
        !dialog.is_searching
            && self.log.active_tab_id.is_some()
            && !self.effective_quick_search_keywords().is_empty()
    }

    /// 判断当前文件计数按钮是否可用。
    ///
    /// 业务意图：
    /// - 计数按钮只统计当前已经打开并成功解码的活动文件，不触发后台读取，也不扫描当前目录。
    /// - 空关键字没有统计意义；加载中或失败 tab 也不能提供可靠计数。
    fn search_can_count_current_file(&self, dialog: &SearchDialogState) -> bool {
        !dialog.query_input.text.trim().is_empty() && self.active_log_tab_document().is_some()
    }

    /// 返回当前激活 tab 的文档。
    ///
    /// 边界条件：
    /// - 没有活动 tab、tab 已关闭、仍在加载或打开失败时都返回 `None`，调用方据此展示不可用状态。
    fn active_log_tab_document(&self) -> Option<LogTabDocument> {
        let active_tab_id = self.log.active_tab_id?;
        let active_tab = self
            .log
            .open_tabs
            .iter()
            .find(|tab| tab.id == active_tab_id)?;
        match &active_tab.state {
            LogTabState::Ready { document } => Some(document.clone()),
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => None,
        }
    }

    /// 清空当前文件计数缓存。
    ///
    /// 业务意图：
    /// - 查询词、大小写选项、活动 tab 或解码内容变化后，旧计数不再代表当前条件，必须清空。
    fn clear_search_current_file_match_count(&mut self) {
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.current_file_match_count = None;
        }
    }

    /// 设置搜索对话框匹配模式并清理依赖旧条件的临时状态。
    ///
    /// 业务意图：
    /// - “正则”开关会改变同一查询词的解释方式，当前文件计数缓存和历史下拉都必须失效。
    /// - 正则模式不修改 `case_sensitive` 原值，只让 UI 禁用该开关，方便切回普通文本后恢复用户之前的大小写选择。
    fn set_search_dialog_match_mode(dialog: &mut SearchDialogState, match_mode: SearchMatchMode) {
        if dialog.match_mode == match_mode {
            dialog.query_history_menu_open = false;
            return;
        }

        dialog.match_mode = match_mode;
        dialog.query_history_menu_open = false;
        dialog.current_file_match_count = None;
        dialog.message = match match_mode {
            SearchMatchMode::Literal => "已切换为普通文本搜索".to_string(),
            SearchMatchMode::Regex => "已切换为正则搜索，大小写由表达式控制".to_string(),
        };
    }

    /// 统计搜索关键字在当前文件中的出现次数。
    ///
    /// 业务意图：
    /// - 该动作由搜索窗口“计数”按钮触发，只针对当前激活文件的已解码内容，给用户一个轻量的命中规模反馈。
    /// - 计数结果以片段出现次数为单位，同一行多次出现会累加；完整搜索按钮仍负责生成结果面板和跳转明细。
    fn count_search_query_in_current_file(&mut self, context: &mut Context<Self>) {
        let Some(dialog) = self.search.search_dialog.as_ref() else {
            return;
        };
        let options = SearchOptions::single_with_mode(
            dialog.query_input.text.trim().to_string(),
            dialog.case_sensitive,
            dialog.match_mode,
        );
        if options.is_empty_query() {
            self.update_search_dialog_message("请输入要计数的关键字", context);
            return;
        }
        if let Err(error) = options.validate() {
            self.clear_search_current_file_match_count();
            self.update_search_dialog_message(error.to_string(), context);
            return;
        }
        let Some(document) = self.active_log_tab_document() else {
            self.update_search_dialog_message("当前文件未打开完成，无法计数", context);
            return;
        };
        let count = match document {
            LogTabDocument::InMemory(document) => {
                count_query_occurrences(&document.lines, &options)
            }
            LogTabDocument::Paged(document) => count_query_occurrences_paged(&document, &options),
        };
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            dialog.current_file_match_count = Some(count);
            dialog.message = format!("当前文件命中 {count} 次");
        }
        context.notify();
    }
}

impl MainView {
    fn render_splitter_hit_overlay(&self, context: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("content-splitter-hit-overlay")
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .h_full()
            .w(px(SPLITTER_OVERLAY_HIT_WIDTH))
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(Self::start_resizing_splitter),
            )
    }

    /// 渲染已加载目录树但尚未打开文件时的右侧提示。
    ///
    /// 业务意图：
    /// - 用户完成“加载日志”后，下一步是从左侧树选择具体日志文件，右侧需要明确指引当前空态。
    fn render_loaded_right_empty_message(&self) -> impl IntoElement {
        let palette = self.palette();
        div()
            .id("loaded-right-empty-message")
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .size_full()
            .px_4()
            .bg(rgb(palette.background))
            .child(Self::render_lucide_icon(
                Some(Icon::FileSearch),
                34.0,
                30.0,
                palette.muted_text,
            ))
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.text))
                    .child("点击左侧日志文件查看内容"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_text))
                    .child("支持普通日志文件和压缩包内日志文件。"),
            )
    }

    /// 渲染右侧 tab 栏。
    ///
    /// 业务意图：
    /// - 每个打开的日志文件对应一个 tab，用户可以在多个日志之间快速切换。
    /// - tab 上的右键菜单提供关闭当前、关闭其他和关闭所有三个命令。
    /// - tab 标题完整展示，超出可视宽度时通过横向滚动和两侧箭头访问。
    fn render_log_tab_bar(&self, context: &mut Context<Self>) -> impl IntoElement {
        let tabs = self
            .log
            .open_tabs
            .iter()
            .map(|tab| {
                (
                    tab.id,
                    tab.title.clone(),
                    self.log.active_tab_id == Some(tab.id),
                )
            })
            .collect::<Vec<_>>();
        let tab_elements = tabs
            .into_iter()
            .map(|(tab_id, title, active)| self.render_log_tab(tab_id, title, active, context))
            .collect::<Vec<_>>();

        div()
            .id("log-tab-bar")
            .relative()
            .flex()
            .items_center()
            .h(px(LOG_TAB_BAR_HEIGHT))
            .flex_none()
            .overflow_hidden()
            .bg(rgb(self.palette().panel))
            .border_b_1()
            .border_color(rgb(self.palette().border))
            .child(self.render_tab_scroll_button(Icon::ChevronLeft, -LOG_TAB_SCROLL_STEP, context))
            .child(
                div()
                    .id("log-tab-scroll-viewport")
                    .flex()
                    .items_end()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .overflow_x_scroll()
                    .scrollbar_width(px(0.0))
                    .track_scroll(&self.log.tab_bar_scroll_handle)
                    .children(tab_elements),
            )
            .child(self.render_tab_scroll_button(Icon::ChevronRight, LOG_TAB_SCROLL_STEP, context))
    }

    /// 渲染 tab 栏横向滚动箭头。
    ///
    /// 业务意图：
    /// - 打开的日志较多时，用户可以通过左右箭头移动 tab 栏，而不需要依赖触控板横向滚动。
    /// - 按钮始终显示，符合用户“tab页签两边显示滚动箭头”的要求。
    fn render_tab_scroll_button(
        &self,
        icon: Icon,
        delta: f32,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        div()
            .id(SharedString::from(format!("tab-scroll-{}", delta)))
            .flex()
            .items_center()
            .justify_center()
            .h_full()
            .w(px(LOG_TAB_SCROLL_BUTTON_WIDTH))
            .flex_none()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .text_color(rgb(palette.muted_text))
            .cursor_pointer()
            .hover(move |button| {
                button
                    .bg(rgb(palette.surface))
                    .text_color(rgb(palette.accent))
            })
            .when(delta < 0.0, |button| button.border_r_1())
            .when(delta > 0.0, |button| button.border_l_1())
            .child(Self::render_lucide_icon(
                Some(icon),
                14.0,
                14.0,
                palette.muted_text,
            ))
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.scroll_tab_bar(delta, context);
                    // 滚动箭头是 tab 栏内的独立按钮，不应把 click 继续传递给 tab 容器。
                    context.stop_propagation();
                }),
            )
    }

    /// 按给定像素距离横向滚动 tab 栏。
    ///
    /// 业务意图：
    /// - `ScrollHandle` 的偏移量向左滚动时为负值，因此向右箭头需要减少 x 偏移。
    /// - 滚动结果限制在 `[最大负偏移, 0]`，避免箭头点击后出现空白区域。
    fn scroll_tab_bar(&mut self, delta: f32, context: &mut Context<Self>) {
        let current_offset = self.log.tab_bar_scroll_handle.offset();
        let max_scroll = self.log.tab_bar_scroll_handle.max_offset().width;
        let next_x = (current_offset.x - px(delta)).clamp(-max_scroll, px(0.0));
        self.log
            .tab_bar_scroll_handle
            .set_offset(point(next_x, current_offset.y));
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        context.notify();
    }

    /// 渲染单个日志 tab。
    ///
    /// 业务意图：
    /// - 左键激活 tab，右键打开 tab 操作菜单。
    /// - tab 标题完整展示，不做省略；关闭按钮放在右侧，便于快速收起单个日志。
    fn render_log_tab(
        &self,
        tab_id: usize,
        title: String,
        active: bool,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        let background = if active {
            palette.background
        } else {
            palette.panel
        };
        div()
            .id(SharedString::from(format!("log-tab-{}", tab_id)))
            .flex()
            .items_center()
            .h(px(LOG_TAB_BAR_HEIGHT - 1.0))
            .min_w(px(96.0))
            .flex_none()
            .px_3()
            .gap_1()
            .border_r_1()
            .border_color(rgb(palette.border))
            .bg(rgb(background))
            .text_size(px(LOG_TAB_FONT_SIZE))
            .text_color(rgb(if active {
                palette.text
            } else {
                palette.muted_text
            }))
            .cursor_pointer()
            .hover(move |tab| tab.bg(rgb(palette.surface)))
            .child(Self::render_lucide_icon(
                Some(Icon::FileText),
                14.0,
                13.0,
                palette.muted_text,
            ))
            .child(div().flex_none().whitespace_nowrap().child(title))
            .child(self.render_tab_close_button(tab_id, context))
            .on_click(
                context.listener(move |view, event: &ClickEvent, _window, context| {
                    if event.standard_click() {
                        view.activate_tab(tab_id);
                        context.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(move |view, event: &MouseDownEvent, _window, context| {
                    view.open_tab_context_menu(
                        tab_id,
                        f32::from(event.position.x),
                        f32::from(event.position.y),
                        context,
                    );
                }),
            )
    }

    /// 渲染 tab 右侧关闭按钮。
    ///
    /// 业务意图：
    /// - 每个 tab 都提供明确的关闭入口，避免用户只能通过右键菜单关闭。
    /// - 关闭后按 `close_tab` 的统一规则切换激活 tab，并清理相关弹层状态。
    fn render_tab_close_button(
        &self,
        tab_id: usize,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let palette = self.palette();
        div()
            .id(SharedString::from(format!("log-tab-close-{}", tab_id)))
            .flex()
            .items_center()
            .justify_center()
            .w(px(LOG_TAB_CLOSE_BUTTON_WIDTH))
            .h(px(LOG_TAB_CLOSE_BUTTON_WIDTH))
            .flex_none()
            .rounded(px(3.0))
            .text_color(rgb(palette.muted_text))
            .hover(move |button| button.bg(rgb(palette.hover)).text_color(rgb(palette.text)))
            .child(Self::render_lucide_icon(
                Some(Icon::X),
                12.0,
                12.0,
                palette.muted_text,
            ))
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.close_tab(tab_id);
                    view.log.tab_context_menu = None;
                    view.log.encoding_dropdown_menu = None;
                    context.notify();
                    // 关闭按钮嵌套在 tab 元素中，关闭后不能再让父 tab 收到同一次点击。
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染当前激活 tab 的内容。
    ///
    /// 业务意图：
    /// - 激活 tab 统一显示编码工具条；正文根据读取状态显示加载、错误或日志虚拟列表。
    fn render_active_log_tab(&self, context: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let Some(active_tab_id) = self.log.active_tab_id else {
            return div()
                .id("active-log-tab-empty")
                .flex()
                .flex_1()
                .child(self.render_loaded_right_empty_message());
        };
        let Some(tab) = self
            .log
            .open_tabs
            .iter()
            .find(|tab| tab.id == active_tab_id)
        else {
            return div()
                .id("active-log-tab-missing")
                .flex()
                .flex_1()
                .child(self.render_loaded_right_empty_message());
        };

        div()
            .id("active-log-tab")
            .flex()
            .flex_col()
            .flex_1()
            .overflow_hidden()
            .child(self.render_log_document_toolbar(tab, context))
            .child(self.render_log_tab_body(tab, context))
    }

    /// 渲染当前日志文档工具条。
    ///
    /// 业务意图：
    /// - 左侧不再放置独立编码控件，避免挤占日志正文起点上方空间。
    /// - 工具条右侧把实际编码直接渲染成编码选择器，同时展示来源、识别方式和行数。
    fn render_log_document_toolbar(
        &self,
        tab: &OpenLogTab,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let source_kind = match &tab.source {
            LogFileSource::LocalFile { .. } => "本地文件",
            LogFileSource::ArchiveMember { .. } => "压缩包内文件",
            LogFileSource::MaterializedArchiveMember { .. } => "压缩包内文件",
            LogFileSource::NestedArchiveMember { .. } => "嵌套压缩包内文件",
        };
        let encoding_button_label = Self::log_tab_encoding_selector_label(tab);
        let status = match &tab.state {
            LogTabState::Ready { document } => {
                // 状态栏展示编码来源和行数；实际编码本身由右侧内联编码选择器负责显示和切换。
                let encoding_source = if document.detected_automatically() {
                    "自动识别"
                } else {
                    "手动选择"
                };
                let replacement_warning = if document.had_replacements() {
                    " · 含替换字符"
                } else {
                    ""
                };
                format!(
                    "{} · {}{}",
                    encoding_source,
                    document.status_line_label(),
                    replacement_warning
                )
            }
            LogTabState::Loading { .. } => "正在处理".to_string(),
            LogTabState::Failed { .. } => "需要选择正确编码或重新加载".to_string(),
        };
        let palette = self.palette();

        div()
            .id("log-document-toolbar")
            .flex()
            .items_center()
            .justify_end()
            .h(px(LOG_DOCUMENT_TOOLBAR_HEIGHT))
            .flex_none()
            .px_3()
            .gap_2()
            .bg(rgb(palette.panel))
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .id("log-document-status-group")
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(rgb(palette.muted_text))
                    .child(div().flex_none().child(source_kind))
                    .child(Self::render_status_separator(palette))
                    .child(self.render_encoding_selector(
                        tab.id,
                        encoding_button_label,
                        matches!(tab.state, LogTabState::Ready { .. }) || tab.raw_bytes.is_some(),
                        palette,
                        context,
                    ))
                    .child(Self::render_status_separator(palette))
                    .child(div().min_w_0().truncate().child(status)),
            )
    }

    /// 返回日志工具条编码选择器应展示的文案。
    ///
    /// 业务意图：
    /// - 自动模式下按钮展示实际检测出的编码，方便用户理解当前文件最终按什么编码打开。
    /// - 手动模式下按钮必须优先展示用户选择的编码，即使底层解码出的文本编码枚举与自动检测结果相同，
    ///   也不能回退成检测结果，否则用户会误以为“编码切换选择无效”。
    ///
    /// 边界条件：
    /// - 加载中或解码失败时没有稳定的文档编码，此时直接展示当前选择，保证失败后仍能看到正在尝试的编码。
    fn log_tab_encoding_selector_label(tab: &OpenLogTab) -> &'static str {
        match tab.encoding_choice {
            EncodingChoice::Auto => match &tab.state {
                LogTabState::Ready { document } => document.encoding_label(),
                LogTabState::Loading { .. } | LogTabState::Failed { .. } => {
                    EncodingChoice::Auto.label()
                }
            },
            EncodingChoice::Manual(encoding) => encoding.label(),
        }
    }

    /// 渲染编码切换控件。
    ///
    /// 业务意图：
    /// - 编码选项改为紧凑下拉框，避免多个编码按钮平铺占用顶部工具条。
    /// - “自动”重新执行检测流程，手动选项按指定编码重新解码原始字节。
    /// - 原始字节未读取完成前保持禁用，避免用户提前选择编码导致 UI 状态和实际解码结果不一致。
    fn render_encoding_selector(
        &self,
        tab_id: usize,
        display_label: &'static str,
        enabled: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("encoding-selector")
            .flex()
            .items_center()
            .flex_none()
            .child(
                div()
                    .id(SharedString::from(format!(
                        "encoding-dropdown-button-{}",
                        tab_id
                    )))
                    .flex()
                    .items_center()
                    .justify_between()
                    .w(px(ENCODING_DROPDOWN_BUTTON_WIDTH))
                    .h(px(ENCODING_DROPDOWN_BUTTON_HEIGHT))
                    .px_2()
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.input))
                    .text_xs()
                    .text_color(rgb(if enabled {
                        palette.text
                    } else {
                        palette.muted_text
                    }))
                    .when(enabled, |button| {
                        button.cursor_pointer().hover(move |button| {
                            button
                                .border_color(rgb(palette.accent))
                                .bg(rgb(palette.hover))
                        })
                    })
                    .when(!enabled, |button| button.opacity(0.62))
                    .child(display_label)
                    .child(Self::render_lucide_icon(
                        Some(Icon::ChevronDown),
                        12.0,
                        12.0,
                        palette.muted_text,
                    ))
                    .on_click(context.listener(
                        move |view, event: &ClickEvent, _window, context| {
                            if enabled {
                                let position = event.position();
                                view.toggle_encoding_dropdown(
                                    tab_id,
                                    f32::from(position.x),
                                    f32::from(position.y),
                                    context,
                                );
                            }
                            // 编码选择按钮属于状态栏内的独立控件，点击不应继续传给日志正文或外层浮层。
                            context.stop_propagation();
                        },
                    )),
            )
    }

    /// 渲染右侧状态栏中的点状分隔符。
    ///
    /// 业务意图：
    /// - 来源、编码选择器、识别方式和行数属于同一组状态信息，用轻量分隔符维持可读性。
    /// - 单独函数可以避免多个位置重复硬编码颜色和文本。
    fn render_status_separator(palette: AppThemePalette) -> gpui::Div {
        div()
            .flex_none()
            .text_color(rgb(palette.muted_text))
            .child("·")
    }

    /// 切换编码下拉框展开状态。
    ///
    /// 业务意图：
    /// - 同一个 tab 再次点击编码框会收起菜单；点击其它 tab 的编码框会切换到新的菜单。
    /// - 打开编码菜单时收起 tab 右键菜单，避免两个弹层同时争抢点击区域。
    fn toggle_encoding_dropdown(
        &mut self,
        tab_id: usize,
        window_x: f32,
        window_y: f32,
        context: &mut Context<Self>,
    ) {
        if !self
            .log
            .open_tabs
            .iter()
            .any(|tab| tab.id == tab_id && matches!(tab.state, LogTabState::Ready { .. }))
        {
            // 编码菜单必须等内存原始字节或分页文档可用后才能打开，避免用户在加载过程中选择编码但无法立即解析。
            self.log.encoding_dropdown_menu = None;
            self.log.tab_context_menu = None;
            self.search.search_results_context_menu = None;
            self.log.log_viewer_context_menu = None;
            context.notify();
            return;
        }

        let is_same_menu_open = self
            .log
            .encoding_dropdown_menu
            .as_ref()
            .is_some_and(|menu| menu.tab_id == tab_id);
        self.log.encoding_dropdown_menu = if is_same_menu_open {
            None
        } else {
            Some(EncodingDropdownMenu {
                tab_id,
                x: self.encoding_dropdown_menu_x(window_x),
                y: Self::encoding_dropdown_menu_y(window_y),
            })
        };
        self.log.tab_context_menu = None;
        self.search.search_results_context_menu = None;
        self.log.log_viewer_context_menu = None;
        context.notify();
    }

    /// 根据点击位置计算编码菜单在右侧工作区内的横坐标。
    ///
    /// 业务意图：
    /// - 编码选择器位于右侧状态栏，状态栏内容会随来源类型、识别状态和行数变化。
    /// - GPUI 当前没有直接暴露按钮布局矩形给点击监听，因此用点击点近似还原按钮左缘，让菜单跟随按钮打开。
    ///
    /// 边界条件：
    /// - 点击按钮文字或箭头会带来几个像素的偏差，但菜单仍紧邻编码按钮，不会回到旧版左侧固定位置。
    fn encoding_dropdown_menu_x(&self, window_x: f32) -> f32 {
        let panel_x = (window_x - self.right_panel_left_offset()).max(0.0);
        (panel_x - ENCODING_DROPDOWN_BUTTON_WIDTH / 2.0).max(0.0)
    }

    /// 根据点击位置计算编码菜单在右侧工作区内的纵坐标。
    ///
    /// 业务意图：
    /// - 菜单应出现在编码按钮下方；点击位置通常位于按钮中部，因此加上半个按钮高度和固定间隔。
    fn encoding_dropdown_menu_y(window_y: f32) -> f32 {
        (window_y - TOOLBAR_HEIGHT
            + ENCODING_DROPDOWN_BUTTON_HEIGHT / 2.0
            + ENCODING_DROPDOWN_MENU_GAP)
            .max(0.0)
    }

    /// 返回编码下拉框的手动编码选项。
    ///
    /// 业务意图：
    /// - 打开日志时仍默认自动识别；下拉菜单只承载用户主动纠正编码的手动选项。
    /// - 用户要求去除菜单中的“自动”，因此这里不再把 `EncodingChoice::Auto` 作为可点击项渲染。
    /// - UI 渲染和点击处理共享同一组选项，避免菜单项和编码解析能力不一致。
    fn encoding_choices() -> Vec<EncodingChoice> {
        LogTextEncoding::manual_options()
            .into_iter()
            .map(EncodingChoice::Manual)
            .collect()
    }

    /// 渲染编码下拉菜单。
    ///
    /// 业务意图：
    /// - 菜单作为右侧工作区内部弹层绘制，视觉上贴在当前 tab 的编码选择框下方。
    /// - 选择任一编码后复用当前 tab 保存的原始字节重新解析，并立即收起菜单。
    fn render_encoding_dropdown_menu(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(menu) = &self.log.encoding_dropdown_menu else {
            return div().id("encoding-dropdown-menu-empty").hidden();
        };
        let Some(tab) = self.log.open_tabs.iter().find(|tab| tab.id == menu.tab_id) else {
            return div().id("encoding-dropdown-menu-missing").hidden();
        };
        let tab_id = tab.id;
        let selected_choice = tab.encoding_choice;
        let palette = self.palette();
        let menu_items = Self::encoding_choices()
            .into_iter()
            .map(|choice| {
                self.render_encoding_dropdown_item(
                    tab_id,
                    choice,
                    choice == selected_choice,
                    palette,
                    context,
                )
            })
            .collect::<Vec<_>>();

        div()
            .id("encoding-dropdown-menu")
            .absolute()
            .left(px(menu.x))
            .top(px(menu.y))
            .w(px(ENCODING_DROPDOWN_WIDTH))
            .py_1()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 下拉菜单包含内边距和选中态空白，菜单壳层需要兜住这些区域的点击。
                    context.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    // 右键点在菜单上不能穿透到底层日志正文并打开日志右键菜单。
                    context.stop_propagation();
                }),
            )
            .children(menu_items)
    }

    /// 渲染编码下拉菜单单项。
    ///
    /// 业务意图：
    /// - 当前选中编码通过浅蓝背景标识；点击其它项会触发重新解码。
    /// - 菜单项使用左键按下立即处理，而不是等待 click 合成事件；编码菜单上方有关闭遮罩，
    ///   这样可以避免鼠标按下和释放之间弹层状态变化导致选择事件丢失。
    fn render_encoding_dropdown_item(
        &self,
        tab_id: usize,
        choice: EncodingChoice,
        selected: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let item = div()
            .id(SharedString::from(format!(
                "encoding-dropdown-item-{}-{}",
                tab_id,
                choice.label()
            )))
            .flex()
            .items_center()
            .justify_between()
            .h(px(ENCODING_DROPDOWN_ITEM_HEIGHT))
            .px_2()
            .text_xs()
            .text_color(rgb(if selected {
                palette.accent
            } else {
                palette.text
            }))
            .bg(rgb(if selected {
                palette.selected
            } else {
                palette.menu
            }))
            .cursor_pointer()
            .hover(move |item| item.bg(rgb(palette.hover)).text_color(rgb(palette.accent)))
            .child(choice.label());

        let item = if selected {
            item.child(Self::render_lucide_icon(
                Some(Icon::Check),
                12.0,
                12.0,
                palette.accent,
            ))
        } else {
            item
        };

        item.on_mouse_down(
            MouseButton::Left,
            context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                view.select_tab_encoding(tab_id, choice, context);
                context.stop_propagation();
            }),
        )
    }
}

impl MainView {
    fn open_tab_context_menu(
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
    fn render_tab_context_menu(&self, context: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
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
    fn render_tab_context_menu_item(
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
    fn handle_tab_context_menu_action(
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
    fn close_tab(&mut self, tab_id: usize) {
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
    fn close_other_tabs(&mut self, tab_id: usize) {
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
    fn close_all_tabs(&mut self) {
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
    fn cleanup_tab_paged_resources(tab: &OpenLogTab) {
        if let LogTabState::Ready {
            document: LogTabDocument::Paged(document),
        } = &tab.state
            && let Some(temp_path) = document.materialized_temp_path.as_deref()
        {
            cleanup_materialized_file(temp_path);
        }
    }

    /// 渲染当前大功能页内容。
    ///
    /// 业务意图：
    /// - 左侧主导航只负责切换功能，右侧区域按当前功能渲染完整工作区。
    /// - 日志分析保留现有目录树和日志正文；HPROF 解析嵌入可复用分析实体；AI 对话渲染完整聊天工作区。
    fn render_main_feature_page(&mut self, context: &mut Context<Self>) -> gpui::AnyElement {
        match self.navigation.active_main_feature {
            MainFeature::LogAnalysis => self.render_log_analysis_page(context).into_any_element(),
            MainFeature::HprofAnalysis => {
                self.render_hprof_analysis_page(context).into_any_element()
            }
            MainFeature::AiChat => self.render_ai_chat_page(context).into_any_element(),
        }
    }

    /// 渲染日志分析大功能页。
    ///
    /// 业务意图：
    /// - 根级顶部工具栏已移除，日志页内部保留“加载日志”和“搜索”操作栏，下面继续使用原有左右分栏日志工作区。
    fn render_log_analysis_page(&self, context: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("log-analysis-page")
            .flex()
            .flex_col()
            .size_full()
            .child(self.render_log_action_bar(context))
            .child(self.render_content(context))
    }

    /// 渲染 HPROF 解析大功能页。
    ///
    /// 业务意图：
    /// - HPROF 入口从独立窗口迁入主窗口，页顶部提供文件选择按钮，下面直接嵌入解析视图的空态、进度或结果。
    fn render_hprof_analysis_page(&mut self, context: &mut Context<Self>) -> impl IntoElement {
        let hprof_view = self.hprof_analysis_view(context);
        div()
            .id("hprof-analysis-page")
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(self.palette().background))
            .child(self.render_hprof_action_bar(context))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(hprof_view),
            )
    }

    /// 渲染 HPROF 页顶部操作栏。
    fn render_hprof_action_bar(&self, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette();
        div()
            .id("hprof-action-bar")
            .flex()
            .items_center()
            .h(px(TOOLBAR_HEIGHT))
            .pl(px(LOG_TREE_ROW_HORIZONTAL_PADDING))
            .pr_4()
            .bg(rgb(palette.panel))
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(self.render_hprof_select_file_button(context))
    }

    /// 渲染 HPROF 文件选择按钮。
    ///
    /// 业务意图：
    /// - HPROF 页内选择文件后复用当前内嵌分析视图启动后台解析，不再打开新的分析窗口。
    fn render_hprof_select_file_button(&self, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette();
        div()
            .id("hprof-select-file-button")
            .flex()
            .items_center()
            .gap_1()
            .flex_none()
            .pl(px(0.0))
            .pr(px(TOOLBAR_BUTTON_HORIZONTAL_PADDING))
            .py(px(TOOLBAR_BUTTON_VERTICAL_PADDING))
            .text_sm()
            .text_color(rgb(palette.text))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(move |button| button.text_color(rgb(palette.accent)))
            .active(|button| button.opacity(0.82))
            .child(Self::render_lucide_icon(
                Some(Icon::ChartNoAxesCombined),
                TOOLBAR_BUTTON_ICON_WIDTH,
                TOOLBAR_BUTTON_ICON_SIZE,
                palette.muted_text,
            ))
            .child("选择 HPROF 文件")
            .on_click(context.listener(Self::open_hprof_from_toolbar))
    }

    /// 渲染左右分栏内容区域。
    ///
    /// 业务意图：
    /// - 未加载日志时主内容区占满剩余空间并显示友好提示，不出现空的左侧目录区域。
    /// - 加载完成后切换为左右两栏，左侧展示目录树，右侧展示日志 tab 工作区或点击文件提示。
    ///
    /// 边界条件：
    /// - 当前只有加载完成状态显示可拖动分割线；未加载、加载中或失败状态不显示分割线。
    /// - 没有打开任何日志 tab 时，右侧只显示下一步提示，不创建空白占位 tab。
    /// - 当前分割宽度仅在内存中生效，不跨启动保存。
    fn render_content(&self, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette();
        if !matches!(self.log.load_state, LogTreeLoadState::Loaded(_)) {
            return div()
                .id("log-content-empty-state")
                .flex()
                .flex_1()
                .size_full()
                .bg(rgb(palette.background))
                .child(self.render_primary_content_message());
        }

        if self.should_hide_log_tree_panel() {
            return div()
                .id("log-content-single-log-view")
                .flex()
                .flex_1()
                .size_full()
                .bg(rgb(palette.background))
                // 单文件模式隐藏了左侧目录树和分割线，但右侧日志正文仍使用自绘滚动条。
                // 拖动滑块时后续鼠标移动可能落在正文空白处，必须和分栏模式一样由内容容器统一续传和清理拖动状态。
                .on_mouse_move(context.listener(Self::handle_content_mouse_move))
                .on_mouse_up(
                    MouseButton::Left,
                    context.listener(Self::handle_content_mouse_up),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    context.listener(Self::handle_content_mouse_up),
                )
                .child(
                    div()
                        .id("right-log-panel-single-log")
                        .relative()
                        .h_full()
                        .flex_1()
                        .bg(rgb(palette.background))
                        .overflow_hidden()
                        .child(self.render_right_log_panel(context)),
                );
        }

        div()
            .id("log-content-split-view")
            .flex()
            .flex_1()
            .size_full()
            .bg(rgb(palette.background))
            .on_mouse_move(context.listener(Self::handle_content_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(Self::handle_content_mouse_up),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(Self::handle_content_mouse_up),
            )
            .child(
                div()
                    .id("left-log-panel")
                    .h_full()
                    .w(px(self.left_panel_width))
                    .flex_none()
                    .overflow_hidden()
                    .child(self.render_log_tree_panel(context)),
            )
            .child(self.render_splitter(context))
            .child(
                div()
                    .id("right-log-panel")
                    .relative()
                    .h_full()
                    .flex_1()
                    .bg(rgb(palette.background))
                    .overflow_hidden()
                    .child(self.render_right_log_panel(context))
                    .child(self.render_splitter_hit_overlay(context)),
            )
    }

    /// 判断当前加载结果是否应隐藏左侧目录树。
    ///
    /// 业务意图：
    /// - 当用户加载的内容最终只有一个可打开日志时，左侧树没有选择价值，应把完整空间留给日志浏览。
    /// - 多文件目录或压缩包仍显示左侧树，便于用户选择、右键保存和线程分析。
    fn should_hide_log_tree_panel(&self) -> bool {
        matches!(
            &self.log.load_state,
            LogTreeLoadState::Loaded(tree_state) if tree_state.single_log_source().is_some()
        )
    }

    /// 返回右侧日志工作区相对主窗口左侧的横向偏移。
    ///
    /// 业务意图：
    /// - 右键菜单和编码下拉菜单由窗口坐标转换到右侧面板局部坐标，必须和当前布局是否隐藏左侧树保持一致。
    /// - 主窗口最左侧现在固定 56px 大导航，因此所有日志页内部弹层都必须先扣除导航宽度。
    /// - 单日志模式没有左侧树和分割线，只扣除导航宽度；多文件模式继续额外扣除左侧树宽度和分割线宽度。
    fn right_panel_left_offset(&self) -> f32 {
        Self::right_panel_left_offset_for_layout(
            self.should_hide_log_tree_panel(),
            self.left_panel_width,
        )
    }

    /// 按布局状态计算右侧日志工作区相对主窗口左侧的横向偏移。
    ///
    /// 业务意图：
    /// - 将坐标规则拆成纯函数，便于测试固定导航竖条加入后各类日志弹层不会整体向左错位。
    fn right_panel_left_offset_for_layout(hide_log_tree_panel: bool, left_panel_width: f32) -> f32 {
        if hide_log_tree_panel {
            MAIN_NAV_WIDTH
        } else {
            MAIN_NAV_WIDTH + left_panel_width + SPLITTER_VISIBLE_WIDTH
        }
    }

    /// 计算左侧目录树右键菜单在目录树面板内的横坐标。
    ///
    /// 业务意图：
    /// - 鼠标事件使用主窗口坐标，而菜单渲染在目录树面板内部；固定大导航宽度必须在进入面板坐标前扣除。
    fn log_tree_context_menu_x(window_x: f32, left_panel_width: f32) -> f32 {
        (window_x - MAIN_NAV_WIDTH)
            .max(0.0)
            .clamp(0.0, left_panel_width - LOG_TREE_CONTEXT_MENU_WIDTH)
    }

    /// 渲染左右两栏之间的可拖动分割线。
    ///
    /// 业务意图：
    /// - 分割线提供明确的拖动命中区域，让用户可以动态调整左右区域宽度。
    /// - 可见部分保持 1px 细线，透明命中区由右侧内容覆盖层提供，避免右侧出现白色拖动间隔。
    ///
    /// 边界条件：
    /// - 当前只支持水平拖动，不能双击重置，也不能通过键盘调整。
    /// - 该元素只承担视觉线条和左侧边界拖动；右侧扩展命中区由 `render_splitter_hit_overlay` 提供。
    fn render_splitter(&self, context: &mut Context<Self>) -> impl IntoElement {
        // 拖动中使用略深的线条颜色，提供清晰反馈；非拖动状态保持轻量，避免抢占内容注意力。
        let line_color = if self.is_resizing_splitter {
            0x94a3b8
        } else {
            self.palette().border
        };

        div()
            .id("content-splitter")
            .flex()
            .items_center()
            .justify_center()
            .h_full()
            .w(px(SPLITTER_VISIBLE_WIDTH))
            .flex_none()
            .cursor_col_resize()
            .bg(rgb(line_color))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(Self::start_resizing_splitter),
            )
    }

    /// 渲染另存为同名文件确认弹窗。
    ///
    /// 业务意图：
    /// - 目标目录已有同名文件时，必须先让用户在“跳过”和“覆盖”之间明确选择，避免后台任务静默覆盖用户文件。
    ///
    /// 边界条件：
    /// - 弹窗作为主窗口内模态层绘制，遮挡底层日志区域并拦截鼠标事件，防止用户在确认前继续触发其它操作。
    fn render_save_overwrite_confirm_dialog(
        &self,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(dialog) = &self.log.save_overwrite_confirm_dialog else {
            return div().id("save-overwrite-confirm-empty").hidden();
        };
        let palette = self.palette();
        let mut backdrop = rgb(0x000000);
        backdrop.a = 0.34;

        div()
            .id("save-overwrite-confirm")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .bottom(px(0.0))
            .flex()
            .items_center()
            .justify_center()
            .bg(backdrop)
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                    context.stop_propagation();
                }),
            )
            .child(
                div()
                    .w(px(420.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .shadow_lg()
                    .p_4()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child("目标目录已有同名文件"),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_sm()
                            .line_height(px(20.0))
                            .text_color(rgb(palette.muted_text))
                            .child(format!(
                                "发现 {} 个目标文件已存在。请选择跳过这些文件，或覆盖目标目录中的同名文件。",
                                dialog.conflict_count
                            )),
                    )
                    .child(
                        div()
                            .mt_2()
                            .px_2()
                            .py_1()
                            .rounded(px(5.0))
                            .bg(rgb(palette.input))
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child(format!("示例：{}", dialog.first_conflict_path.display())),
                    )
                    .child(
                        div()
                            .mt_4()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(self.render_save_overwrite_button(
                                "跳过",
                                SaveConflictPolicy::SkipExisting,
                                false,
                                palette,
                                context,
                            ))
                            .child(self.render_save_overwrite_button(
                                "覆盖",
                                SaveConflictPolicy::OverwriteExisting,
                                true,
                                palette,
                                context,
                            )),
                    ),
            )
    }

    /// 渲染另存为冲突确认按钮。
    ///
    /// 业务意图：
    /// - “跳过”是保守操作，“覆盖”是破坏性操作；通过不同视觉权重帮助用户理解风险。
    fn render_save_overwrite_button(
        &self,
        label: &'static str,
        conflict_policy: SaveConflictPolicy,
        primary: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!("save-overwrite-{label}")))
            .flex()
            .items_center()
            .justify_center()
            .h(px(30.0))
            .px_4()
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(if primary {
                palette.accent
            } else {
                palette.border
            }))
            .bg(rgb(if primary {
                palette.accent
            } else {
                palette.panel
            }))
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(if primary {
                palette.on_accent
            } else {
                palette.text
            }))
            .cursor_pointer()
            .hover(move |button| {
                button.bg(rgb(if primary {
                    palette.accent_hover
                } else {
                    palette.hover
                }))
            })
            .child(label)
            .on_click(
                context.listener(move |view, _event: &ClickEvent, _window, context| {
                    view.handle_save_overwrite_choice(conflict_policy, context);
                }),
            )
    }
}

impl EntityInputHandler for MainView {
    /// 返回指定 UTF-16 范围内的搜索框文本。
    ///
    /// 业务意图：
    /// - 平台输入法需要查询当前文本片段以管理候选词、组合文本和替换范围。
    /// - 搜索框内部保存 UTF-8 字符串，因此这里必须进行 UTF-16 到 UTF-8 的安全转换。
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<String> {
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let state = self.model_config_input_state(kind);
            let range = Self::search_input_range_from_utf16(&state.text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &state.text,
                range.clone(),
            ));
            return Some(state.text[range].to_string());
        }
        if self.ai_chat_input_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(&self.ai_chat_input_text, range_utf16);
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &self.ai_chat_input_text,
                range.clone(),
            ));
            return Some(self.ai_chat_input_text[range].to_string());
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(
                &self.settings.quick_search_keywords_input.text,
                range_utf16,
            );
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &self.settings.quick_search_keywords_input.text,
                range.clone(),
            ));
            return Some(self.settings.quick_search_keywords_input.text[range].to_string());
        }
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            let range = Self::search_input_range_from_utf16(
                &self.settings.thread_analysis_filter_text,
                range_utf16,
            );
            adjusted_range.replace(Self::search_input_range_to_utf16(
                &self.settings.thread_analysis_filter_text,
                range.clone(),
            ));
            return Some(self.settings.thread_analysis_filter_text[range].to_string());
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, _, _) = Self::search_text_state(dialog, input_kind);
        let range = Self::search_input_range_from_utf16(text, range_utf16);
        adjusted_range.replace(Self::search_input_range_to_utf16(text, range.clone()));
        Some(text[range].to_string())
    }

    /// 返回搜索框当前选择范围。
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let state = self.model_config_input_state(kind);
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &state.text,
                    state.selection_range.clone(),
                ),
                reversed: false,
            });
        }
        if self.ai_chat_input_focus.is_focused(window) {
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &self.ai_chat_input_text,
                    self.ai_chat_input_selection_range.clone(),
                ),
                reversed: false,
            });
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &self.settings.quick_search_keywords_input.text,
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .clone(),
                ),
                reversed: false,
            });
        }
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            return Some(UTF16Selection {
                range: Self::search_input_range_to_utf16(
                    &self.settings.thread_analysis_filter_text,
                    self.settings.thread_analysis_filter_selection_range.clone(),
                ),
                reversed: false,
            });
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, selection_range, _) = Self::search_text_state(dialog, input_kind);
        Some(UTF16Selection {
            range: Self::search_input_range_to_utf16(text, selection_range),
            reversed: false,
        })
    }

    /// 返回输入法当前组合文本范围。
    fn marked_text_range(
        &self,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let state = self.model_config_input_state(kind);
            return state
                .marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&state.text, range));
        }
        if self.ai_chat_input_focus.is_focused(window) {
            return self
                .ai_chat_input_marked_range
                .clone()
                .map(|range| Self::search_input_range_to_utf16(&self.ai_chat_input_text, range));
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            return self
                .settings
                .quick_search_keywords_input
                .marked_range
                .clone()
                .map(|range| {
                    Self::search_input_range_to_utf16(
                        &self.settings.quick_search_keywords_input.text,
                        range,
                    )
                });
        }
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            return self
                .settings
                .thread_analysis_filter_marked_range
                .clone()
                .map(|range| {
                    Self::search_input_range_to_utf16(
                        &self.settings.thread_analysis_filter_text,
                        range,
                    )
                });
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, _, marked_range) = Self::search_text_state(dialog, input_kind);
        marked_range.map(|range| Self::search_input_range_to_utf16(text, range))
    }

    /// 清除输入法组合文本状态。
    fn unmark_text(&mut self, window: &mut Window, context: &mut Context<Self>) {
        if let Some(kind) = self.active_model_config_input_kind(window) {
            self.model_config_input_state_mut(kind).marked_range = None;
            context.notify();
            return;
        }
        if self.ai_chat_input_focus.is_focused(window) {
            self.ai_chat_input_marked_range = None;
            context.notify();
            return;
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            self.settings.quick_search_keywords_input.marked_range = None;
            context.notify();
            return;
        }
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            self.settings.thread_analysis_filter_marked_range = None;
            context.notify();
            return;
        }
        let input_kind = self.active_search_text_input_kind(window);
        if let Some(dialog) = self.search.search_dialog.as_mut() {
            let (_, _, marked_range) = Self::search_text_state_mut(dialog, input_kind);
            *marked_range = None;
        }
        context.notify();
    }

    /// 用平台提交文本替换搜索框中的指定范围。
    ///
    /// 业务意图：
    /// - 中文 IME 候选词确认后会通过该入口提交最终文本，不能再依赖按键字符。
    /// - 替换范围优先采用平台指定范围，其次采用组合文本范围，最后使用普通选择范围。
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let replacement = Self::sanitize_search_input_text(text);
            let state = self.model_config_input_state_mut(kind);
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&state.text, range))
                .or_else(|| state.marked_range.clone())
                .unwrap_or_else(|| state.selection_range.clone());
            let range = Self::clamp_search_text_range(&state.text, range);
            state.text.replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            state.selection_range = cursor..cursor;
            state.marked_range = None;
            state.clear_layout();
            self.model_config.model_test_status = ModelTestStatus::Idle;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.ai_chat_input_focus.is_focused(window) {
            let replacement = text.replace("\r\n", "\n").replace('\r', "\n");
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&self.ai_chat_input_text, range))
                .or_else(|| self.ai_chat_input_marked_range.clone())
                .unwrap_or_else(|| self.ai_chat_input_selection_range.clone());
            let range = Self::clamp_search_text_range(&self.ai_chat_input_text, range);
            self.ai_chat_input_text
                .replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            self.ai_chat_input_selection_range = cursor..cursor;
            self.ai_chat_input_marked_range = None;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            if !self.settings.quick_search_keywords_is_editing {
                return;
            }
            let replacement = Self::sanitize_search_input_text(text);
            let range = range_utf16
                .map(|range| {
                    Self::search_input_range_from_utf16(
                        &self.settings.quick_search_keywords_input.text,
                        range,
                    )
                })
                .or_else(|| {
                    self.settings
                        .quick_search_keywords_input
                        .marked_range
                        .clone()
                })
                .unwrap_or_else(|| {
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .clone()
                });
            let range = Self::clamp_search_text_range(
                &self.settings.quick_search_keywords_input.text,
                range,
            );
            self.settings
                .quick_search_keywords_input
                .text
                .replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            self.settings.quick_search_keywords_input.selection_range = cursor..cursor;
            self.settings.quick_search_keywords_input.marked_range = None;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            if !self.settings.thread_analysis_filter_is_editing {
                return;
            }
            let replacement = normalize_thread_analysis_filter_text(text);
            let range = range_utf16
                .map(|range| {
                    Self::search_input_range_from_utf16(
                        &self.settings.thread_analysis_filter_text,
                        range,
                    )
                })
                .or_else(|| self.settings.thread_analysis_filter_marked_range.clone())
                .unwrap_or_else(|| self.settings.thread_analysis_filter_selection_range.clone());
            let range =
                Self::clamp_search_text_range(&self.settings.thread_analysis_filter_text, range);
            self.settings
                .thread_analysis_filter_text
                .replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            self.settings.thread_analysis_filter_selection_range = cursor..cursor;
            self.settings.thread_analysis_filter_marked_range = None;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        let input_kind = self.active_search_text_input_kind(window);
        let Some(dialog) = self.search.search_dialog.as_mut() else {
            return;
        };
        let replacement = Self::sanitize_search_input_text(text);
        let (target_text, selection_range, marked_range) =
            Self::search_text_state_mut(dialog, input_kind);
        let range = range_utf16
            .map(|range| Self::search_input_range_from_utf16(target_text, range))
            .or_else(|| marked_range.clone())
            .unwrap_or_else(|| selection_range.clone());
        let range = Self::clamp_search_text_range(target_text, range);
        target_text.replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        *selection_range = cursor..cursor;
        *marked_range = None;
        if input_kind == SearchTextInputKind::Query {
            dialog.query_history_menu_open = false;
            self.clear_search_current_file_match_count();
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 用平台组合文本替换搜索框中的指定范围，并保留组合状态。
    ///
    /// 边界条件：
    /// - 输入法可能多次更新同一段组合文本，必须优先替换旧 `marked_range`，避免拼音或候选词重复追加。
    /// - 搜索框只支持单行文本，因此组合文本中的换行会被移除。
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let replacement = Self::sanitize_search_input_text(new_text);
            let state = self.model_config_input_state_mut(kind);
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&state.text, range))
                .or_else(|| state.marked_range.clone())
                .unwrap_or_else(|| state.selection_range.clone());
            let range = Self::clamp_search_text_range(&state.text, range);
            state.text.replace_range(range.clone(), &replacement);

            if replacement.is_empty() {
                state.marked_range = None;
            } else {
                state.marked_range = Some(range.start..range.start + replacement.len());
            }

            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            state.selection_range = selected_range;
            state.clear_layout();
            self.model_config.model_test_status = ModelTestStatus::Idle;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.ai_chat_input_focus.is_focused(window) {
            let replacement = new_text.replace("\r\n", "\n").replace('\r', "\n");
            let range = range_utf16
                .map(|range| Self::search_input_range_from_utf16(&self.ai_chat_input_text, range))
                .or_else(|| self.ai_chat_input_marked_range.clone())
                .unwrap_or_else(|| self.ai_chat_input_selection_range.clone());
            let range = Self::clamp_search_text_range(&self.ai_chat_input_text, range);
            self.ai_chat_input_text
                .replace_range(range.clone(), &replacement);

            if replacement.is_empty() {
                self.ai_chat_input_marked_range = None;
            } else {
                self.ai_chat_input_marked_range =
                    Some(range.start..range.start + replacement.len());
            }

            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            self.ai_chat_input_selection_range = selected_range;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            if !self.settings.quick_search_keywords_is_editing {
                return;
            }
            let replacement = Self::sanitize_search_input_text(new_text);
            let range = range_utf16
                .map(|range| {
                    Self::search_input_range_from_utf16(
                        &self.settings.quick_search_keywords_input.text,
                        range,
                    )
                })
                .or_else(|| {
                    self.settings
                        .quick_search_keywords_input
                        .marked_range
                        .clone()
                })
                .unwrap_or_else(|| {
                    self.settings
                        .quick_search_keywords_input
                        .selection_range
                        .clone()
                });
            let range = Self::clamp_search_text_range(
                &self.settings.quick_search_keywords_input.text,
                range,
            );
            self.settings
                .quick_search_keywords_input
                .text
                .replace_range(range.clone(), &replacement);

            if replacement.is_empty() {
                self.settings.quick_search_keywords_input.marked_range = None;
            } else {
                self.settings.quick_search_keywords_input.marked_range =
                    Some(range.start..range.start + replacement.len());
            }

            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            self.settings.quick_search_keywords_input.selection_range = selected_range;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            if !self.settings.thread_analysis_filter_is_editing {
                return;
            }
            let replacement = normalize_thread_analysis_filter_text(new_text);
            let range = range_utf16
                .map(|range| {
                    Self::search_input_range_from_utf16(
                        &self.settings.thread_analysis_filter_text,
                        range,
                    )
                })
                .or_else(|| self.settings.thread_analysis_filter_marked_range.clone())
                .unwrap_or_else(|| self.settings.thread_analysis_filter_selection_range.clone());
            let range =
                Self::clamp_search_text_range(&self.settings.thread_analysis_filter_text, range);
            self.settings
                .thread_analysis_filter_text
                .replace_range(range.clone(), &replacement);

            if replacement.is_empty() {
                self.settings.thread_analysis_filter_marked_range = None;
            } else {
                self.settings.thread_analysis_filter_marked_range =
                    Some(range.start..range.start + replacement.len());
            }

            let selected_range = new_selected_range_utf16
                .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            self.settings.thread_analysis_filter_selection_range = selected_range;
            self.touch_search_text_cursor_activity();
            context.notify();
            return;
        }
        let input_kind = self.active_search_text_input_kind(window);
        let Some(dialog) = self.search.search_dialog.as_mut() else {
            return;
        };
        let replacement = Self::sanitize_search_input_text(new_text);
        let (target_text, selection_range, marked_range) =
            Self::search_text_state_mut(dialog, input_kind);
        let range = range_utf16
            .map(|range| Self::search_input_range_from_utf16(target_text, range))
            .or_else(|| marked_range.clone())
            .unwrap_or_else(|| selection_range.clone());
        let range = Self::clamp_search_text_range(target_text, range);
        target_text.replace_range(range.clone(), &replacement);

        if replacement.is_empty() {
            *marked_range = None;
        } else {
            *marked_range = Some(range.start..range.start + replacement.len());
        }

        let selected_range = new_selected_range_utf16
            .map(|utf16_range| Self::search_input_range_from_utf16(&replacement, utf16_range))
            .map(|relative_range| {
                range.start + relative_range.start..range.start + relative_range.end
            })
            .unwrap_or_else(|| {
                let cursor = range.start + replacement.len();
                cursor..cursor
            });
        *selection_range = selected_range;
        if input_kind == SearchTextInputKind::Query {
            dialog.query_history_menu_open = false;
            self.clear_search_current_file_match_count();
        }
        self.touch_search_text_cursor_activity();
        context.notify();
    }

    /// 返回指定文本范围在屏幕上的边界，用于放置 IME 候选窗口。
    ///
    /// 实现原因：
    /// - 搜索输入框采用 GPUI 官方示例的自定义文本元素实现，最近一次 `ShapedLine` 可以提供真实字符位置。
    /// - 如果首次绘制前布局不可用，则回退到输入框整体边界，保证候选窗口仍贴近控件。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let state = self.model_config_input_state(kind);
            let range = Self::search_input_range_from_utf16(&state.text, range_utf16);
            let Some(layout) = state.last_layout.as_ref() else {
                return Some(element_bounds);
            };
            return Some(Bounds::from_corners(
                point(
                    element_bounds.left() + layout.x_for_index(range.start),
                    element_bounds.top(),
                ),
                point(
                    element_bounds.left() + layout.x_for_index(range.end),
                    element_bounds.bottom(),
                ),
            ));
        }
        if self.ai_chat_input_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(&self.ai_chat_input_text, range_utf16);
            let cursor = range.start;
            for layout in &self.ai_chat_input_last_layouts {
                if cursor >= layout.byte_range.start && cursor <= layout.byte_range.end {
                    let x = layout
                        .line
                        .x_for_index(cursor.saturating_sub(layout.byte_range.start));
                    return Some(Bounds::new(
                        point(layout.bounds.left() + x, layout.bounds.top()),
                        size(px(1.0), layout.bounds.bottom() - layout.bounds.top()),
                    ));
                }
            }
            return Some(element_bounds);
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            let range = Self::search_input_range_from_utf16(
                &self.settings.quick_search_keywords_input.text,
                range_utf16,
            );
            let Some(layout) = self.settings.quick_search_keywords_last_layout.as_ref() else {
                return Some(element_bounds);
            };
            return Some(Bounds::from_corners(
                point(
                    element_bounds.left() + layout.x_for_index(range.start),
                    element_bounds.top(),
                ),
                point(
                    element_bounds.left() + layout.x_for_index(range.end),
                    element_bounds.bottom(),
                ),
            ));
        }
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            let range = Self::search_input_range_from_utf16(
                &self.settings.thread_analysis_filter_text,
                range_utf16,
            );
            let cursor = range.start;
            for layout in &self.settings.thread_analysis_filter_last_layouts {
                if cursor >= layout.byte_range.start && cursor <= layout.byte_range.end {
                    let x = layout
                        .line
                        .x_for_index(cursor.saturating_sub(layout.byte_range.start));
                    return Some(Bounds::new(
                        point(layout.bounds.left() + x, layout.bounds.top()),
                        size(px(1.0), layout.bounds.bottom() - layout.bounds.top()),
                    ));
                }
            }
            return Some(element_bounds);
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, _, _) = Self::search_text_state(dialog, input_kind);
        let range = Self::search_input_range_from_utf16(text, range_utf16);
        let layout = match input_kind {
            SearchTextInputKind::Query => self.search.search_query_last_layout.as_ref(),
            SearchTextInputKind::DirectoryTarget => {
                self.search.search_directory_last_layout.as_ref()
            }
        };
        let Some(layout) = layout else {
            return Some(element_bounds);
        };
        Some(Bounds::from_corners(
            point(
                element_bounds.left() + layout.x_for_index(range.start),
                element_bounds.top(),
            ),
            point(
                element_bounds.left() + layout.x_for_index(range.end),
                element_bounds.bottom(),
            ),
        ))
    }

    /// 根据鼠标位置返回文本插入点。
    ///
    /// 边界条件：
    /// - 平台 IME 可能通过该入口查询鼠标位置对应的字符；这里复用 GPUI 文本布局命中逻辑。
    /// - 如果布局尚不可用，则回退到文本末尾，避免平台输入协议收到非法下标。
    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<usize> {
        if let Some(kind) = self.active_model_config_input_kind(window) {
            let utf8_index = self.model_config_input_index_for_point(kind, point);
            let state = self.model_config_input_state(kind);
            return Some(Self::search_input_utf16_offset_from_byte(
                &state.text,
                utf8_index,
            ));
        }
        if self.ai_chat_input_focus.is_focused(window) {
            let utf8_index = self.ai_chat_input_index_for_point(point);
            return Some(Self::search_input_utf16_offset_from_byte(
                &self.ai_chat_input_text,
                utf8_index,
            ));
        }
        if self.settings.quick_search_keywords_focus.is_focused(window) {
            let utf8_index = self.quick_search_keywords_index_for_point(point);
            return Some(Self::search_input_utf16_offset_from_byte(
                &self.settings.quick_search_keywords_input.text,
                utf8_index,
            ));
        }
        if self
            .settings
            .thread_analysis_filter_focus
            .is_focused(window)
        {
            let utf8_index = self.thread_analysis_filter_index_for_point(point);
            return Some(Self::search_input_utf16_offset_from_byte(
                &self.settings.thread_analysis_filter_text,
                utf8_index,
            ));
        }
        let input_kind = self.active_search_text_input_kind(window);
        let dialog = self.search.search_dialog.as_ref()?;
        let (text, _, _) = Self::search_text_state(dialog, input_kind);
        let utf8_index = self.search_text_index_for_point(input_kind, point);
        Some(Self::search_input_utf16_offset_from_byte(text, utf8_index))
    }
}

impl Render for MainView {
    /// 渲染主窗口内容。
    ///
    /// 实现原因：
    /// - 左侧固定大导航提供全局入口，右侧根据当前主功能显示日志分析、HPROF 解析或 AI 对话占位页。
    /// - 日志加载拖拽仍挂在根节点，用户从任意功能页拖入日志时都会切回日志分析页并复用原加载流程。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette();

        div()
            .relative()
            .flex()
            .size_full()
            .bg(rgb(palette.background))
            .track_focus(&self.root_focus_handle)
            .key_context("main-view-root")
            .on_key_down(context.listener(Self::handle_root_key_down))
            .on_action(
                context.listener(|view, _: &OpenSearchDialog, window, context| {
                    view.schedule_open_search_dialog(window, context);
                }),
            )
            .on_mouse_move(context.listener(Self::handle_root_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(Self::handle_root_mouse_up),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(Self::handle_root_mouse_up),
            )
            .can_drop(|dragged, _window, _app| dragged.is::<ExternalPaths>())
            .on_drop(
                context.listener(|view, external_paths: &ExternalPaths, _window, context| {
                    // GPUI 会把系统文件拖放转成 `ExternalPaths`；这里只取真实文件系统路径，
                    // 目录、普通文件和压缩包的具体解释仍交给加载模块统一处理。
                    view.navigation.active_main_feature = MainFeature::LogAnalysis;
                    view.start_log_source_load(
                        external_paths.paths().to_vec(),
                        "正在加载拖入的日志".to_string(),
                        context,
                    );
                }),
            )
            .child(self.render_main_navigation(context))
            .child(
                div()
                    .id("main-feature-page")
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .bg(rgb(palette.background))
                    .child(self.render_main_feature_page(context)),
            )
            .child(self.render_main_navigation_tooltip_overlay())
            .child(self.render_load_source_menu_dismiss_overlay(context))
            .child(self.render_load_source_menu(context))
            .child(self.render_save_overwrite_confirm_dialog(context))
    }
}

/// 主窗口运行期状态。
///
/// 业务意图：
/// - macOS 关闭最后一个窗口后进程仍会保留，Dock 再次点按只会触发 reopen 事件，不会重新执行 `run`。
/// - 这里把“当前主窗口句柄”和“可从平台回调重新进入 GPUI 的异步应用句柄”集中保存，让启动、open-url 和 reopen
///   三条入口都能复用同一套主窗口恢复流程。
///
/// 边界条件：
/// - `WindowHandle` 本身不会让窗口继续存活；窗口关闭后句柄可能失效，因此每次使用前都必须通过 `update` 验证。
/// - `AsyncApp` 只在应用启动后可用；启动完成前收到的 macOS open-url 事件需要暂存，等主窗口初始化完成后再处理。
#[derive(Default)]
struct MainWindowRuntime {
    /// 当前仍可能有效的主窗口句柄。
    ///
    /// 业务意图：
    /// - 保存主窗口句柄是为了在 macOS reopen 或 Finder/Dock 打开文件时优先复用已有窗口。
    /// - 主窗口关闭回调会清空该字段；如果因为平台时序导致仍残留旧句柄，后续 `ensure_main_window` 会通过 `update`
    ///   失败识别并创建新窗口。
    main_window: Option<WindowHandle<MainView>>,

    /// 可在平台回调中重新进入 GPUI 主线程的应用句柄。
    ///
    /// 业务意图：
    /// - `Application::on_open_urls` 不直接提供 `App`，但 macOS 可能在应用已经启动后继续从 Finder 或 Dock 交付文件。
    /// - 保存 `AsyncApp` 后，open-url 回调可以在当前进程内恢复主窗口并加载路径，而不是因为主窗口关闭而静默丢弃请求。
    async_app: Option<AsyncApp>,
}

impl MainWindowRuntime {
    /// 记录当前可用的异步应用句柄。
    ///
    /// 边界条件：
    /// - 该方法只保存 GPUI 提供的弱引用包装，不持有窗口或实体所有权，因此不会阻止应用正常退出。
    fn remember_app(&mut self, app: &App) {
        self.async_app = Some(app.to_async());
    }

    /// 记录一个刚创建或刚验证过仍有效的主窗口。
    ///
    /// 业务意图：
    /// - 同时刷新 `AsyncApp`，保证后续 open-url 回调使用的是最新应用上下文。
    fn remember_window(&mut self, main_window: WindowHandle<MainView>, app: &App) {
        self.main_window = Some(main_window);
        self.remember_app(app);
    }

    /// 清空主窗口句柄。
    ///
    /// 边界条件：
    /// - 只清理窗口句柄，不清理 `AsyncApp`；macOS 关闭所有窗口后仍需要通过 `AsyncApp` 响应 Finder/Dock 事件。
    fn clear_window(&mut self) {
        self.main_window = None;
    }

    /// 返回可供平台回调使用的异步应用句柄快照。
    ///
    /// 边界条件：
    /// - 返回 clone 是为了立即释放 `RefCell` 借用，避免在后续进入 GPUI 更新流程时发生运行期借用冲突。
    fn async_app(&self) -> Option<AsyncApp> {
        self.async_app.clone()
    }
}

/// 构造主窗口选项。
///
/// 业务意图：
/// - 启动创建窗口和 macOS reopen 恢复窗口必须使用同一套标题、尺寸和显示器策略。
/// - 把 `WindowOptions` 集中到这里可以避免后续新增菜单栏、最小尺寸或平台差异时只改了一条入口。
fn build_main_window_options(app: &App) -> WindowOptions {
    WindowOptions {
        // 显式设置系统标题栏标题，保证 macOS 和 Windows 的原生窗口标题都使用产品名。
        // 后续如果标题需要包含文件名或状态，应在业务规则明确后统一修改这里的标题策略。
        titlebar: Some(TitlebarOptions {
            title: Some(MAIN_WINDOW_TITLE.into()),
            ..Default::default()
        }),
        // 使用统一启动策略决定主窗口边界：历史宽高优先，其次小屏最大化，最后大屏固定 1600x900 居中。
        // 这里不恢复历史位置，并且显式绑定主显示器，避免多屏环境下计算居中和实际打开使用不同屏幕。
        window_bounds: Some(build_main_window_bounds(app)),
        display_id: primary_display_id(app),
        ..Default::default()
    }
}

/// 在主窗口关闭前保存可恢复的窗口尺寸。
///
/// 业务意图：
/// - 用户手动调整后的普通窗口尺寸应跨会话保留，保证日志查看工作区再次打开时仍符合用户习惯。
///
/// 边界条件：
/// - 最大化和全屏是平台窗口状态，不是用户希望下次以超大普通窗口打开的尺寸，因此跳过保存。
/// - 保存失败不阻止关闭，避免配置目录权限问题导致用户无法关闭日志查看客户端。
fn save_main_window_size_before_close(window: &Window) {
    if window.is_maximized() || window.is_fullscreen() {
        return;
    }

    let bounds = window.window_bounds().get_bounds();
    let width = bounds.size.width / px(1.0);
    let height = bounds.size.height / px(1.0);
    if let Some(size) = MainWindowSizePreference::new(width, height) {
        save_main_window_size_preference(size);
    }
}

/// 查找当前仍打开的主窗口。
///
/// 业务意图：
/// - 防御运行期状态丢失或句柄缓存被清空但窗口仍存在的情况，避免 macOS reopen 或 open-url 创建重复主窗口。
///
/// 边界条件：
/// - `AnyWindowHandle::downcast` 只按根视图类型判断；当前应用只有一个 `MainView` 主窗口，辅助窗口使用独立根视图类型。
fn find_open_main_window(app: &App) -> Option<WindowHandle<MainView>> {
    app.windows()
        .into_iter()
        .find_map(|window| window.downcast::<MainView>())
}

/// 激活一个已知主窗口，并确认句柄仍有效。
///
/// 业务意图：
/// - macOS reopen 和 Finder/Dock 打开文件时，如果主窗口仍存在，应优先把它带回前台，而不是创建第二个主窗口。
///
/// 边界条件：
/// - 如果窗口已经关闭或根视图类型不匹配，`update` 会失败；调用方据此清理缓存并创建新窗口。
fn activate_main_window(main_window: WindowHandle<MainView>, app: &mut App) -> bool {
    main_window
        .update(app, |view, window, _context| {
            view.main_window = Some(main_window);
            window.activate_window();
        })
        .is_ok()
}

/// 为主窗口安装全局快捷键拦截器。
///
/// 业务意图：
/// - GPUI 的应用级拦截器需要绑定一个当前有效的主窗口句柄，才能把 `Cmd+F`、复制等跨焦点快捷键派发回主视图。
/// - macOS 关闭主窗口后旧 `MainView` 会释放订阅；reopen 创建新主窗口时必须重新安装，否则新窗口的全局快捷键会失效。
///
/// 边界条件：
/// - 拦截器捕获的更新异常不能越过 Objective-C key equivalent 边界，否则 macOS 运行时可能直接 abort。
/// - 订阅保存到主视图里，让窗口关闭时自动释放，不需要额外的全局清理逻辑。
fn install_main_window_keystroke_subscription(main_window: WindowHandle<MainView>, app: &mut App) {
    let main_view_for_keys = main_window;
    let subscription = app.intercept_keystrokes(move |event, window, app| {
        // GPUI 0.2.2 在 macOS 上会从 Objective-C `keyEquivalent` 回调进入这里；该回调不能让 Rust panic
        // 继续向外 unwind，否则运行时会直接 abort。快捷键处理本身不是不可恢复业务，因此这里在边界处兜住
        // 我们自己的状态更新异常，并让事件继续按默认路径传播，避免一次快捷键输入击穿整个进程。
        let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            main_view_for_keys
                .update(app, |view, _window, context| {
                    // 应用级拦截器服务搜索、复制和粘贴这类跨焦点快捷键；区域键盘滚动只允许主窗口根节点处理，
                    // 避免设置窗口或其它辅助窗口按 PageUp/PageDown 时滚动背后的日志内容并消费事件。
                    view.handle_global_keystroke(event.keystroke.clone(), window, context, false)
                })
                .unwrap_or(false)
        }))
        .unwrap_or(false);
        if handled {
            app.stop_propagation();
        }
    });
    main_window
        .update(app, |view, _window, _context| {
            view.global_keystroke_subscription = Some(subscription);
        })
        .ok();
}

/// 创建新的主窗口。
///
/// 业务意图：
/// - 启动和 macOS reopen 都通过该函数创建窗口，保证主题观察、关闭保存、焦点和全局快捷键订阅完全一致。
///
/// 边界条件：
/// - 该函数只创建空主窗口；启动参数、Finder/Dock 打开的路径由调用方在窗口创建成功后再加载，避免窗口创建失败时丢失错误边界。
fn create_main_window(
    runtime: &Rc<RefCell<MainWindowRuntime>>,
    app: &mut App,
) -> Result<WindowHandle<MainView>, String> {
    let window_options = build_main_window_options(app);
    let runtime_for_close = Rc::clone(runtime);
    let main_window = app
        .open_window(window_options, move |window, app| {
            let view = app.new(|context| {
                let mut view = MainView::new(context);
                view.system_window_appearance = window.appearance();
                view.window_appearance_subscription = Some(context.observe_window_appearance(
                    window,
                    |view, window, context| {
                        view.set_system_window_appearance(window.appearance(), context);
                    },
                ));
                view
            });
            window.on_window_should_close(app, move |window, _app| {
                save_main_window_size_before_close(window);
                runtime_for_close.borrow_mut().clear_window();
                true
            });
            window.focus(&view.read(app).root_focus_handle);
            view
        })
        .map_err(|error| format!("创建 LogClinic 主窗口失败：{error}"))?;

    main_window
        .update(app, |view, _window, context| {
            // 主窗口句柄只能在 `open_window` 成功返回后获得；回填到主视图供独立工具窗口激活主窗口使用。
            view.main_window = Some(main_window);
            context.notify();
        })
        .map_err(|error| format!("初始化 LogClinic 主窗口状态失败：{error}"))?;
    install_main_window_keystroke_subscription(main_window, app);
    runtime.borrow_mut().remember_window(main_window, app);

    Ok(main_window)
}

/// 确保当前进程内有可用主窗口。
///
/// 业务意图：
/// - macOS 关闭所有窗口后进程不退出，Dock 再点时必须恢复主窗口。
/// - Finder/Dock 在无窗口状态下再次打开日志文件时，也必须先恢复窗口再加载文件。
///
/// 边界条件：
/// - 优先验证缓存句柄，失败后再扫描 GPUI 当前窗口列表，最后才创建新窗口，避免重复窗口。
fn ensure_main_window(
    runtime: &Rc<RefCell<MainWindowRuntime>>,
    app: &mut App,
) -> Result<WindowHandle<MainView>, String> {
    let cached_main_window = runtime.borrow().main_window;
    if let Some(main_window) = cached_main_window {
        if activate_main_window(main_window, app) {
            runtime.borrow_mut().remember_window(main_window, app);
            return Ok(main_window);
        }
        runtime.borrow_mut().clear_window();
    }

    if let Some(main_window) = find_open_main_window(app)
        && activate_main_window(main_window, app)
    {
        runtime.borrow_mut().remember_window(main_window, app);
        return Ok(main_window);
    }

    create_main_window(runtime, app)
}

/// 在主窗口中加载一组日志路径。
///
/// 业务意图：
/// - 启动参数、macOS open-url 和延迟处理的 pending URL 都应走同一条加载入口，保证文件、目录和压缩包行为一致。
///
/// 边界条件：
/// - 空路径集合不触发加载，避免覆盖用户当前工作区。
/// - 如果窗口在平台回调和加载之间被关闭，`update` 失败即可忽略，避免平台回调引发 panic。
fn load_paths_in_main_window(
    main_window: WindowHandle<MainView>,
    app: &mut App,
    paths: Vec<PathBuf>,
    message: &str,
) {
    if paths.is_empty() {
        return;
    }

    let message = message.to_string();
    main_window
        .update(app, |view, window, context| {
            view.start_log_source_load(paths, message, context);
            window.activate_window();
        })
        .ok();
}

/// 程序入口。
///
/// 业务意图：
/// - 初始化 GPUI 应用并创建唯一主窗口。
/// - macOS 关闭所有窗口后进程仍保留，因此额外注册 reopen 处理，在 Dock 再点时恢复主窗口。
///
/// 错误处理：
/// - 启动期主窗口创建失败意味着桌面应用无法进入可交互状态，属于不可恢复错误。
/// - macOS reopen 或 open-url 回调中的窗口恢复失败只能写入 stderr，因为此时可能没有任何可展示错误的窗口。
pub(crate) fn run() {
    let application = Application::new();
    let main_window_runtime: Rc<RefCell<MainWindowRuntime>> =
        Rc::new(RefCell::new(MainWindowRuntime::default()));
    let pending_open_urls: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let main_window_runtime = Rc::clone(&main_window_runtime);
        let pending_open_urls = Rc::clone(&pending_open_urls);
        application.on_open_urls(move |urls| {
            let paths = log_source_paths_from_open_urls(urls.clone());
            if paths.is_empty() {
                return;
            }

            let async_app = main_window_runtime.borrow().async_app();
            let Some(async_app) = async_app else {
                // 某些平台可能在主窗口创建前触发 open-url；先暂存，主窗口完成初始化后再统一处理。
                pending_open_urls.borrow_mut().extend(urls);
                return;
            };

            let main_window_runtime = Rc::clone(&main_window_runtime);
            async_app
                .update(move |app| {
                    main_window_runtime.borrow_mut().remember_app(app);
                    match ensure_main_window(&main_window_runtime, app) {
                        Ok(main_window) => {
                            load_paths_in_main_window(main_window, app, paths, "正在加载拖入的日志")
                        }
                        Err(error) => {
                            eprintln!("macOS open-url 恢复 LogClinic 主窗口失败：{error}");
                        }
                    }
                })
                .ok();
        });
    }
    {
        let main_window_runtime = Rc::clone(&main_window_runtime);
        application.on_reopen(move |app| {
            main_window_runtime.borrow_mut().remember_app(app);
            match ensure_main_window(&main_window_runtime, app) {
                Ok(_main_window) => {
                    // macOS Dock 再次点按应用图标时，应用可能处于后台；显式激活保证恢复出的主窗口可见。
                    app.activate(true);
                }
                Err(error) => {
                    eprintln!("macOS reopen 恢复 LogClinic 主窗口失败：{error}");
                }
            }
        });
    }

    application.run(move |app| {
        // 清理异常退出遗留的超大日志物化目录，避免压缩包大成员长期占用系统临时磁盘。
        cleanup_stale_large_log_cache();
        // 启动参数在 Windows 拖拽到程序图标、开发期命令行启动等场景中承载待打开路径。
        // macOS Dock/Finder 的“用应用打开”通常走下方 `on_open_urls` 回调，因此两条入口都保留。
        let launch_paths = log_source_paths_from_launch_arguments();
        app.bind_keys([
            KeyBinding::new("ctrl-f", OpenSearchDialog, None),
            KeyBinding::new("cmd-f", OpenSearchDialog, None),
        ]);
        // 注册 Lucide 图标字体和内置 JetBrains Mono 正文字体，确保 macOS 和 Windows 上的图标与日志等宽字体
        // 不依赖运行环境预装字体；如果注册失败，核心界面视觉无法可靠渲染，启动期应直接暴露错误。
        app.text_system()
            .add_fonts(vec![
                Cow::Borrowed(LUCIDE_FONT_BYTES),
                Cow::Borrowed(JETBRAINS_MONO_REGULAR_FONT_BYTES),
            ])
            .expect("注册内置字体失败，工具栏图标或日志正文等宽字体无法可靠渲染");
        main_window_runtime.borrow_mut().remember_app(app);

        let main_window = ensure_main_window(&main_window_runtime, app)
            .expect("创建 LogClinic 主窗口失败，应用无法继续启动");
        load_paths_in_main_window(main_window, app, launch_paths, "正在加载启动传入的日志");

        let pending_paths = log_source_paths_from_open_urls(pending_open_urls.take());
        if !pending_paths.is_empty() {
            load_paths_in_main_window(main_window, app, pending_paths, "正在加载拖入的日志");
        }
    });
}
