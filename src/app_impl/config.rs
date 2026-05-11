// 应用配置路径、解析和持久化功能域。
//
// 业务意图：
// - 该文件集中维护主窗口尺寸、主题、日志字号、线程过滤、快搜关键字和模型配置的磁盘读写规则。
// - 配置格式、默认值和错误回退策略保持原样，只把逻辑从 `app.rs` 迁出，降低主视图文件体积。
//
// 边界条件：
// - 写入失败只输出诊断，不阻断当前 UI 会话，避免配置目录权限问题影响日志查看主流程。
// - 所有函数仍限制在 `app` 模块内部使用，不形成 crate 级公开 API。

use super::*;

/// 可持久化的主窗口宽高。
///
/// 业务意图：
/// - 该结构只表达用户最后调整过的窗口内容宽高，启动时会重新居中，不恢复历史位置。
/// - 宽高使用 GPUI 逻辑像素，避免把平台物理像素、DPI 缩放或窗口装饰尺寸写入业务配置。
///
/// 边界条件：
/// - 宽高必须是有限正数；零、负数、NaN 和无穷大都视为损坏配置并丢弃。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MainWindowSizePreference {
    /// 主窗口宽度，单位为 GPUI 逻辑像素。
    pub(super) width: f32,
    /// 主窗口高度，单位为 GPUI 逻辑像素。
    pub(super) height: f32,
}

impl MainWindowSizePreference {
    /// 创建合法的主窗口宽高偏好。
    ///
    /// 业务意图：
    /// - 所有读入、测试和关闭保存路径都经过同一个校验入口，避免损坏配置在下次启动时造成不可见窗口。
    /// - 当前只按“有限正数”校验；如果后续定义最小窗口尺寸，应在这里统一收紧规则。
    pub(super) fn new(width: f32, height: f32) -> Option<Self> {
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
pub(super) fn parse_main_window_size_preference(raw: &str) -> Option<MainWindowSizePreference> {
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
pub(super) fn serialize_main_window_size_preference(size: MainWindowSizePreference) -> String {
    format!("{} {}\n", size.width.round(), size.height.round())
}

/// 从指定文件读取主窗口宽高偏好。
///
/// 错误处理：
/// - 配置文件缺失、无权限读取或内容损坏都不阻止应用启动。
/// - 启动窗口大小不是核心日志查看能力，因此错误统一视为无历史尺寸。
pub(super) fn read_main_window_size_preference(path: &Path) -> Option<MainWindowSizePreference> {
    let raw = fs::read_to_string(path).ok()?;
    parse_main_window_size_preference(&raw)
}

/// 将主窗口宽高偏好写入指定文件。
///
/// 错误处理：
/// - 调用者可以选择忽略错误，因为窗口关闭阶段不应因配置目录权限问题阻止退出。
/// - 这里仍返回 `io::Result`，方便测试覆盖目录创建和文件写入失败的边界。
pub(super) fn write_main_window_size_preference(
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
pub(super) fn main_window_size_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(MAIN_WINDOW_SIZE_FILE_NAME))
}

/// 获取当前平台的应用配置目录。
///
/// 跨平台约束：
/// - macOS 使用 `$HOME/Library/Application Support/LogClinic`，符合普通桌面应用配置目录习惯。
/// - Windows 使用 `%APPDATA%\LogClinic`，避免写入程序安装目录或当前工作目录。
/// - 其他平台当前不是目标运行平台，返回 `None` 并退回内存默认值。
pub(super) fn app_config_dir() -> Option<PathBuf> {
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
pub(super) fn load_main_window_size_preference() -> Option<MainWindowSizePreference> {
    let path = main_window_size_preference_path()?;
    read_main_window_size_preference(&path)
}

/// 保存主窗口宽高偏好。
///
/// 错误处理：
/// - 保存失败不会影响用户关闭应用；该偏好只是体验优化，不属于日志读取、解析或展示的核心数据。
/// - 开发调试时通过 stderr 暴露失败原因，便于定位权限或路径环境变量问题。
pub(super) fn save_main_window_size_preference(size: MainWindowSizePreference) {
    let Some(path) = main_window_size_preference_path() else {
        return;
    };
    if let Err(error) = write_main_window_size_preference(&path, size) {
        eprintln!("保存主窗口尺寸偏好失败：{}：{}", path.display(), error);
    }
}

/// 获取主题偏好文件路径。
pub(super) fn theme_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(THEME_PREFERENCE_FILE_NAME))
}

/// 解析主题偏好配置文本。
///
/// 边界条件：
/// - 配置文件可能被用户手工修改或写入中断破坏；未知值统一视为 `None`，调用方回退到“跟随系统”。
pub(super) fn parse_theme_preference(raw: &str) -> Option<ThemePreference> {
    match raw.trim() {
        "light" => Some(ThemePreference::Light),
        "dark" => Some(ThemePreference::Dark),
        "system" => Some(ThemePreference::System),
        _ => None,
    }
}

/// 序列化主题偏好。
pub(super) fn serialize_theme_preference(preference: ThemePreference) -> String {
    format!("{}\n", preference.as_config_value())
}

/// 从指定文件读取主题偏好。
///
/// 错误处理：
/// - 文件缺失、读取失败或内容损坏都不阻止应用启动，统一由调用方回退到“跟随系统”。
pub(super) fn read_theme_preference(path: &Path) -> Option<ThemePreference> {
    let raw = fs::read_to_string(path).ok()?;
    parse_theme_preference(&raw)
}

/// 将主题偏好写入指定文件。
pub(super) fn write_theme_preference(path: &Path, preference: ThemePreference) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serialize_theme_preference(preference))
}

/// 读取主题偏好。
///
/// 业务意图：
/// - 主题是设置窗口中明确提供的用户偏好，应跨启动恢复；配置不可用时默认跟随系统。
pub(super) fn load_theme_preference() -> ThemePreference {
    theme_preference_path()
        .and_then(|path| read_theme_preference(&path))
        .unwrap_or(ThemePreference::System)
}

/// 保存主题偏好。
///
/// 错误处理：
/// - 写入失败不影响当前会话的主题切换，仅在 stderr 输出诊断信息，避免配置目录权限问题阻断 UI 操作。
pub(super) fn save_theme_preference(preference: ThemePreference) {
    let Some(path) = theme_preference_path() else {
        return;
    };
    if let Err(error) = write_theme_preference(&path, preference) {
        eprintln!("保存主题偏好失败：{}：{}", path.display(), error);
    }
}

/// 获取日志显示字号偏好文件路径。
pub(super) fn log_viewer_font_size_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(LOG_VIEWER_FONT_SIZE_FILE_NAME))
}

/// 规范化日志显示字号。
///
/// 业务意图：
/// - 设置页、配置读取和测试都通过同一套边界规则，避免 UI 可选范围与磁盘配置可接受范围不一致。
///
/// 边界条件：
/// - NaN、无穷大、过小或过大的值都视为无效配置；合法值按 1px 粒度取整，保证设置按钮显示稳定整数 px。
pub(super) fn normalize_log_viewer_font_size(value: f32) -> Option<f32> {
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
pub(super) fn parse_log_viewer_font_size_preference(raw: &str) -> Option<f32> {
    normalize_log_viewer_font_size(raw.trim().parse::<f32>().ok()?)
}

/// 序列化日志显示字号配置。
pub(super) fn serialize_log_viewer_font_size_preference(font_size: f32) -> String {
    format!("{}\n", font_size.round())
}

/// 从指定文件读取日志显示字号。
///
/// 错误处理：
/// - 文件缺失、读取失败或内容损坏都不影响应用启动，统一由调用方回退默认字号。
pub(super) fn read_log_viewer_font_size_preference(path: &Path) -> Option<f32> {
    let raw = fs::read_to_string(path).ok()?;
    parse_log_viewer_font_size_preference(&raw)
}

/// 将日志显示字号写入指定文件。
pub(super) fn write_log_viewer_font_size_preference(path: &Path, font_size: f32) -> io::Result<()> {
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
pub(super) fn load_log_viewer_font_size_preference() -> f32 {
    log_viewer_font_size_preference_path()
        .and_then(|path| read_log_viewer_font_size_preference(&path))
        .unwrap_or(LOG_VIEWER_DEFAULT_FONT_SIZE)
}

/// 保存日志显示字号偏好。
///
/// 错误处理：
/// - 写入失败不影响当前会话的字号调整，仅输出开发期诊断，避免配置目录权限问题阻断 UI 操作。
pub(super) fn save_log_viewer_font_size_preference(font_size: f32) {
    let Some(path) = log_viewer_font_size_preference_path() else {
        return;
    };
    if let Err(error) = write_log_viewer_font_size_preference(&path, font_size) {
        eprintln!("保存日志显示字号偏好失败：{}：{}", path.display(), error);
    }
}

/// 获取线程日志分析过滤配置文件路径。
pub(super) fn thread_analysis_filter_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(THREAD_ANALYSIS_FILTER_FILE_NAME))
}

/// 获取快搜关键字配置文件路径。
pub(super) fn quick_search_keywords_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(QUICK_SEARCH_KEYWORDS_FILE_NAME))
}

/// 获取模型配置文件路径。
pub(super) fn model_configs_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(MODEL_CONFIGS_FILE_NAME))
}

/// 规范化线程日志分析过滤配置文本。
///
/// 业务意图：
/// - 用户可能从 Windows、macOS、终端或网页复制堆栈，换行格式不稳定；内部统一使用 LF，保证规则拆分和匹配可预测。
/// - 不裁剪首尾空白，避免破坏用户粘贴的原始堆栈文本；真正匹配时再按行去首尾空白。
pub(super) fn normalize_thread_analysis_filter_text(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// 从指定文件读取线程日志分析过滤配置。
///
/// 错误处理：
/// - 配置缺失时使用内置默认过滤堆栈，降低首次分析时的噪声线程数量。
/// - 其它读取失败通常来自权限或文件系统异常，此时回退为空文本，避免默认内容覆盖用户已有但暂时不可读的配置。
pub(super) fn read_thread_analysis_filter_preference(path: &Path) -> String {
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
pub(super) fn write_thread_analysis_filter_preference(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, normalize_thread_analysis_filter_text(text))
}

/// 读取线程日志分析过滤配置。
pub(super) fn load_thread_analysis_filter_preference() -> String {
    thread_analysis_filter_preference_path()
        .map(|path| read_thread_analysis_filter_preference(&path))
        .unwrap_or_else(|| DEFAULT_THREAD_ANALYSIS_FILTER_TEXT.to_string())
}

/// 保存线程日志分析过滤配置。
///
/// 错误处理：
/// - 写入失败不影响当前会话输入和后续分析，仅输出开发期诊断，避免配置目录权限问题阻断设置窗口操作。
pub(super) fn save_thread_analysis_filter_preference(text: &str) {
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
pub(super) fn normalize_quick_search_keywords_text(text: &str) -> String {
    text.replace(['\r', '\n'], "")
}

/// 解析快搜关键字配置。
///
/// 业务意图：
/// - 快搜按多个配置关键字执行 OR 搜索；配置文本只按英文逗号分隔，中文逗号保留为关键字内容。
/// - 去掉每项首尾空白并丢弃空项，避免连续逗号或末尾逗号生成无意义搜索。
pub(super) fn parse_quick_search_keywords(raw: &str) -> Vec<String> {
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
pub(super) fn read_quick_search_keywords_preference(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(raw) => normalize_quick_search_keywords_text(&raw),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            DEFAULT_QUICK_SEARCH_KEYWORDS_TEXT.to_string()
        }
        Err(_) => String::new(),
    }
}

/// 将快搜关键字配置写入指定文件。
pub(super) fn write_quick_search_keywords_preference(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, normalize_quick_search_keywords_text(text))
}

/// 读取快搜关键字配置。
pub(super) fn load_quick_search_keywords_preference() -> String {
    quick_search_keywords_preference_path()
        .map(|path| read_quick_search_keywords_preference(&path))
        .unwrap_or_else(|| DEFAULT_QUICK_SEARCH_KEYWORDS_TEXT.to_string())
}

/// 保存快搜关键字配置。
///
/// 错误处理：
/// - 写入失败不影响当前会话的快搜配置草稿，仅输出开发期诊断，避免配置目录权限问题阻断设置窗口操作。
pub(super) fn save_quick_search_keywords_preference(text: &str) {
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
pub(super) fn normalize_model_configs(mut configs: ModelConfigs) -> ModelConfigs {
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
pub(super) fn read_model_configs_preference(path: &Path) -> ModelConfigs {
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
pub(super) fn write_model_configs_preference(
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
pub(super) fn load_model_configs_preference() -> ModelConfigs {
    model_configs_preference_path()
        .map(|path| read_model_configs_preference(&path))
        .unwrap_or_default()
}

/// 保存模型配置。
///
/// 错误处理：
/// - 写入失败不回滚当前 UI 状态，仅输出诊断；这样配置目录权限问题不会让用户丢失当前表单内容。
pub(super) fn save_model_configs_preference(configs: &ModelConfigs) {
    let Some(path) = model_configs_preference_path() else {
        return;
    };
    if let Err(error) = write_model_configs_preference(&path, configs) {
        eprintln!("保存模型配置失败：{}：{}", path.display(), error);
    }
}
