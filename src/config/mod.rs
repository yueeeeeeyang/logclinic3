// 应用配置路径、解析和持久化功能域。
//
// 业务意图：
// - 该文件集中维护主窗口尺寸、主题、日志字号、线程过滤、快搜关键字和模型配置的磁盘读写规则。
// - 配置格式、默认值和错误回退策略保持原样；GPUI 设置页只调用这里的读写和规范化接口。
//
// 边界条件：
// - 写入失败只输出诊断，不阻断当前 UI 会话，避免配置目录权限问题影响日志查看主流程。
// - 所有函数限制在 crate 内部使用，不形成公开 API，避免磁盘格式在未确认迁移策略前被外部依赖。

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::theme::ThemePreference;

/// 主窗口尺寸偏好文件名。
///
/// 业务意图：
/// - 当前只保存主窗口宽高，不保存位置、最大化状态或其他设置，因此使用独立小文本文件即可。
/// - 如果后续接入完整设置系统，应迁移到统一配置文件并保留兼容读取逻辑。
pub(crate) const MAIN_WINDOW_SIZE_FILE_NAME: &str = "window-size.txt";

/// 主题偏好文件名。
///
/// 业务意图：
/// - 主题属于用户明确设置，必须和窗口大小一样跨启动保留。
/// - 文件内容保持为简单英文枚举值，避免仅为单个配置新增 JSON/TOML 依赖。
pub(crate) const THEME_PREFERENCE_FILE_NAME: &str = "theme-preference.txt";

/// 日志显示字号偏好文件名。
///
/// 业务意图：
/// - 日志字号是用户明确调整的阅读偏好，需要像主题一样跨启动恢复。
/// - 文件只保存一个像素值，继续使用简单文本格式，避免为单项设置引入完整配置依赖。
pub(crate) const LOG_VIEWER_FONT_SIZE_FILE_NAME: &str = "log-viewer-font-size.txt";

/// 线程日志分析过滤配置文件名。
///
/// 业务意图：
/// - 用户会在设置窗口中粘贴需要过滤的线程堆栈，配置必须跨重启保留，避免每次排查都重新维护无效线程列表。
/// - 文件保存原始多行文本而不是结构化格式，方便用户直接打开配置文件排查或批量替换。
pub(crate) const THREAD_ANALYSIS_FILTER_FILE_NAME: &str = "thread-analysis-filter.txt";

/// 快搜关键字配置文件名。
///
/// 业务意图：
/// - 快搜关键字是用户面向排障场景维护的常用搜索词集合，需要跨应用重启保留。
/// - 文件保存英文逗号分隔的单行文本，保持可手工编辑，同时避免为一个简单列表引入结构化配置依赖。
pub(crate) const QUICK_SEARCH_KEYWORDS_FILE_NAME: &str = "quick-search-keywords.txt";

/// 模型配置文件名。
///
/// 业务意图：
/// - 模型配置包含多个 OpenAI 兼容接口档案和默认模型选择，需要跨应用重启恢复。
/// - 文件使用 JSON 而不是多个文本文件，便于一次性保存列表、默认 ID 和 API Key 等结构化字段。
///
/// 安全边界：
/// - 用户已确认第一版 API Key 明文保存在应用配置目录；UI 默认掩码显示，代码中避免把 Key 写入错误文案。
pub(crate) const MODEL_CONFIGS_FILE_NAME: &str = "model-configs.json";

/// 测试模型接口的超时时间。
///
/// 业务意图：
/// - 测试按钮只用于快速确认配置是否可用，不能因为网络不可达或本地服务无响应长期占用后台线程。
/// - 20 秒是需求确认的默认超时，既兼容本地模型冷启动，也能让 UI 尽快反馈失败。
pub(crate) const MODEL_TEST_TIMEOUT_SECONDS: u64 = 20;

/// OpenAI 兼容 Chat Completions 路径。
///
/// 业务意图：
/// - `base_url` 约定为 API 根路径，例如 `https://api.openai.com/v1`，测试和真实 AI 请求统一拼接该相对路径。
/// - 单独定义后测试和真实请求共用一套拼接规则，避免尾斜杠处理不一致。
pub(crate) const MODEL_TEST_CHAT_COMPLETIONS_PATH: &str = "chat/completions";

/// 日志正文默认字号。
///
/// 边界条件：
/// - 用户要求默认字号为 12px；当前继续沿用 22px 行高，保证单屏可见行数和滚动计算稳定。
/// - 设置页只调整文字字号，不改变固定行高和信息密度。
pub(crate) const LOG_VIEWER_DEFAULT_FONT_SIZE: f32 = 12.0;

/// 日志正文可设置的最小字号。
///
/// 边界条件：
/// - 过小字体会导致中文、英文和符号在长时间阅读时难以辨认，因此设置页不允许继续减小。
pub(crate) const LOG_VIEWER_MIN_FONT_SIZE: f32 = 10.0;

/// 日志正文可设置的最大字号。
///
/// 边界条件：
/// - 当前日志行高仍保持固定密度；过大字体可能与行高冲突并影响虚拟列表测量，因此先限制到 20px。
pub(crate) const LOG_VIEWER_MAX_FONT_SIZE: f32 = 20.0;

/// 快搜默认关键字配置。
///
/// 业务意图：
/// - 首次使用快搜时内置常见导入、导出、转换和水印相关排障词，用户不需要先进入设置维护才能使用快搜。
///
/// 边界条件：
/// - 该默认值只在配置文件不存在时使用；如果用户保存空配置，会写入空文件，后续启动必须尊重用户显式选择。
/// - 关键字只使用英文逗号分隔，和快搜配置解析规则保持一致。
pub(crate) const DEFAULT_QUICK_SEARCH_KEYWORDS_TEXT: &str =
    "excel,import,export,wbi,convertFile,waterMark";

/// 线程日志分析默认过滤配置。
///
/// 业务意图：
/// - Resin 的网络 accept 和 keepalive 线程在大量 thread dump 中经常长期存在，通常不代表业务阻塞根因。
/// - 首次使用线程分析时默认过滤这些稳定噪声线程，减少时间线中的无效线程；用户仍可在设置页清空或改写配置文件。
///
/// 边界条件：
/// - 该默认值只在配置文件不存在时使用；如果用户点击“清空”，会写入空文件，后续启动必须尊重用户显式选择。
/// - 文本使用 LF 作为内置换行，粘贴或保存路径仍会通过统一规范化函数处理 CRLF。
pub(crate) const DEFAULT_THREAD_ANALYSIS_FILTER_TEXT: &str = concat!(
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

/// OpenAI 兼容模型配置文件的单条档案。
///
/// 业务意图：
/// - 每条档案保存一个可命名的 API 端点和模型 ID，用户可以在不同 OpenAI 兼容服务之间切换。
/// - 字段保持简单字符串，便于 JSON 配置手工排查和后续迁移。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelProfile {
    /// 稳定 ID，用于列表选择和默认模型引用；不直接展示给用户。
    pub(crate) id: String,
    /// 用户可读配置名称。
    pub(crate) name: String,
    /// OpenAI 兼容 API 根路径，例如 `https://api.openai.com/v1`。
    pub(crate) base_url: String,
    /// API Key，允许为空以兼容不需要鉴权的本地服务。
    pub(crate) api_key: String,
    /// Chat Completions 请求体中的模型 ID。
    pub(crate) model: String,
}

/// 模型配置文件的完整结构。
///
/// 业务意图：
/// - `profiles` 保存多条配置，`default_profile_id` 保存当前默认配置的稳定 ID。
/// - 删除或读取损坏配置时会通过规范化逻辑清理悬空默认 ID，避免 UI 指向不存在的配置。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ModelConfigs {
    /// 已保存的模型配置列表。
    pub(crate) profiles: Vec<ModelProfile>,
    /// 当前默认配置 ID；没有默认或默认配置被删除时为 `None`。
    pub(crate) default_profile_id: Option<String>,
}

/// 校验模型配置表单的必填字段和 URL 协议。
///
/// 业务意图：
/// - 保存、测试和 AI 对话流式请求都必须使用同一套校验，避免 UI 能保存但后台请求使用非法 URL。
/// - API Key 明确允许为空，以兼容本地 Ollama/vLLM 等不需要 Bearer 鉴权的服务。
pub(crate) fn validate_model_profile_fields(
    name: &str,
    base_url: &str,
    model: &str,
) -> Result<(), String> {
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

/// 拼接 OpenAI 兼容 Chat Completions 请求 URL。
///
/// 边界条件：
/// - 用户可能在 Base URL 末尾输入一个或多个 `/`，拼接时统一去掉末尾斜杠，避免出现双斜杠路径。
pub(crate) fn model_test_chat_completions_url(base_url: &str) -> Result<String, String> {
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
pub(crate) fn model_test_request_body(model: &str) -> serde_json::Value {
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

/// 返回 OpenAI 兼容请求需要发送的 Authorization 头。
///
/// 业务意图：
/// - API Key 非空才发送 Bearer header，避免本地模型服务因为无意义空鉴权头拒绝请求。
pub(crate) fn model_test_authorization_header(api_key: &str) -> Option<String> {
    let api_key = api_key.trim();
    (!api_key.is_empty()).then(|| format!("Bearer {api_key}"))
}

/// 判断模型测试响应是否包含 Chat Completions 的 choices。
///
/// 边界条件：
/// - 只要求 `choices` 是数组，不强制数组非空；部分兼容服务在 `max_tokens=1` 下仍可能返回空内容但格式有效。
pub(crate) fn model_test_response_has_choices(value: &serde_json::Value) -> bool {
    value
        .get("choices")
        .is_some_and(|choices| choices.is_array())
}

/// 将 HTTP 错误响应体裁剪成适合 UI 展示的短文本。
///
/// 业务意图：
/// - 兼容服务可能返回很长的 JSON 错误，设置窗口只需要展示可理解的前段原因，避免撑破状态栏。
pub(crate) fn model_test_http_error_body_snippet(body: &str) -> String {
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
pub(crate) fn test_openai_compatible_model(profile: ModelProfile) -> Result<String, String> {
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

/// 可持久化的主窗口宽高。
///
/// 业务意图：
/// - 该结构只表达用户最后调整过的窗口内容宽高，启动时会重新居中，不恢复历史位置。
/// - 宽高使用 GPUI 逻辑像素，避免把平台物理像素、DPI 缩放或窗口装饰尺寸写入业务配置。
///
/// 边界条件：
/// - 宽高必须是有限正数；零、负数、NaN 和无穷大都视为损坏配置并丢弃。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MainWindowSizePreference {
    /// 主窗口宽度，单位为 GPUI 逻辑像素。
    pub(crate) width: f32,
    /// 主窗口高度，单位为 GPUI 逻辑像素。
    pub(crate) height: f32,
}

impl MainWindowSizePreference {
    /// 创建合法的主窗口宽高偏好。
    ///
    /// 业务意图：
    /// - 所有读入、测试和关闭保存路径都经过同一个校验入口，避免损坏配置在下次启动时造成不可见窗口。
    /// - 当前只按“有限正数”校验；如果后续定义最小窗口尺寸，应在这里统一收紧规则。
    pub(crate) fn new(width: f32, height: f32) -> Option<Self> {
        if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
            Some(Self { width, height })
        } else {
            None
        }
    }
}

/// 解析主窗口宽高偏好文件内容。
///
/// 文件格式：
/// - 第一列为宽度，第二列为高度，中间使用空白字符分隔。
/// - 不使用 JSON/TOML 是为了避免仅为一个内部小配置新增依赖。
///
/// 边界条件：
/// - 格式错误、缺少字段、多余字段、非法浮点数或非正尺寸都返回 `None`，让启动流程回退默认策略。
pub(crate) fn parse_main_window_size_preference(raw: &str) -> Option<MainWindowSizePreference> {
    let mut parts = raw.split_whitespace();
    let width = parts.next()?.parse::<f32>().ok()?;
    let height = parts.next()?.parse::<f32>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    MainWindowSizePreference::new(width, height)
}

/// 序列化主窗口宽高偏好。
///
/// 业务意图：
/// - 固定使用简单空格分隔，便于人工排查配置损坏，也便于测试按文本断言。
pub(crate) fn serialize_main_window_size_preference(size: MainWindowSizePreference) -> String {
    format!("{} {}\n", size.width.round(), size.height.round())
}

/// 从指定文件读取主窗口宽高偏好。
///
/// 错误处理：
/// - 配置文件缺失、无权限读取或内容损坏都不阻止应用启动。
/// - 启动窗口大小不是核心日志查看能力，因此错误统一视为无历史尺寸。
pub(crate) fn read_main_window_size_preference(path: &Path) -> Option<MainWindowSizePreference> {
    let raw = fs::read_to_string(path).ok()?;
    parse_main_window_size_preference(&raw)
}

/// 将主窗口宽高偏好写入指定文件。
///
/// 错误处理：
/// - 调用者可以选择忽略错误，因为窗口关闭阶段不应因配置目录权限问题阻止退出。
/// - 这里仍返回 `io::Result`，方便测试覆盖目录创建和文件写入失败的边界。
pub(crate) fn write_main_window_size_preference(
    path: &Path,
    size: MainWindowSizePreference,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serialize_main_window_size_preference(size))
}

/// 获取当前平台的主窗口宽高偏好文件路径。
///
/// 跨平台约束：
/// - macOS 使用 `$HOME/Library/Application Support/LogClinic`，符合普通桌面应用配置目录习惯。
/// - Windows 使用 `%APPDATA%\LogClinic`，避免写入程序安装目录或当前工作目录。
/// - 其他平台当前不是目标运行平台，返回 `None` 并退回默认窗口策略。
pub(crate) fn main_window_size_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(MAIN_WINDOW_SIZE_FILE_NAME))
}

/// 获取当前平台的应用配置目录。
///
/// 跨平台约束：
/// - macOS 使用 `$HOME/Library/Application Support/LogClinic`，符合普通桌面应用配置目录习惯。
/// - Windows 使用 `%APPDATA%\LogClinic`，避免写入程序安装目录或当前工作目录。
/// - 其他平台当前不是目标运行平台，返回 `None` 并退回内存默认值。
pub(crate) fn app_config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        env::var_os("HOME").map(PathBuf::from).map(|home| {
            home.join("Library")
                .join("Application Support")
                .join("LogClinic")
        })
    }

    #[cfg(target_os = "windows")]
    {
        env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|app_data| app_data.join("LogClinic"))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

/// 读取主窗口宽高偏好。
///
/// 业务意图：
/// - 该函数作为主入口的读配置边界，隔离平台路径选择和配置文件损坏处理。
pub(crate) fn load_main_window_size_preference() -> Option<MainWindowSizePreference> {
    let path = main_window_size_preference_path()?;
    read_main_window_size_preference(&path)
}

/// 保存主窗口宽高偏好。
///
/// 错误处理：
/// - 保存失败不会影响用户关闭应用；该偏好只是体验优化，不属于日志读取、解析或展示的核心数据。
/// - 开发调试时通过 stderr 暴露失败原因，便于定位权限或路径环境变量问题。
pub(crate) fn save_main_window_size_preference(size: MainWindowSizePreference) {
    let Some(path) = main_window_size_preference_path() else {
        return;
    };
    if let Err(error) = write_main_window_size_preference(&path, size) {
        eprintln!("保存主窗口尺寸偏好失败：{}：{}", path.display(), error);
    }
}

/// 获取主题偏好文件路径。
pub(crate) fn theme_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(THEME_PREFERENCE_FILE_NAME))
}

/// 解析主题偏好配置文本。
///
/// 边界条件：
/// - 配置文件可能被用户手工修改或写入中断破坏；未知值统一视为 `None`，调用方回退到“跟随系统”。
pub(crate) fn parse_theme_preference(raw: &str) -> Option<ThemePreference> {
    match raw.trim() {
        "light" => Some(ThemePreference::Light),
        "dark" => Some(ThemePreference::Dark),
        "system" => Some(ThemePreference::System),
        _ => None,
    }
}

/// 序列化主题偏好。
pub(crate) fn serialize_theme_preference(preference: ThemePreference) -> String {
    format!("{}\n", preference.as_config_value())
}

/// 从指定文件读取主题偏好。
///
/// 错误处理：
/// - 文件缺失、读取失败或内容损坏都不阻止应用启动，统一由调用方回退到“跟随系统”。
pub(crate) fn read_theme_preference(path: &Path) -> Option<ThemePreference> {
    let raw = fs::read_to_string(path).ok()?;
    parse_theme_preference(&raw)
}

/// 将主题偏好写入指定文件。
pub(crate) fn write_theme_preference(path: &Path, preference: ThemePreference) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serialize_theme_preference(preference))
}

/// 读取主题偏好。
///
/// 业务意图：
/// - 主题是设置窗口中明确提供的用户偏好，应跨启动恢复；配置不可用时默认跟随系统。
pub(crate) fn load_theme_preference() -> ThemePreference {
    theme_preference_path()
        .and_then(|path| read_theme_preference(&path))
        .unwrap_or(ThemePreference::System)
}

/// 保存主题偏好。
///
/// 错误处理：
/// - 写入失败不影响当前会话的主题切换，仅在 stderr 输出诊断信息，避免配置目录权限问题阻断 UI 操作。
pub(crate) fn save_theme_preference(preference: ThemePreference) {
    let Some(path) = theme_preference_path() else {
        return;
    };
    if let Err(error) = write_theme_preference(&path, preference) {
        eprintln!("保存主题偏好失败：{}：{}", path.display(), error);
    }
}

/// 获取日志显示字号偏好文件路径。
pub(crate) fn log_viewer_font_size_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(LOG_VIEWER_FONT_SIZE_FILE_NAME))
}

/// 规范化日志显示字号。
///
/// 业务意图：
/// - 设置页、配置读取和测试都通过同一套边界规则，避免 UI 可选范围与磁盘配置可接受范围不一致。
///
/// 边界条件：
/// - NaN、无穷大、过小或过大的值都视为无效配置；合法值按 1px 粒度取整，保证设置按钮显示稳定整数 px。
pub(crate) fn normalize_log_viewer_font_size(value: f32) -> Option<f32> {
    if !value.is_finite() {
        return None;
    }
    let rounded = value.round();
    if (LOG_VIEWER_MIN_FONT_SIZE..=LOG_VIEWER_MAX_FONT_SIZE).contains(&rounded) {
        Some(rounded)
    } else {
        None
    }
}

/// 解析日志显示字号配置文本。
///
/// 边界条件：
/// - 配置文件可能被用户手工修改；格式错误、空文本和超出范围都返回 `None`，调用方回退默认 12px。
pub(crate) fn parse_log_viewer_font_size_preference(raw: &str) -> Option<f32> {
    normalize_log_viewer_font_size(raw.trim().parse::<f32>().ok()?)
}

/// 序列化日志显示字号配置。
pub(crate) fn serialize_log_viewer_font_size_preference(font_size: f32) -> String {
    format!("{}\n", font_size.round())
}

/// 从指定文件读取日志显示字号。
///
/// 错误处理：
/// - 文件缺失、读取失败或内容损坏都不影响应用启动，统一由调用方回退默认字号。
pub(crate) fn read_log_viewer_font_size_preference(path: &Path) -> Option<f32> {
    let raw = fs::read_to_string(path).ok()?;
    parse_log_viewer_font_size_preference(&raw)
}

/// 将日志显示字号写入指定文件。
pub(crate) fn write_log_viewer_font_size_preference(path: &Path, font_size: f32) -> io::Result<()> {
    let Some(font_size) = normalize_log_viewer_font_size(font_size) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "日志显示字号超出允许范围",
        ));
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serialize_log_viewer_font_size_preference(font_size))
}

/// 读取日志显示字号偏好。
///
/// 业务意图：
/// - 日志正文是应用最核心的长时间阅读区域，用户调整字号后应在下次启动恢复。
pub(crate) fn load_log_viewer_font_size_preference() -> f32 {
    log_viewer_font_size_preference_path()
        .and_then(|path| read_log_viewer_font_size_preference(&path))
        .unwrap_or(LOG_VIEWER_DEFAULT_FONT_SIZE)
}

/// 保存日志显示字号偏好。
///
/// 错误处理：
/// - 写入失败不影响当前会话的字号调整，仅输出开发期诊断，避免配置目录权限问题阻断 UI 操作。
pub(crate) fn save_log_viewer_font_size_preference(font_size: f32) {
    let Some(path) = log_viewer_font_size_preference_path() else {
        return;
    };
    if let Err(error) = write_log_viewer_font_size_preference(&path, font_size) {
        eprintln!("保存日志显示字号偏好失败：{}：{}", path.display(), error);
    }
}

/// 获取线程日志分析过滤配置文件路径。
pub(crate) fn thread_analysis_filter_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(THREAD_ANALYSIS_FILTER_FILE_NAME))
}

/// 获取快搜关键字配置文件路径。
pub(crate) fn quick_search_keywords_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(QUICK_SEARCH_KEYWORDS_FILE_NAME))
}

/// 获取模型配置文件路径。
pub(crate) fn model_configs_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(MODEL_CONFIGS_FILE_NAME))
}

/// 规范化线程日志分析过滤配置文本。
///
/// 业务意图：
/// - 用户可能从 Windows、macOS、终端或网页复制堆栈，换行格式不稳定；内部统一使用 LF，保证规则拆分和匹配可预测。
/// - 不裁剪首尾空白，避免破坏用户粘贴的原始堆栈文本；真正匹配时再按行去首尾空白。
pub(crate) fn normalize_thread_analysis_filter_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// 从指定文件读取线程日志分析过滤配置。
///
/// 错误处理：
/// - 配置缺失时使用内置默认过滤堆栈，降低首次分析时的噪声线程数量。
/// - 其它读取失败通常来自权限或文件系统异常，此时回退为空文本，避免默认内容覆盖用户已有但暂时不可读的配置。
pub(crate) fn read_thread_analysis_filter_preference(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(raw) => normalize_thread_analysis_filter_text(&raw),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            DEFAULT_THREAD_ANALYSIS_FILTER_TEXT.to_string()
        }
        Err(_) => String::new(),
    }
}

/// 将线程日志分析过滤配置写入指定文件。
///
/// 业务意图：
/// - 过滤内容是用户明确在设置页维护的排障偏好，应和主题、字号一样保存到应用配置目录。
pub(crate) fn write_thread_analysis_filter_preference(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, normalize_thread_analysis_filter_text(text))
}

/// 读取线程日志分析过滤配置。
pub(crate) fn load_thread_analysis_filter_preference() -> String {
    thread_analysis_filter_preference_path()
        .map(|path| read_thread_analysis_filter_preference(&path))
        .unwrap_or_else(|| DEFAULT_THREAD_ANALYSIS_FILTER_TEXT.to_string())
}

/// 保存线程日志分析过滤配置。
///
/// 错误处理：
/// - 写入失败不影响当前会话输入和后续分析，仅输出开发期诊断，避免配置目录权限问题阻断设置窗口操作。
pub(crate) fn save_thread_analysis_filter_preference(text: &str) {
    let Some(path) = thread_analysis_filter_preference_path() else {
        return;
    };
    if let Err(error) = write_thread_analysis_filter_preference(&path, text) {
        eprintln!(
            "保存线程日志分析过滤配置失败：{}：{}",
            path.display(),
            error
        );
    }
}

/// 规范化快搜关键字配置文本。
///
/// 业务意图：
/// - 快搜关键字配置是单行英文逗号分隔文本，平台粘贴或手工编辑时出现换行应被移除，避免保存后解析出跨行不可见字符。
/// - 这里只处理换行，不替换中文逗号；用户已确认“仅英文逗号”才是分隔符。
pub(crate) fn normalize_quick_search_keywords_text(text: &str) -> String {
    text.replace(['\r', '\n'], "")
}

/// 解析快搜关键字配置。
///
/// 业务意图：
/// - 快搜按多个配置关键字执行 OR 搜索；配置文本只按英文逗号分隔，中文逗号保留为关键字内容。
/// - 去掉每项首尾空白并丢弃空项，避免连续逗号或末尾逗号生成无意义搜索。
pub(crate) fn parse_quick_search_keywords(raw: &str) -> Vec<String> {
    normalize_quick_search_keywords_text(raw)
        .split(',')
        .map(str::trim)
        .filter(|keyword| !keyword.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// 从指定文件读取快搜关键字配置。
///
/// 错误处理：
/// - 配置缺失时使用内置默认关键字，便于首次使用快搜；其它读取失败回退为空文本，避免覆盖用户已有但暂时不可读的配置。
pub(crate) fn read_quick_search_keywords_preference(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(raw) => normalize_quick_search_keywords_text(&raw),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            DEFAULT_QUICK_SEARCH_KEYWORDS_TEXT.to_string()
        }
        Err(_) => String::new(),
    }
}

/// 将快搜关键字配置写入指定文件。
pub(crate) fn write_quick_search_keywords_preference(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, normalize_quick_search_keywords_text(text))
}

/// 读取快搜关键字配置。
pub(crate) fn load_quick_search_keywords_preference() -> String {
    quick_search_keywords_preference_path()
        .map(|path| read_quick_search_keywords_preference(&path))
        .unwrap_or_else(|| DEFAULT_QUICK_SEARCH_KEYWORDS_TEXT.to_string())
}

/// 保存快搜关键字配置。
///
/// 错误处理：
/// - 写入失败不影响当前会话的快搜配置草稿，仅输出开发期诊断，避免配置目录权限问题阻断设置窗口操作。
pub(crate) fn save_quick_search_keywords_preference(text: &str) {
    let Some(path) = quick_search_keywords_preference_path() else {
        return;
    };
    if let Err(error) = write_quick_search_keywords_preference(&path, text) {
        eprintln!("保存快搜关键字配置失败：{}：{}", path.display(), error);
    }
}

/// 规范化模型配置文件内容。
///
/// 业务意图：
/// - 配置文件可能被用户手工修改，默认模型 ID 可能指向不存在的档案；读取后统一清理悬空引用，避免 UI 高亮错误。
/// - 这里不主动裁剪字段内容，保存按钮的校验负责约束新写入内容，读取历史配置时尽量保持用户原文可修复。
pub(crate) fn normalize_model_configs(mut configs: ModelConfigs) -> ModelConfigs {
    if configs
        .default_profile_id
        .as_ref()
        .is_some_and(|default_id| {
            !configs
                .profiles
                .iter()
                .any(|profile| &profile.id == default_id)
        })
    {
        configs.default_profile_id = None;
    }
    configs
}

/// 从指定文件读取模型配置。
///
/// 错误处理：
/// - 文件缺失、读取失败或 JSON 损坏都返回空配置，不能阻断日志查看主流程或设置窗口打开。
/// - 损坏 JSON 不会自动覆盖原文件，避免用户仍可手工恢复其中的 API Key 和模型信息。
pub(crate) fn read_model_configs_preference(path: &Path) -> ModelConfigs {
    let Ok(raw) = fs::read_to_string(path) else {
        return ModelConfigs::default();
    };
    serde_json::from_str::<ModelConfigs>(&raw)
        .map(normalize_model_configs)
        .unwrap_or_default()
}

/// 将模型配置写入指定文件。
///
/// 业务意图：
/// - 模型配置包含列表和默认 ID，使用 pretty JSON 保存，方便用户在配置目录中直接核对。
pub(crate) fn write_model_configs_preference(
    path: &Path,
    configs: &ModelConfigs,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let serialized = serde_json::to_string_pretty(&normalize_model_configs(configs.clone()))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    fs::write(path, format!("{serialized}\n"))
}

/// 读取模型配置。
pub(crate) fn load_model_configs_preference() -> ModelConfigs {
    model_configs_preference_path()
        .map(|path| read_model_configs_preference(&path))
        .unwrap_or_default()
}

/// 保存模型配置。
///
/// 错误处理：
/// - 写入失败不回滚当前 UI 状态，仅输出诊断；这样配置目录权限问题不会让用户丢失当前表单内容。
pub(crate) fn save_model_configs_preference(configs: &ModelConfigs) {
    let Some(path) = model_configs_preference_path() else {
        return;
    };
    if let Err(error) = write_model_configs_preference(&path, configs) {
        eprintln!("保存模型配置失败：{}：{}", path.display(), error);
    }
}
