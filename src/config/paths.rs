// 应用配置路径功能域。
//
// 业务意图：
// - 集中维护 macOS 和 Windows 的配置目录选择，以及各类配置文件的稳定文件名。
// - 上层偏好、模型配置和 AI 对话存储都通过这里取路径，避免平台判断散落在不同业务域。
//
// 边界条件：
// - 当前产品目标平台是 macOS 和 Windows；其它平台返回 `None`，调用方继续使用内存默认值。
// - 这里只拼接路径，不创建目录；具体写入函数负责按需创建父目录并处理权限错误。

use std::{env, path::PathBuf};

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

/// 日志 minimap 显示开关偏好文件名。
///
/// 业务意图：
/// - minimap 会在日志右侧额外绘制预览位图，属于用户可选择的性能和视觉偏好，需要跨启动恢复。
/// - 文件只保存 enabled/disabled 单值，保持人工可读，同时避免为一个布尔开关引入结构化配置依赖。
pub(crate) const LOG_MINIMAP_ENABLED_FILE_NAME: &str = "log-minimap-enabled.txt";

/// 线程日志分析过滤配置文件名。
///
/// 业务意图：
/// - 用户会在设置窗口中粘贴需要过滤的线程堆栈，配置必须跨重启保留，避免每次排查都重新维护无效线程列表。
/// - 文件保存原始多行文本而不是结构化格式，方便用户直接打开配置文件排查或批量替换。
pub(crate) const THREAD_ANALYSIS_FILTER_FILE_NAME: &str = "thread-analysis-filter.txt";

/// 线程日志分析线程名过滤配置文件名。
///
/// 业务意图：
/// - 线程名通配规则比完整堆栈片段更短、更常改，单独落盘可以让设置页用独立输入框维护，避免用户在大段堆栈文本中找规则。
/// - 文件仍保存纯文本多行列表，跨平台路径和权限处理继续由应用配置目录统一承担。
pub(crate) const THREAD_ANALYSIS_NAME_FILTER_FILE_NAME: &str = "thread-analysis-name-filter.txt";

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

/// 插件注册表文件名。
///
/// 业务意图：
/// - 插件 manifest 已固定为 JSON，注册表也使用 JSON 保存用户安装目录、来源类型和启用状态。
/// - 注册表只保存插件位置和开关，不复制插件 manifest 内容，重新加载时始终以插件目录中的 `plugin.json` 为准。
pub(crate) const PLUGIN_REGISTRY_FILE_NAME: &str = "plugin-registry.json";

/// 插件安装目录名。
///
/// 业务意图：
/// - 从 zip 安装的插件需要复制到应用配置目录，避免用户删除下载目录后插件失效。
/// - 开发目录插件只保存引用路径，不复制到该目录，方便第三方编辑后重新加载。
pub(crate) const PLUGINS_INSTALL_DIR_NAME: &str = "plugins";

/// 插件专属设置目录名。
///
/// 业务意图：
/// - 外部插件可以声明自己的设置页签，但插件设置不能写入全局偏好文件或注册表，避免不同职责互相污染。
/// - 每个插件使用独立 JSON 文件，损坏时只影响对应插件，并且方便用户手工定位和删除。
pub(crate) const PLUGIN_SETTINGS_DIR_NAME: &str = "plugin-settings";

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

/// 获取当前平台的主窗口宽高偏好文件路径。
///
/// 跨平台约束：
/// - 该函数只负责路径拼接，不判断历史尺寸是否适合当前显示器；窗口启动策略在 `app` 层处理。
pub(crate) fn main_window_size_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(MAIN_WINDOW_SIZE_FILE_NAME))
}

/// 获取主题偏好文件路径。
pub(crate) fn theme_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(THEME_PREFERENCE_FILE_NAME))
}

/// 获取日志显示字号偏好文件路径。
pub(crate) fn log_viewer_font_size_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(LOG_VIEWER_FONT_SIZE_FILE_NAME))
}

/// 获取日志 minimap 显示开关偏好文件路径。
///
/// 跨平台约束：
/// - 路径仍使用应用配置目录，macOS 和 Windows 的目录选择由 `app_config_dir` 统一处理。
pub(crate) fn log_minimap_enabled_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(LOG_MINIMAP_ENABLED_FILE_NAME))
}

/// 获取线程日志分析过滤配置文件路径。
pub(crate) fn thread_analysis_filter_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(THREAD_ANALYSIS_FILTER_FILE_NAME))
}

/// 获取线程日志分析线程名过滤配置文件路径。
///
/// 跨平台约束：
/// - macOS 和 Windows 使用不同配置目录，但文件名保持稳定，便于用户和维护脚本定位。
pub(crate) fn thread_analysis_name_filter_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(THREAD_ANALYSIS_NAME_FILTER_FILE_NAME))
}

/// 获取快搜关键字配置文件路径。
pub(crate) fn quick_search_keywords_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(QUICK_SEARCH_KEYWORDS_FILE_NAME))
}

/// 获取模型配置文件路径。
pub(crate) fn model_configs_preference_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(MODEL_CONFIGS_FILE_NAME))
}

/// 获取插件注册表路径。
pub(crate) fn plugin_registry_path() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(PLUGIN_REGISTRY_FILE_NAME))
}

/// 获取 zip 插件安装根目录。
pub(crate) fn plugins_install_dir() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(PLUGINS_INSTALL_DIR_NAME))
}

/// 获取插件专属设置根目录。
///
/// 跨平台约束：
/// - macOS 和 Windows 仍复用 `app_config_dir`，只在应用配置目录下追加稳定子目录。
/// - 函数只拼接路径，不创建目录；保存设置时再按需创建，避免只打开应用就产生无用目录。
pub(crate) fn plugin_settings_dir() -> Option<PathBuf> {
    app_config_dir().map(|dir| dir.join(PLUGIN_SETTINGS_DIR_NAME))
}
