//! 外部插件框架核心模型与安装运行支持。
//!
//! 业务意图：
//! - LogClinic 需要向第三方开放可编辑插件能力，但不能把内部 Rust/GPUI 类型暴露给插件。
//! - 本模块把插件定义收敛为 JSON manifest、JSON 注册表和 JSON-RPC 风格的一次性外部进程调用。
//! - UI 层只消费这里提供的插件清单、贡献点和声明式页面模型，插件进程不能直接持有主窗口状态。
//!
//! 关键约束：
//! - manifest 和注册表都使用 JSON，避免为插件系统新增 TOML 依赖。
//! - 插件 zip 解压必须做路径安全检查，拒绝绝对路径、`..` 和符号链接，避免恶意安装包写出配置目录。
//! - 默认只向插件传递日志/笔记元数据；只有显式声明 `logs.content` 权限的插件才会收到宿主已加载日志的受控读取路径。

use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use zip::ZipArchive;

use crate::config::{plugin_registry_path, plugin_settings_dir, plugins_install_dir};
use crate::log_source::LogFileSource;

/// 当前宿主支持的插件协议版本。
pub(crate) const PLUGIN_API_VERSION: u32 = 1;

/// 插件 manifest 固定文件名。
pub(crate) const PLUGIN_MANIFEST_FILE_NAME: &str = "plugin.json";

/// 插件命令最长运行时间。
///
/// 业务意图：
/// - 插件可能处理大量日志元数据，不能沿用短交互任务的十秒级超时，否则用户刚看到进度窗口任务就被终止。
/// - 仍保留上限，避免第三方插件永久挂起造成后台进程长期占用资源。
const PLUGIN_COMMAND_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// 插件 manifest 顶层结构。
///
/// 业务意图：
/// - manifest 是第三方插件和宿主之间的稳定声明边界，描述插件身份、入口命令、权限和 UI 贡献点。
/// - 字段保持扁平，方便用户直接编辑 JSON，也方便设置页展示关键诊断信息。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginManifest {
    /// 插件协议版本；第一版必须等于 1。
    pub(crate) api_version: u32,
    /// 插件唯一 ID。
    pub(crate) id: String,
    /// 展示名称。
    pub(crate) name: String,
    /// 插件版本号。
    pub(crate) version: String,
    /// 插件入口命令。
    pub(crate) entry: PluginEntry,
    /// 插件声明的权限。
    #[serde(default)]
    pub(crate) permissions: Vec<String>,
    /// 插件贡献点。
    #[serde(default)]
    pub(crate) contributes: PluginContributes,
}

impl PluginManifest {
    /// 校验 manifest 中会影响插件加载和 UI 展示的基础字段。
    ///
    /// 边界条件：
    /// - 插件 ID 既会用于配置目录名，也会用于注册表键，因此限制为 ASCII 字母、数字、点、下划线和短横线。
    /// - 入口命令允许是系统命令、绝对路径或相对插件目录路径，但不能为空；ZIP 插件额外要求入口文件随包分发。
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.api_version != PLUGIN_API_VERSION {
            return Err(format!(
                "插件协议版本 {} 暂不支持，当前仅支持 {}",
                self.api_version, PLUGIN_API_VERSION
            ));
        }
        if !is_valid_plugin_id(&self.id) {
            return Err("插件 ID 只能包含字母、数字、点、下划线和短横线".to_string());
        }
        if self.name.trim().is_empty() {
            return Err("插件名称不能为空".to_string());
        }
        if self.version.trim().is_empty() {
            return Err("插件版本不能为空".to_string());
        }
        if self.entry.command.is_empty() || self.entry.command[0].trim().is_empty() {
            return Err("插件入口命令不能为空".to_string());
        }
        let mut contribution_ids = HashSet::new();
        for contribution in self.all_menu_contributions() {
            if contribution.id.trim().is_empty() {
                return Err("插件菜单贡献点 ID 不能为空".to_string());
            }
            if contribution.title.trim().is_empty() {
                return Err(format!("插件菜单贡献点 {} 的标题不能为空", contribution.id));
            }
            if !contribution_ids.insert(contribution.id.clone()) {
                return Err(format!("插件贡献点 ID 重复：{}", contribution.id));
            }
        }
        for contribution in &self.contributes.navigation {
            if contribution.id.trim().is_empty() {
                return Err("插件导航贡献点 ID 不能为空".to_string());
            }
            if contribution.title.trim().is_empty() {
                return Err(format!("插件导航贡献点 {} 的标题不能为空", contribution.id));
            }
            if !contribution_ids.insert(contribution.id.clone()) {
                return Err(format!("插件贡献点 ID 重复：{}", contribution.id));
            }
        }
        for contribution in &self.contributes.log_toolbar {
            if contribution.id.trim().is_empty() {
                return Err("插件日志工具栏贡献点 ID 不能为空".to_string());
            }
            if contribution.title.trim().is_empty() {
                return Err(format!(
                    "插件日志工具栏贡献点 {} 的标题不能为空",
                    contribution.id
                ));
            }
            if !contribution_ids.insert(contribution.id.clone()) {
                return Err(format!("插件贡献点 ID 重复：{}", contribution.id));
            }
        }
        let mut setting_keys = HashSet::new();
        for contribution in &self.contributes.settings_tabs {
            if contribution.id.trim().is_empty() {
                return Err("插件设置页签贡献点 ID 不能为空".to_string());
            }
            if contribution.title.trim().is_empty() {
                return Err(format!(
                    "插件设置页签贡献点 {} 的标题不能为空",
                    contribution.id
                ));
            }
            if !contribution_ids.insert(contribution.id.clone()) {
                return Err(format!("插件贡献点 ID 重复：{}", contribution.id));
            }
            for setting in &contribution.pattern_settings {
                if setting.key.trim().is_empty() {
                    return Err(format!("插件设置页签 {} 的规则键不能为空", contribution.id));
                }
                if setting.label.trim().is_empty() {
                    return Err(format!(
                        "插件设置页签 {} 的规则 {} 标题不能为空",
                        contribution.id, setting.key
                    ));
                }
                if !setting_keys.insert(setting.key.clone()) {
                    return Err(format!(
                        "插件设置规则键重复：{}，所在页签：{}",
                        setting.key, contribution.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// 返回所有右键菜单贡献点，便于统一校验 ID 和标题。
    fn all_menu_contributions(&self) -> Vec<&PluginMenuContribution> {
        self.contributes
            .log_tree_context_menu
            .iter()
            .chain(self.contributes.notes_tree_context_menu.iter())
            .collect()
    }
}

/// 插件入口命令。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginEntry {
    /// 命令及参数。
    ///
    /// 业务意图：
    /// - 第一项是可执行程序，后续项是固定参数；宿主会把请求 JSON 写入 stdin。
    /// - ZIP 插件应把入口文件放在插件根目录或其子目录中，命令使用相对插件目录路径，避免依赖主程序目录。
    pub(crate) command: Vec<String>,
}

/// 插件贡献点集合。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginContributes {
    /// 主导航页贡献。
    #[serde(default)]
    pub(crate) navigation: Vec<PluginNavigationContribution>,
    /// 日志目录树右键菜单贡献。
    #[serde(default)]
    pub(crate) log_tree_context_menu: Vec<PluginMenuContribution>,
    /// 笔记树右键菜单贡献。
    #[serde(default)]
    pub(crate) notes_tree_context_menu: Vec<PluginMenuContribution>,
    /// 日志分析页顶部工具栏贡献。
    ///
    /// 业务意图：
    /// - 第三方插件可以在用户已经加载日志树后提供独立分析入口，例如泛微日志分析。
    /// - 宿主只暴露当前左侧树快照，不允许插件自行扩大扫描范围。
    #[serde(default)]
    pub(crate) log_toolbar: Vec<PluginToolbarContribution>,
    /// 设置窗口中的插件配置页签贡献。
    ///
    /// 业务意图：
    /// - 插件可声明宿主可渲染的轻量配置表单，配置保存到插件专属 JSON 文件。
    /// - 当前版本只支持通配规则文本，避免外部插件执行设置页代码。
    #[serde(default)]
    pub(crate) settings_tabs: Vec<PluginSettingsTabContribution>,
}

/// 插件主导航页贡献。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginNavigationContribution {
    /// 贡献点 ID。
    pub(crate) id: String,
    /// 展示标题。
    pub(crate) title: String,
    /// 图标名称。
    pub(crate) icon: Option<String>,
    /// 触发命令；缺省时使用贡献点 ID。
    pub(crate) command: Option<String>,
}

impl PluginNavigationContribution {
    /// 返回实际调用插件时使用的命令 ID。
    pub(crate) fn command_id(&self) -> &str {
        self.command.as_deref().unwrap_or(&self.id)
    }
}

/// 插件右键菜单贡献。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginMenuContribution {
    /// 贡献点 ID。
    pub(crate) id: String,
    /// 菜单标题。
    pub(crate) title: String,
    /// 图标名称。
    pub(crate) icon: Option<String>,
    /// 触发命令；缺省时使用贡献点 ID。
    pub(crate) command: Option<String>,
    /// 简单启用条件；第一版只保留给 UI 展示和后续扩展，具体过滤由宿主上下文决定。
    pub(crate) when: Option<String>,
}

impl PluginMenuContribution {
    /// 返回实际调用插件时使用的命令 ID。
    pub(crate) fn command_id(&self) -> &str {
        self.command.as_deref().unwrap_or(&self.id)
    }
}

/// 插件日志工具栏按钮贡献。
///
/// 业务意图：
/// - 工具栏按钮必须由 manifest 静态声明，宿主才能在不启动插件进程的情况下渲染入口。
/// - 点击后宿主会收集当前左侧日志树快照，并通过 `LogToolbarAction` 上下文交给插件处理。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginToolbarContribution {
    /// 贡献点 ID。
    pub(crate) id: String,
    /// 按钮标题。
    pub(crate) title: String,
    /// 图标名称。
    pub(crate) icon: Option<String>,
    /// 触发命令；缺省时使用贡献点 ID。
    pub(crate) command: Option<String>,
    /// 简单启用条件；第一版只保留给后续扩展，当前不在宿主侧解析表达式。
    pub(crate) when: Option<String>,
}

impl PluginToolbarContribution {
    /// 返回实际调用插件时使用的命令 ID。
    pub(crate) fn command_id(&self) -> &str {
        self.command.as_deref().unwrap_or(&self.id)
    }
}

/// 插件设置页签贡献。
///
/// 业务意图：
/// - 设置页签由宿主声明式渲染，插件进程不会在设置窗口打开时启动，避免配置页成为任意代码执行入口。
/// - 第一版只支持按键保存字符串规则，满足日志类型匹配规则这种轻量配置。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginSettingsTabContribution {
    /// 设置页签 ID。
    pub(crate) id: String,
    /// 设置页签标题。
    pub(crate) title: String,
    /// 图标名称。
    pub(crate) icon: Option<String>,
    /// 可编辑的匹配规则列表。
    #[serde(default)]
    pub(crate) pattern_settings: Vec<PluginPatternSettingContribution>,
}

/// 插件声明式匹配规则配置项。
///
/// 边界条件：
/// - `default` 保持原始字符串，宿主不解析语义；插件命令执行时再按自身规则解释。
/// - 多个 glob 用分号分隔是 `weaver-logext` 的业务约定，宿主只负责保存和恢复。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginPatternSettingContribution {
    /// 配置键。
    pub(crate) key: String,
    /// 设置页展示名称。
    pub(crate) label: String,
    /// 默认规则文本。
    #[serde(default)]
    pub(crate) default: String,
    /// 可选说明。
    pub(crate) description: Option<String>,
}

/// 插件安装来源。
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum PluginInstallSource {
    /// 本地开发目录。
    DevelopmentDirectory,
    /// 已从 zip 安装到配置目录。
    InstalledZip,
}

impl PluginInstallSource {
    /// 设置页展示文案。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::DevelopmentDirectory => "开发目录",
            Self::InstalledZip => "ZIP 安装",
        }
    }
}

/// 插件注册表条目。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginRegistryEntry {
    /// 插件 ID。
    pub(crate) id: String,
    /// 插件来源。
    pub(crate) source: PluginInstallSource,
    /// 插件目录。损坏注册表可能缺失路径，加载时会转成错误占位。
    pub(crate) path: Option<PathBuf>,
    /// 是否启用。
    pub(crate) enabled: bool,
}

/// 插件注册表。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginRegistry {
    /// 用户安装或显式配置过的插件。
    #[serde(default)]
    pub(crate) entries: Vec<PluginRegistryEntry>,
}

impl PluginRegistry {
    /// 返回指定插件 ID 的注册表条目。
    pub(crate) fn entry_for(&self, id: &str) -> Option<&PluginRegistryEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// 插入或更新注册表条目。
    pub(crate) fn upsert(&mut self, entry: PluginRegistryEntry) {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|existing| existing.id == entry.id)
        {
            *existing = entry;
        } else {
            self.entries.push(entry);
        }
        self.entries.sort_by(|left, right| left.id.cmp(&right.id));
    }

    /// 移除指定插件。
    pub(crate) fn remove(&mut self, plugin_id: &str) -> Option<PluginRegistryEntry> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == plugin_id)?;
        Some(self.entries.remove(index))
    }
}

/// 当前可用插件定义。
#[derive(Clone, Debug)]
pub(crate) struct PluginDefinition {
    /// 插件 manifest；加载失败时为 `None`。
    pub(crate) manifest: Option<PluginManifest>,
    /// 插件 ID；加载失败时来自注册表条目。
    pub(crate) id: String,
    /// 插件展示名称；加载失败时回退 ID。
    pub(crate) name: String,
    /// 插件版本。
    pub(crate) version: String,
    /// 安装来源。
    pub(crate) source: PluginInstallSource,
    /// 插件根目录；加载失败或损坏注册表可能为空。
    pub(crate) root_path: Option<PathBuf>,
    /// 是否启用。
    pub(crate) enabled: bool,
    /// 加载错误。
    pub(crate) load_error: Option<String>,
}

impl PluginDefinition {
    /// 插件是否可参与贡献点渲染。
    pub(crate) fn active(&self) -> bool {
        self.enabled && self.load_error.is_none() && self.manifest.is_some()
    }

    /// 设置页展示路径。
    pub(crate) fn display_path(&self) -> String {
        self.root_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "未配置目录".to_string())
    }
}

/// 发给插件进程的一次命令请求。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginCommandRequest {
    /// 协议版本。
    pub(crate) api_version: u32,
    /// 插件 ID。
    pub(crate) plugin_id: String,
    /// 命令 ID。
    pub(crate) command_id: String,
    /// 命令上下文。
    pub(crate) context: PluginCommandContext,
}

/// 插件命令上下文。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PluginCommandContext {
    /// 主导航页上下文。
    NavigationPage,
    /// 日志树菜单上下文。
    LogTreeMenu {
        /// 菜单贡献点 ID。
        menu_id: String,
        /// 候选日志元数据。
        files: Vec<PluginLogFile>,
    },
    /// 日志工具栏按钮上下文。
    ///
    /// 业务意图：
    /// - 工具栏入口面向“当前已加载日志树”的整体分析，不依赖右键选中节点。
    /// - 宿主传入的 `files` 是左侧树授权范围快照，插件只能基于这份快照做元数据或正文读取。
    LogToolbarAction {
        /// 工具栏贡献点 ID。
        toolbar_id: String,
        /// 当前左侧日志树快照中的候选日志元数据。
        files: Vec<PluginLogFile>,
        /// 合并 manifest 默认值和用户保存值后的插件设置。
        #[serde(default)]
        settings: BTreeMap<String, String>,
    },
    /// 插件表格行内动作针对单个日志文件发起的二次命令。
    ///
    /// 业务意图：
    /// - 大结果集不能把所有下钻页面一次性塞进首次响应；例如 SQL 明细应在用户点击对应请求后再由插件读取并返回。
    /// - 宿主仍只传递快照化的日志元数据和字符串参数，不暴露 GPUI 内部状态，也不允许插件直接操作主窗口。
    LogFileAction {
        /// 行动作 ID，由插件自行定义，便于一个命令复用不同按钮语义。
        action_id: String,
        /// 当前行对应的日志文件快照。
        file: PluginLogFile,
        /// 附加业务参数，例如请求地址、文件名解析字段等。
        #[serde(default)]
        data: BTreeMap<String, String>,
    },
    /// 笔记树菜单上下文。
    NotesTreeMenu {
        /// 菜单贡献点 ID。
        menu_id: String,
        /// 当前右键节点。
        target: PluginNoteTreeTarget,
    },
}

/// 传给插件的日志文件元数据。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginLogFile {
    /// 来源稳定键。
    pub(crate) source_key: String,
    /// 展示文件名。
    pub(crate) display_name: String,
    /// 展示路径或来源描述。
    pub(crate) path_label: String,
    /// 来源类型。
    pub(crate) source_kind: String,
    /// 左侧树节点类型。
    pub(crate) node_kind: String,
    /// 插件可读取的本地文件路径。
    ///
    /// 业务意图：
    /// - 只有声明 `logs.content` 权限的插件才会收到该字段；未声明权限时宿主会在序列化前清空。
    /// - 该路径只来自用户已经通过左侧日志树授权/选择的文件或已物化临时文件，不接受插件自行扩展扫描范围。
    ///
    /// 边界条件：
    /// - 压缩包内未物化成员没有稳定本地路径，因此为 `None`，插件应只做元数据解析或跳过正文解析。
    /// - 路径可能包含非 UTF-8 字节；这里使用有损展示字符串是跨进程 JSON 协议的折中，真实读取仍在宿主内部使用 `PathBuf`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) read_path: Option<String>,
    /// 宿主内部保留的原始日志来源。
    ///
    /// 业务意图：
    /// - 插件只能看到 JSON 中的轻量字段；宿主在行内延迟命令触发时需要用该来源把压缩包成员物化为临时文件。
    /// - 该字段通过 `serde(skip)` 永远不进入插件协议，避免把内部路径和压缩包定位模型直接暴露给第三方。
    #[serde(skip)]
    pub(crate) host_source: Option<LogFileSource>,
}

/// 传给插件的笔记树目标元数据。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginNoteTreeTarget {
    /// 笔记或目录 ID。
    pub(crate) id: String,
    /// 标题。
    pub(crate) title: String,
    /// 节点类型。
    pub(crate) kind: String,
}

/// 插件命令响应。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(crate) enum PluginCommandResponse {
    /// 打开声明式页面窗口。
    OpenWindow {
        /// 窗口标题。
        title: String,
        /// 页面模型。
        page: PluginPage,
    },
    /// 展示提示。
    ShowMessage {
        /// 提示等级。
        level: PluginMessageLevel,
        /// 提示内容。
        message: String,
    },
    /// 插件返回业务错误。
    Error {
        /// 错误内容。
        message: String,
    },
}

/// 插件进程 stdout 中的流式事件。
///
/// 业务意图：
/// - 大数据插件可能需要较长时间才能生成最终页面；进度事件允许宿主先打开窗口并持续刷新处理状态。
/// - 事件与最终响应都通过 stdout JSON Lines 传输，旧插件仍可只输出一个最终响应 JSON 行。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum PluginCommandEvent {
    /// 处理进度。
    Progress {
        /// 进度快照。
        progress: PluginCommandProgress,
    },
}

/// 插件处理进度快照。
///
/// 边界条件：
/// - `total` 为空或为 0 时表示无法计算百分比，UI 只展示当前阶段文案。
/// - `done` 必须由插件保持单调递增；宿主只负责展示，不信任它作为权限或任务完成依据。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginCommandProgress {
    /// 当前阶段主文案。
    pub(crate) message: String,
    /// 当前阶段补充文案，例如正在处理的文件名。
    pub(crate) detail: Option<String>,
    /// 已完成工作量。
    pub(crate) done: u64,
    /// 总工作量。
    pub(crate) total: Option<u64>,
    /// 工作量单位。
    pub(crate) unit: Option<String>,
}

/// 插件提示等级。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PluginMessageLevel {
    /// 普通信息。
    Info,
    /// 警告。
    Warning,
    /// 错误。
    Error,
}

/// 声明式插件页面。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginPage {
    /// 页面标题。
    pub(crate) title: String,
    /// 页面说明。
    pub(crate) description: Option<String>,
    /// 统计摘要。
    #[serde(default)]
    pub(crate) stats: Vec<PluginPageStat>,
    /// 可选表格。
    pub(crate) table: Option<PluginPageTable>,
    /// 可选处理进度。
    ///
    /// 业务意图：
    /// - 宿主触发插件后会先打开窗口并填入进度；插件最终返回页面后通常不再携带该字段。
    /// - 字段带默认值以兼容第三方旧插件响应，避免要求所有插件立即补齐 progress。
    #[serde(default)]
    pub(crate) progress: Option<PluginCommandProgress>,
}

/// 插件页面统计项。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginPageStat {
    /// 统计名称。
    pub(crate) label: String,
    /// 统计值。
    pub(crate) value: String,
}

/// 插件页面表格。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginPageTable {
    /// 表头。
    pub(crate) headers: Vec<String>,
    /// 行数据。
    pub(crate) rows: Vec<Vec<String>>,
    /// 每一行对应的可点击动作。
    ///
    /// 业务意图：
    /// - 插件 v1 的表格主体仍保持字符串矩阵，方便第三方插件生成和旧版本兼容。
    /// - 需要从汇总行打开明细窗口时，通过与 `rows` 同下标的动作列表声明按钮；未提供该字段的旧插件按空动作处理。
    ///
    /// 边界条件：
    /// - 行动作可以直接携带静态页面，也可以声明一个延迟插件命令；旧插件不提供该字段时仍按普通表格渲染。
    /// - 延迟命令仍走外部进程 JSON 协议和权限剥离，不允许插件借 UI 按钮绕过宿主隔离。
    #[serde(default)]
    pub(crate) row_actions: Vec<Vec<PluginTableRowAction>>,
}

/// 插件表格行内动作。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginTableRowAction {
    /// 按钮文案。
    pub(crate) label: String,
    /// 新窗口标题；为空时使用按钮文案。
    pub(crate) title: Option<String>,
    /// 点击后直接打开的声明式页面。
    ///
    /// 业务意图：
    /// - 使用 `Box` 打破 `PluginPage -> PluginPageTable -> PluginTableRowAction -> PluginPage` 的递归类型。
    /// - 该字段保持可选，兼容需要点击后再处理大量数据的延迟命令按钮。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) page: Option<Box<PluginPage>>,
    /// 点击后发起的延迟插件命令。
    ///
    /// 业务意图：
    /// - 对 SQL 明细这类体量不确定的数据，首次结果只渲染按钮，用户点击后才启动插件读取对应日志正文。
    /// - 命令仍由同一个插件执行，宿主会先打开进度窗口，最终替换为插件返回的声明式页面。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command: Option<PluginTableRowCommand>,
}

/// 插件表格行内延迟命令。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PluginTableRowCommand {
    /// 要调用的插件命令 ID。
    pub(crate) command_id: String,
    /// 命令上下文快照。
    pub(crate) context: PluginCommandContext,
}

/// 从磁盘读取插件注册表。
pub(crate) fn load_plugin_registry() -> PluginRegistry {
    let Some(path) = plugin_registry_path() else {
        return PluginRegistry::default();
    };
    let Ok(raw) = fs::read_to_string(path) else {
        return PluginRegistry::default();
    };
    parse_plugin_registry_tolerant(&raw)
}

/// 宽容解析插件注册表。
///
/// 业务意图：
/// - 早期版本曾写入旧来源标记；插件完全独立后，未知来源不应导致整个注册表丢失。
/// - 这里先尝试严格解析，失败后按条目逐个解析并丢弃未知来源或损坏条目，保留用户已安装的独立插件。
fn parse_plugin_registry_tolerant(raw: &str) -> PluginRegistry {
    if let Ok(registry) = serde_json::from_str::<PluginRegistry>(raw) {
        return registry;
    }

    #[derive(Deserialize)]
    struct PartialRegistry {
        /// 原始条目使用 Value 承接，避免单个旧字段破坏其它有效插件记录。
        #[serde(default)]
        entries: Vec<serde_json::Value>,
    }

    let Ok(partial) = serde_json::from_str::<PartialRegistry>(raw) else {
        return PluginRegistry::default();
    };
    PluginRegistry {
        entries: partial
            .entries
            .into_iter()
            .filter_map(|entry| serde_json::from_value::<PluginRegistryEntry>(entry).ok())
            .collect(),
    }
}

/// 写入插件注册表。
pub(crate) fn save_plugin_registry(registry: &PluginRegistry) -> Result<(), String> {
    let Some(path) = plugin_registry_path() else {
        return Err("当前平台没有可用的应用配置目录，无法保存插件注册表".to_string());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建插件注册表目录失败：{error}"))?;
    }
    let raw = serde_json::to_string_pretty(registry)
        .map_err(|error| format!("序列化插件注册表失败：{error}"))?;
    fs::write(&path, raw).map_err(|error| format!("写入插件注册表失败：{error}"))
}

/// 插件设置文件结构。
///
/// 业务意图：
/// - 插件设置与注册表分离，注册表只保存安装和启用状态，设置文件只保存用户可编辑业务规则。
/// - 外层 `settings` 字段方便后续追加更新时间、schema 版本等元信息，同时保持当前键值读取简单。
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
struct PluginSettingsFile {
    /// 插件声明式设置键值。
    #[serde(default)]
    settings: BTreeMap<String, String>,
}

/// 返回 manifest 中所有设置项的默认值。
///
/// 边界条件：
/// - 多个设置页签不应声明同名 key；manifest 校验会拦截重复，这里仍按最后一次写入兜底，避免损坏插件导致崩溃。
pub(crate) fn plugin_settings_defaults(manifest: &PluginManifest) -> BTreeMap<String, String> {
    manifest
        .contributes
        .settings_tabs
        .iter()
        .flat_map(|tab| tab.pattern_settings.iter())
        .map(|setting| (setting.key.clone(), setting.default.clone()))
        .collect()
}

/// 读取插件专属设置，并与 manifest 默认值合并。
///
/// 业务意图：
/// - 设置文件缺失或损坏时回退默认规则，保证插件入口仍可使用。
/// - manifest 新增规则后，旧设置文件不会覆盖新增项；manifest 删除规则后，旧文件中的孤立键不会再传给插件。
pub(crate) fn load_plugin_settings(manifest: &PluginManifest) -> BTreeMap<String, String> {
    let defaults = plugin_settings_defaults(manifest);
    let Some(path) = plugin_settings_path_for(&manifest.id) else {
        return defaults;
    };
    load_plugin_settings_from_path(&path, defaults)
}

/// 保存插件设置。
///
/// 边界条件：
/// - 只保存 manifest 当前声明的 key，避免损坏 UI 或旧配置把未知键继续传给插件。
/// - 写入失败不会修改内存默认值，调用方需要把中文错误展示给用户。
pub(crate) fn save_plugin_settings(
    manifest: &PluginManifest,
    values: &BTreeMap<String, String>,
) -> Result<(), String> {
    let defaults = plugin_settings_defaults(manifest);
    let filtered = defaults
        .keys()
        .map(|key| {
            (
                key.clone(),
                values
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| defaults[key].clone()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let Some(path) = plugin_settings_path_for(&manifest.id) else {
        return Err("当前平台没有可用的应用配置目录，无法保存插件设置".to_string());
    };
    save_plugin_settings_to_path(&path, &filtered)
}

/// 将指定插件设置恢复为 manifest 默认值并落盘。
pub(crate) fn restore_plugin_settings_defaults(manifest: &PluginManifest) -> Result<(), String> {
    let defaults = plugin_settings_defaults(manifest);
    save_plugin_settings(manifest, &defaults)
}

/// 返回插件设置文件路径。
///
/// 业务意图：
/// - 插件 ID 会成为文件名，必须再次校验，避免损坏注册表或第三方 manifest 把设置写出配置目录。
fn plugin_settings_path_for(plugin_id: &str) -> Option<PathBuf> {
    if !is_valid_plugin_id(plugin_id) {
        return None;
    }
    plugin_settings_dir().map(|dir| dir.join(format!("{plugin_id}.json")))
}

/// 从指定路径读取设置并按默认值过滤。
///
/// 边界条件：
/// - 损坏 JSON、缺失字段或旧版本直接写入平铺 map 都要能回退或兼容，避免一次手工编辑错误让设置页不可用。
fn load_plugin_settings_from_path(
    path: &Path,
    defaults: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let Ok(raw) = fs::read_to_string(path) else {
        return defaults;
    };
    let saved = serde_json::from_str::<PluginSettingsFile>(&raw)
        .map(|file| file.settings)
        .or_else(|_| serde_json::from_str::<BTreeMap<String, String>>(&raw))
        .unwrap_or_default();
    merge_plugin_settings_with_defaults(defaults, saved)
}

/// 合并默认设置和用户保存设置。
///
/// 业务意图：
/// - 只信任 manifest 当前声明的 key；旧设置文件里的未知键可能来自已经卸载的页签或手工误写，不能继续传给插件。
fn merge_plugin_settings_with_defaults(
    defaults: BTreeMap<String, String>,
    saved: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    defaults
        .into_iter()
        .map(|(key, default_value)| {
            let value = saved.get(&key).cloned().unwrap_or(default_value);
            (key, value)
        })
        .collect()
}

/// 将插件设置写入指定路径。
fn save_plugin_settings_to_path(
    path: &Path,
    values: &BTreeMap<String, String>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("创建插件设置目录失败：{error}"))?;
    }
    let raw = serde_json::to_string_pretty(&PluginSettingsFile {
        settings: values.clone(),
    })
    .map_err(|error| format!("序列化插件设置失败：{error}"))?;
    fs::write(path, raw).map_err(|error| format!("写入插件设置失败：{error}"))
}

/// 加载当前所有插件定义。
pub(crate) fn load_plugin_definitions(registry: &PluginRegistry) -> Vec<PluginDefinition> {
    let mut definitions = Vec::new();
    let mut seen_ids = HashSet::new();

    for entry in &registry.entries {
        if !seen_ids.insert(entry.id.clone()) {
            definitions.push(PluginDefinition {
                manifest: None,
                id: entry.id.clone(),
                name: entry.id.clone(),
                version: "-".to_string(),
                source: entry.source,
                root_path: entry.path.clone(),
                enabled: entry.enabled,
                load_error: Some("插件 ID 与其它插件重复".to_string()),
            });
            continue;
        }
        definitions.push(load_registered_plugin(entry));
    }
    definitions
}

/// 安装本地开发目录插件。
pub(crate) fn register_development_plugin_directory(
    registry: &mut PluginRegistry,
    directory: &Path,
) -> Result<PluginRegistryEntry, String> {
    let manifest = read_manifest_from_directory(directory)?;
    manifest.validate()?;
    let entry = PluginRegistryEntry {
        id: manifest.id,
        source: PluginInstallSource::DevelopmentDirectory,
        path: Some(directory.to_path_buf()),
        enabled: true,
    };
    registry.upsert(entry.clone());
    Ok(entry)
}

/// 从 zip 安装插件到配置目录。
pub(crate) fn install_plugin_zip(
    registry: &mut PluginRegistry,
    zip_path: &Path,
) -> Result<PluginRegistryEntry, String> {
    let install_root =
        plugins_install_dir().ok_or_else(|| "当前平台没有可用的插件安装目录".to_string())?;
    fs::create_dir_all(&install_root).map_err(|error| {
        format!(
            "创建插件安装目录 {} 失败：{}",
            install_root.display(),
            error
        )
    })?;

    let zip_file = File::open(zip_path)
        .map_err(|error| format!("无法打开插件 ZIP {}：{}", zip_path.display(), error))?;
    let mut archive =
        ZipArchive::new(zip_file).map_err(|error| format!("读取插件 ZIP 失败：{error}"))?;
    let manifest_index = find_zip_manifest_index(&mut archive)?;
    let manifest_path = safe_zip_entry_path(
        archive
            .by_index(manifest_index)
            .map_err(|error| format!("读取插件 manifest 条目失败：{error}"))?
            .name(),
    )?;
    let strip_prefix = zip_manifest_strip_prefix(&manifest_path);

    let mut manifest_raw = String::new();
    archive
        .by_index(manifest_index)
        .map_err(|error| format!("读取插件 manifest 条目失败：{error}"))?
        .read_to_string(&mut manifest_raw)
        .map_err(|error| format!("读取插件 manifest 内容失败：{error}"))?;
    let manifest = parse_plugin_manifest(&manifest_raw)?;
    manifest.validate()?;

    let staging_dir = install_root.join(format!(
        ".install-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir)
            .map_err(|error| format!("清理旧插件临时目录失败：{error}"))?;
    }
    fs::create_dir_all(&staging_dir).map_err(|error| format!("创建插件临时目录失败：{error}"))?;

    extract_plugin_zip(&mut archive, strip_prefix.as_deref(), &staging_dir).map_err(|error| {
        let _ = fs::remove_dir_all(&staging_dir);
        error
    })?;
    validate_installed_zip_entry(&manifest, &staging_dir).map_err(|error| {
        let _ = fs::remove_dir_all(&staging_dir);
        error
    })?;

    let destination = install_root.join(&manifest.id);
    if destination.exists() {
        fs::remove_dir_all(&destination).map_err(|error| {
            format!("替换已有插件目录 {} 失败：{}", destination.display(), error)
        })?;
    }
    fs::rename(&staging_dir, &destination).map_err(|error| {
        let _ = fs::remove_dir_all(&staging_dir);
        format!("安装插件到 {} 失败：{}", destination.display(), error)
    })?;

    let enabled = registry
        .entry_for(&manifest.id)
        .map(|entry| entry.enabled)
        .unwrap_or(true);
    let entry = PluginRegistryEntry {
        id: manifest.id,
        source: PluginInstallSource::InstalledZip,
        path: Some(destination),
        enabled,
    };
    registry.upsert(entry.clone());
    Ok(entry)
}

/// 卸载插件注册项，并按来源决定是否删除安装目录。
pub(crate) fn uninstall_plugin(
    registry: &mut PluginRegistry,
    plugin_id: &str,
) -> Result<(), String> {
    if let Some(entry) = registry.remove(plugin_id)
        && entry.source == PluginInstallSource::InstalledZip
        && let Some(path) = entry.path
        && path.exists()
    {
        fs::remove_dir_all(&path)
            .map_err(|error| format!("删除插件目录 {} 失败：{}", path.display(), error))?;
    }
    Ok(())
}

/// 调用插件外部进程并接收流式进度。
///
/// 业务意图：
/// - 菜单点击后宿主会立即打开插件窗口，后台进程通过 stdout JSON Lines 上报进度，最终再输出页面响应。
/// - `progress_sender` 为空时仍支持设置页或测试路径的同步调用，保持旧插件调用语义。
///
/// 边界条件：
/// - stdout 只允许插件协议 JSON 行，普通日志应写到 stderr；否则宿主会把非法输出视为协议错误。
/// - 最终响应必须且只能出现一次；没有最终响应时展示明确错误，避免进度窗口永久停留。
pub(crate) fn invoke_plugin_command_with_progress(
    plugin: &PluginDefinition,
    command_id: &str,
    context: PluginCommandContext,
    progress_sender: Option<mpsc::Sender<PluginCommandProgress>>,
) -> Result<PluginCommandResponse, String> {
    let manifest = plugin
        .manifest
        .as_ref()
        .ok_or_else(|| "插件未成功加载，无法执行命令".to_string())?;
    let mut command_parts = manifest.entry.command.clone();
    let executable = command_parts
        .first_mut()
        .ok_or_else(|| "插件入口命令为空".to_string())?;
    *executable = resolve_plugin_command_path(executable, plugin.root_path.as_deref())?;

    let mut command = Command::new(&command_parts[0]);
    if command_parts.len() > 1 {
        command.args(&command_parts[1..]);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(root_path) = plugin.root_path.as_deref() {
        command.current_dir(root_path);
    }

    let request = PluginCommandRequest {
        api_version: PLUGIN_API_VERSION,
        plugin_id: manifest.id.clone(),
        command_id: command_id.to_string(),
        context: sanitize_plugin_context_for_permissions(plugin, context),
    };
    let request_raw =
        serde_json::to_vec(&request).map_err(|error| format!("序列化插件请求失败：{error}"))?;
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动插件进程失败：{error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(error) = stdin
            .write_all(&request_raw)
            .and_then(|_| stdin.write_all(b"\n"))
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("写入插件请求失败：{error}"));
        }
    }
    wait_for_plugin_response(child, PLUGIN_COMMAND_TIMEOUT, progress_sender)
}

/// 调用插件外部进程，并在请求 JSON 后继续向 stdin 写入受控日志内容流。
///
/// 业务意图：
/// - 压缩包成员没有稳定本地路径，不能为了插件读取正文而先落盘物化；宿主可以把用户选中的单个日志按行流式写给插件。
/// - 该入口只用于点击后的延迟命令，避免首次右键解析时读取或解压大量日志正文。
///
/// 边界条件：
/// - 插件必须声明 `logs.content` 权限，否则拒绝写入正文流。
/// - stdin 第一行仍是标准插件请求 JSON，后续行是内容事件 JSON；旧插件不会走该入口，因此不会破坏旧协议。
pub(crate) fn invoke_plugin_command_with_content_stream<F>(
    plugin: &PluginDefinition,
    command_id: &str,
    context: PluginCommandContext,
    progress_sender: Option<mpsc::Sender<PluginCommandProgress>>,
    write_content: F,
) -> Result<PluginCommandResponse, String>
where
    F: FnOnce(&mut dyn Write, Option<&mpsc::Sender<PluginCommandProgress>>) -> Result<(), String>
        + Send
        + 'static,
{
    if !plugin_allows_log_content(plugin) {
        return Err("插件未声明 logs.content 权限，无法读取日志正文".to_string());
    }
    let manifest = plugin
        .manifest
        .as_ref()
        .ok_or_else(|| "插件未成功加载，无法执行命令".to_string())?;
    let mut command_parts = manifest.entry.command.clone();
    let executable = command_parts
        .first_mut()
        .ok_or_else(|| "插件入口命令为空".to_string())?;
    *executable = resolve_plugin_command_path(executable, plugin.root_path.as_deref())?;

    let mut command = Command::new(&command_parts[0]);
    if command_parts.len() > 1 {
        command.args(&command_parts[1..]);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(root_path) = plugin.root_path.as_deref() {
        command.current_dir(root_path);
    }

    let request = PluginCommandRequest {
        api_version: PLUGIN_API_VERSION,
        plugin_id: manifest.id.clone(),
        command_id: command_id.to_string(),
        context: sanitize_plugin_context_for_permissions(plugin, context),
    };
    let request_raw =
        serde_json::to_vec(&request).map_err(|error| format!("序列化插件请求失败：{error}"))?;
    let mut child = command
        .spawn()
        .map_err(|error| format!("启动插件进程失败：{error}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "插件 stdin 管道不可用".to_string())?;
    let writer_progress_sender = progress_sender.clone();
    let (stdin_sender, stdin_receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut stdin = stdin;
        let result = stdin
            .write_all(&request_raw)
            .and_then(|_| stdin.write_all(b"\n"))
            .map_err(|error| format!("写入插件请求失败：{error}"))
            .and_then(|_| write_content(&mut stdin, writer_progress_sender.as_ref()))
            .and_then(|_| {
                stdin
                    .flush()
                    .map_err(|error| format!("刷新插件 stdin 失败：{error}"))
            });
        let _ = stdin_sender.send(result);
    });
    wait_for_plugin_response_with_stdin_result(
        child,
        PLUGIN_COMMAND_TIMEOUT,
        progress_sender,
        stdin_receiver,
    )
}

/// 写入一行日志正文内容事件。
///
/// 业务意图：
/// - 内容流使用 JSON Lines，避免原始日志行中的控制字符、引号或换行破坏插件 stdin 协议。
/// - 每个事件只携带一行文本，插件可以边读边解析，不需要等待宿主把完整日志组装到一个 JSON 字段中。
pub(crate) fn write_plugin_log_content_line(
    writer: &mut dyn Write,
    line: &str,
) -> Result<(), String> {
    serde_json::to_writer(
        &mut *writer,
        &serde_json::json!({
            "event": "log_content_line",
            "line": line,
        }),
    )
    .map_err(|error| format!("序列化日志内容流失败：{error}"))?;
    writer
        .write_all(b"\n")
        .map_err(|error| format!("写入日志内容流失败：{error}"))
}

/// 按插件声明权限清理即将发送的命令上下文。
///
/// 业务意图：
/// - `PluginLogFile.read_path` 会让第三方插件读取用户选择的日志正文，必须由 `logs.content` 权限显式开启。
/// - 清理逻辑放在最终序列化前，避免未来新增调用入口时忘记做权限判断。
///
/// 边界条件：
/// - 只剥离读取路径，不删除文件名、来源类型和展示路径等元数据；旧的元数据插件继续可用。
/// - 插件返回的延迟行命令也会再次经过这里，防止旧插件或恶意响应自行拼出未授权路径。
fn sanitize_plugin_context_for_permissions(
    plugin: &PluginDefinition,
    context: PluginCommandContext,
) -> PluginCommandContext {
    if plugin_allows_log_content(plugin) {
        return context;
    }

    match context {
        PluginCommandContext::LogTreeMenu { menu_id, files } => PluginCommandContext::LogTreeMenu {
            menu_id,
            files: files
                .into_iter()
                .map(plugin_log_file_without_read_path)
                .collect(),
        },
        PluginCommandContext::LogToolbarAction {
            toolbar_id,
            files,
            settings,
        } => PluginCommandContext::LogToolbarAction {
            toolbar_id,
            files: files
                .into_iter()
                .map(plugin_log_file_without_read_path)
                .collect(),
            settings,
        },
        PluginCommandContext::LogFileAction {
            action_id,
            file,
            data,
        } => PluginCommandContext::LogFileAction {
            action_id,
            file: plugin_log_file_without_read_path(file),
            data,
        },
        other => other,
    }
}

/// 判断插件是否声明了读取日志正文权限。
///
/// 业务意图：
/// - `logs.metadata` 只允许文件名和来源摘要；`logs.content` 才允许读取用户选中日志的正文。
/// - 权限名称使用字符串是为了保持 manifest JSON 简单，也便于后续新增权限而不破坏旧插件。
fn plugin_allows_log_content(plugin: &PluginDefinition) -> bool {
    plugin.manifest.as_ref().is_some_and(|manifest| {
        manifest
            .permissions
            .iter()
            .any(|permission| permission == "logs.content")
    })
}

/// 返回不含正文读取路径的日志元数据。
fn plugin_log_file_without_read_path(mut file: PluginLogFile) -> PluginLogFile {
    file.read_path = None;
    file
}

/// 解析插件 manifest JSON。
pub(crate) fn parse_plugin_manifest(raw: &str) -> Result<PluginManifest, String> {
    serde_json::from_str(raw).map_err(|error| format!("解析 plugin.json 失败：{error}"))
}

/// 从目录读取插件 manifest。
pub(crate) fn read_manifest_from_directory(directory: &Path) -> Result<PluginManifest, String> {
    let manifest_path = directory.join(PLUGIN_MANIFEST_FILE_NAME);
    let raw = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("读取 {} 失败：{}", manifest_path.display(), error))?;
    parse_plugin_manifest(&raw)
}

/// 返回插件 ID 是否满足配置目录安全要求。
fn is_valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

/// 从注册表条目加载插件。
fn load_registered_plugin(entry: &PluginRegistryEntry) -> PluginDefinition {
    let Some(path) = entry.path.as_deref() else {
        return PluginDefinition {
            manifest: None,
            id: entry.id.clone(),
            name: entry.id.clone(),
            version: "-".to_string(),
            source: entry.source,
            root_path: None,
            enabled: entry.enabled,
            load_error: Some("插件注册表缺少路径".to_string()),
        };
    };

    match read_manifest_from_directory(path).and_then(|manifest| {
        manifest.validate()?;
        if manifest.id != entry.id {
            return Err(format!(
                "插件目录中的 ID {} 与注册表 ID {} 不一致",
                manifest.id, entry.id
            ));
        }
        if entry.source == PluginInstallSource::InstalledZip {
            validate_installed_zip_entry(&manifest, path)?;
        }
        Ok(manifest)
    }) {
        Ok(manifest) => PluginDefinition {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            source: entry.source,
            root_path: Some(path.to_path_buf()),
            enabled: entry.enabled,
            load_error: None,
            manifest: Some(manifest),
        },
        Err(error) => PluginDefinition {
            manifest: None,
            id: entry.id.clone(),
            name: entry.id.clone(),
            version: "-".to_string(),
            source: entry.source,
            root_path: Some(path.to_path_buf()),
            enabled: entry.enabled,
            load_error: Some(error),
        },
    }
}

/// 解析 zip 中唯一合法 manifest 条目下标。
fn find_zip_manifest_index<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
) -> Result<usize, String> {
    let mut candidates = Vec::new();
    let mut top_level = HashSet::new();
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|error| format!("读取插件 ZIP 条目失败：{error}"))?;
        let path = safe_zip_entry_path(file.name())?;
        if let Some(first) = path.components().next() {
            top_level.insert(first.as_os_str().to_string_lossy().to_string());
        }
        if path.file_name().and_then(|name| name.to_str()) == Some(PLUGIN_MANIFEST_FILE_NAME) {
            let component_count = path.components().count();
            if component_count == 1 || component_count == 2 {
                candidates.push(index);
            }
        }
    }
    match candidates.len() {
        1 => {
            let manifest_path = {
                let file = archive
                    .by_index(candidates[0])
                    .map_err(|error| format!("读取插件 manifest 条目失败：{error}"))?;
                safe_zip_entry_path(file.name())?
            };
            if manifest_path.components().count() == 2 && top_level.len() != 1 {
                return Err("插件 ZIP 的单目录模式只能包含一个顶层目录".to_string());
            }
            Ok(candidates[0])
        }
        0 => Err("插件 ZIP 中没有找到 plugin.json".to_string()),
        _ => Err("插件 ZIP 中发现多个 plugin.json，无法判断插件根目录".to_string()),
    }
}

/// 根据 manifest 路径返回解压时需要剥离的顶层目录。
fn zip_manifest_strip_prefix(manifest_path: &Path) -> Option<PathBuf> {
    if manifest_path.components().count() == 2 {
        manifest_path
            .components()
            .next()
            .map(|component| PathBuf::from(component.as_os_str()))
    } else {
        None
    }
}

/// 解压插件 ZIP 到目标目录。
fn extract_plugin_zip<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    strip_prefix: Option<&Path>,
    destination: &Path,
) -> Result<(), String> {
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|error| format!("读取插件 ZIP 条目失败：{error}"))?;
        let path = safe_zip_entry_path(file.name())?;
        if is_zip_symlink(&file) {
            return Err(format!("插件 ZIP 不允许包含符号链接：{}", file.name()));
        }
        let relative_path = if let Some(prefix) = strip_prefix {
            match path.strip_prefix(prefix) {
                Ok(path) if !path.as_os_str().is_empty() => path.to_path_buf(),
                _ => continue,
            }
        } else {
            path
        };
        let target = destination.join(&relative_path);
        ensure_child_path(destination, &target)?;
        if file.is_dir() {
            fs::create_dir_all(&target)
                .map_err(|error| format!("创建插件目录 {} 失败：{}", target.display(), error))?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    format!("创建插件目录 {} 失败：{}", parent.display(), error)
                })?;
            }
            let mut output = File::create(&target)
                .map_err(|error| format!("创建插件文件 {} 失败：{}", target.display(), error))?;
            std::io::copy(&mut file, &mut output)
                .map_err(|error| format!("写入插件文件 {} 失败：{}", target.display(), error))?;
            restore_zip_file_permissions(&file, &target)?;
        }
    }
    Ok(())
}

/// 从 ZIP 条目恢复普通文件权限。
///
/// 业务意图：
/// - 独立插件通过 ZIP 分发时，macOS/Linux 的 sidecar 二进制必须保留可执行位，否则宿主能识别入口文件但启动会失败。
/// - Windows 不依赖 Unix mode，保持空实现，避免把平台权限语义混入通用安装流程。
#[cfg(unix)]
fn restore_zip_file_permissions<R: Read>(
    file: &zip::read::ZipFile<'_, R>,
    target: &Path,
) -> Result<(), String> {
    if let Some(mode) = file.unix_mode() {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(target, fs::Permissions::from_mode(mode & 0o777))
            .map_err(|error| format!("恢复插件文件权限 {} 失败：{}", target.display(), error))?;
    }
    Ok(())
}

/// Windows ZIP 解压不需要恢复 Unix 权限位。
#[cfg(not(unix))]
fn restore_zip_file_permissions<R: Read>(
    _file: &zip::read::ZipFile<'_, R>,
    _target: &Path,
) -> Result<(), String> {
    Ok(())
}

/// 将 zip 条目名转换为安全相对路径。
fn safe_zip_entry_path(name: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(name);
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(format!("插件 ZIP 包含非法路径：{name}"));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(format!("插件 ZIP 包含路径穿越条目：{name}"));
            }
        }
    }
    Ok(path)
}

/// 插件 stdout 读取线程发回的事件。
enum PluginStdoutReadEvent {
    /// 读取到一行 stdout。
    Line(String),
    /// 读取 stdout 时发生错误。
    ReadError(String),
}

/// 解析后的插件 stdout 协议行。
enum ParsedPluginStdoutLine {
    /// 进度事件。
    Progress(PluginCommandProgress),
    /// 最终响应。
    Response(PluginCommandResponse),
}

/// 等待插件进程返回最终响应并转发流式进度。
///
/// 业务意图：
/// - 第三方插件进程不可信，不能因为插件卡住而让后台任务永久占用资源。
/// - 大数据插件需要在最终响应前持续反馈进度，因此 stdout 必须边读边解析，不能等进程结束后一次性读取。
///
/// 边界条件：
/// - 超时后会主动 kill 子进程并回收退出状态；读取线程会因管道关闭自然结束。
/// - 插件 stderr 只作为错误诊断，不参与协议解析，避免普通诊断日志污染最终响应。
fn wait_for_plugin_response(
    mut child: std::process::Child,
    timeout: Duration,
    progress_sender: Option<mpsc::Sender<PluginCommandProgress>>,
) -> Result<PluginCommandResponse, String> {
    wait_for_plugin_response_inner(&mut child, timeout, progress_sender, None)
}

/// 等待插件响应，同时监听 stdin 写入线程结果。
///
/// 业务意图：
/// - 日志正文流可能很大，宿主必须一边写插件 stdin，一边读取插件 stdout 进度；否则插件进度输出填满管道后会和宿主输入流互相等待。
/// - stdin 写入错误需要由等待循环统一 kill 子进程并转换成中文错误，避免后台线程静默失败。
fn wait_for_plugin_response_with_stdin_result(
    mut child: std::process::Child,
    timeout: Duration,
    progress_sender: Option<mpsc::Sender<PluginCommandProgress>>,
    stdin_result_receiver: mpsc::Receiver<Result<(), String>>,
) -> Result<PluginCommandResponse, String> {
    wait_for_plugin_response_inner(
        &mut child,
        timeout,
        progress_sender,
        Some(stdin_result_receiver),
    )
}

/// 等待插件进程返回最终响应的共享实现。
fn wait_for_plugin_response_inner(
    child: &mut std::process::Child,
    timeout: Duration,
    progress_sender: Option<mpsc::Sender<PluginCommandProgress>>,
    stdin_result_receiver: Option<mpsc::Receiver<Result<(), String>>>,
) -> Result<PluginCommandResponse, String> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "插件 stdout 管道不可用".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "插件 stderr 管道不可用".to_string())?;
    let (stdout_sender, stdout_receiver) = mpsc::channel();
    let stdout_reader = thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let event = match line {
                Ok(line) => PluginStdoutReadEvent::Line(line),
                Err(error) => {
                    PluginStdoutReadEvent::ReadError(format!("读取插件 stdout 失败：{error}"))
                }
            };
            if stdout_sender.send(event).is_err() {
                break;
            }
        }
    });

    let (stderr_sender, stderr_receiver) = mpsc::channel();
    let stderr_reader = thread::spawn(move || {
        let mut stderr_raw = Vec::new();
        let read_result = BufReader::new(stderr).read_to_end(&mut stderr_raw);
        let message = match read_result {
            Ok(_) => String::from_utf8_lossy(&stderr_raw).trim().to_string(),
            Err(error) => format!("读取插件 stderr 失败：{error}"),
        };
        let _ = stderr_sender.send(message);
    });

    let start = Instant::now();
    let mut final_response = None;
    let mut stdin_finished = stdin_result_receiver.is_none();
    loop {
        drain_plugin_stdout_events(
            &stdout_receiver,
            progress_sender.as_ref(),
            &mut final_response,
        )?;
        if !stdin_finished && let Some(receiver) = stdin_result_receiver.as_ref() {
            match receiver.try_recv() {
                Ok(Ok(())) => stdin_finished = true,
                Ok(Err(error)) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err(error);
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    return Err("插件 stdin 写入线程异常结束".to_string());
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                drain_plugin_stdout_events(
                    &stdout_receiver,
                    progress_sender.as_ref(),
                    &mut final_response,
                )?;
                if !stdin_finished && let Some(receiver) = stdin_result_receiver.as_ref() {
                    match receiver.recv_timeout(Duration::from_millis(200)) {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => return Err(error),
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            return Err("插件进程已退出，但日志正文流尚未写入完成".to_string());
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            return Err("插件 stdin 写入线程异常结束".to_string());
                        }
                    }
                }
                let stderr = stderr_receiver.try_recv().unwrap_or_default();
                if !status.success() {
                    return Err(format!("插件进程退出失败：{}", stderr.trim()));
                }
                return final_response.ok_or_else(|| "插件没有返回最终响应".to_string());
            }
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err("插件执行超时，已终止插件进程".to_string());
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(format!("等待插件进程失败：{error}")),
        }
    }
}

/// 读取并应用所有已经到达的插件 stdout 协议行。
fn drain_plugin_stdout_events(
    receiver: &mpsc::Receiver<PluginStdoutReadEvent>,
    progress_sender: Option<&mpsc::Sender<PluginCommandProgress>>,
    final_response: &mut Option<PluginCommandResponse>,
) -> Result<(), String> {
    loop {
        match receiver.try_recv() {
            Ok(PluginStdoutReadEvent::Line(line)) => match parse_plugin_stdout_line(&line)? {
                Some(ParsedPluginStdoutLine::Progress(progress)) => {
                    if let Some(sender) = progress_sender {
                        let _ = sender.send(progress);
                    }
                }
                Some(ParsedPluginStdoutLine::Response(response)) => {
                    if final_response.replace(response).is_some() {
                        return Err("插件返回了多个最终响应".to_string());
                    }
                }
                None => {}
            },
            Ok(PluginStdoutReadEvent::ReadError(message)) => return Err(message),
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
        }
    }
}

/// 解析插件 stdout 单行协议。
fn parse_plugin_stdout_line(line: &str) -> Result<Option<ParsedPluginStdoutLine>, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    if let Ok(event) = serde_json::from_str::<PluginCommandEvent>(line) {
        return Ok(Some(match event {
            PluginCommandEvent::Progress { progress } => ParsedPluginStdoutLine::Progress(progress),
        }));
    }
    serde_json::from_str::<PluginCommandResponse>(line)
        .map(|response| Some(ParsedPluginStdoutLine::Response(response)))
        .map_err(|error| format!("解析插件 stdout 协议行失败：{error}"))
}

/// 判断 zip 条目是否为符号链接。
fn is_zip_symlink<R: Read>(file: &zip::read::ZipFile<'_, R>) -> bool {
    file.unix_mode()
        .map(|mode| (mode & 0o170000) == 0o120000)
        .unwrap_or(false)
}

/// 确保目标路径仍位于指定父目录下。
fn ensure_child_path(parent: &Path, child: &Path) -> Result<(), String> {
    let parent = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    let child_parent = child
        .parent()
        .unwrap_or(child)
        .canonicalize()
        .unwrap_or_else(|_| child.parent().unwrap_or(child).to_path_buf());
    if child_parent.starts_with(&parent) {
        Ok(())
    } else {
        Err(format!("插件解压目标越界：{}", child.display()))
    }
}

/// 解析插件命令入口。
fn resolve_plugin_command_path(raw: &str, root_path: Option<&Path>) -> Result<String, String> {
    if raw.contains("${app_dir}") {
        return Err("插件入口不再支持 ${app_dir}，请把可执行文件随插件一起打包".to_string());
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        return Ok(raw.to_string());
    }
    if let Some(root_path) = root_path {
        for candidate in plugin_entry_candidates(root_path, raw) {
            if candidate.exists() {
                return Ok(candidate.display().to_string());
            }
        }
        if raw.contains('/') || raw.contains('\\') {
            let candidate = root_path.join(raw);
            return Ok(candidate.display().to_string());
        }
    }
    Ok(raw.to_string())
}

/// 校验 ZIP 插件入口文件是否随包安装。
///
/// 业务意图：
/// - ZIP 是面向分发的插件形态，必须自包含 sidecar 二进制或脚本，不能依赖主程序目录或用户 PATH。
/// - 开发目录插件仍允许开发者自行管理可执行入口，ZIP 安装则在导入时直接暴露缺失文件，避免运行时才失败。
fn validate_installed_zip_entry(manifest: &PluginManifest, root_path: &Path) -> Result<(), String> {
    let Some(command) = manifest.entry.command.first() else {
        return Err("插件入口命令不能为空".to_string());
    };
    if command.contains("${app_dir}") {
        return Err("ZIP 插件入口不支持 ${app_dir}，请使用相对插件目录路径".to_string());
    }
    let command_path = Path::new(command);
    if command_path.is_absolute() {
        return Err("ZIP 插件入口必须使用相对插件目录路径".to_string());
    }
    if command_path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("ZIP 插件入口路径不能包含 .、.. 或根路径".to_string());
    }
    if plugin_entry_candidates(root_path, command)
        .into_iter()
        .any(|path| path.is_file())
    {
        Ok(())
    } else {
        Err(format!("ZIP 插件缺少入口文件：{command}"))
    }
}

/// 返回插件入口命令在插件根目录下的候选路径。
///
/// 业务意图：
/// - manifest 使用无扩展名命令名可以同时服务 macOS/Linux 和 Windows；Windows 安装包只需放置 `.exe` 文件。
fn plugin_entry_candidates(root_path: &Path, command: &str) -> Vec<PathBuf> {
    let mut candidates = vec![root_path.join(command)];
    if Path::new(command).extension().is_none() {
        candidates.push(root_path.join(format!("{command}.exe")));
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write};
    use zip::write::SimpleFileOptions;

    /// 构造测试用插件 manifest。
    fn sample_manifest(id: &str) -> String {
        format!(
            r#"{{
  "api_version": 1,
  "id": "{id}",
  "name": "测试插件",
  "version": "0.1.0",
  "entry": {{ "command": ["test-plugin"] }},
  "permissions": ["ui.log_tree_menu"],
  "contributes": {{
    "log_tree_context_menu": [
      {{ "id": "parse", "title": "解析", "icon": "Search" }}
    ]
  }}
}}"#
        )
    }

    /// 创建独立临时目录，避免插件安装测试污染真实配置目录。
    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "logclinic-plugin-test-{}-{}",
            std::process::id(),
            name
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("测试目录应能创建");
        path
    }

    #[test]
    fn 解析合法_plugin_json_manifest() {
        let manifest = parse_plugin_manifest(&sample_manifest("example.plugin"))
            .expect("合法 manifest 应能解析");
        manifest.validate().expect("合法 manifest 应通过校验");
        assert_eq!(manifest.id, "example.plugin");
        assert_eq!(
            manifest.contributes.log_tree_context_menu[0].command_id(),
            "parse"
        );
    }

    #[test]
    fn manifest_兼容日志工具栏和插件设置页签贡献() {
        let raw = r#"{
  "api_version": 1,
  "id": "weaver-logext",
  "name": "泛微日志插件",
  "version": "0.3.0",
  "entry": {"command": ["weaver-logext"]},
  "permissions": ["ui.log_toolbar", "ui.settings_tabs"],
  "contributes": {
    "log_toolbar": [
      {
        "id": "weaver.log_scan",
        "title": "泛微日志分析",
        "icon": "Search",
        "command": "weaver_log_scan"
      }
    ],
    "settings_tabs": [
      {
        "id": "weaver.settings",
        "title": "泛微插件",
        "icon": "Settings",
        "pattern_settings": [
          {
            "key": "memory",
            "label": "内存日志",
            "default": "memory_yyyy-MM-dd.log"
          }
        ]
      }
    ]
  }
}"#;
        let manifest = parse_plugin_manifest(raw).expect("新贡献点 manifest 应能解析");
        manifest.validate().expect("新贡献点 manifest 应通过校验");

        assert_eq!(
            manifest.contributes.log_toolbar[0].command_id(),
            "weaver_log_scan"
        );
        assert_eq!(manifest.contributes.settings_tabs[0].title, "泛微插件");
        assert_eq!(
            plugin_settings_defaults(&manifest)
                .get("memory")
                .map(String::as_str),
            Some("memory_yyyy-MM-dd.log")
        );
    }

    #[test]
    fn manifest_拒绝跨设置页签重复规则键() {
        let raw = r#"{
  "api_version": 1,
  "id": "duplicate-settings.plugin",
  "name": "重复设置插件",
  "version": "0.1.0",
  "entry": {"command": ["settings-plugin"]},
  "contributes": {
    "settings_tabs": [
      {
        "id": "settings-a",
        "title": "设置 A",
        "pattern_settings": [
          {"key": "memory", "label": "内存日志", "default": "memory.log"}
        ]
      },
      {
        "id": "settings-b",
        "title": "设置 B",
        "pattern_settings": [
          {"key": "memory", "label": "另一个内存日志", "default": "memory2.log"}
        ]
      }
    ]
  }
}"#;
        let manifest = parse_plugin_manifest(raw).expect("重复 key manifest 应能先完成 JSON 解析");
        let error = manifest
            .validate()
            .expect_err("跨设置页签重复 key 应被拒绝");

        assert!(
            error.contains("插件设置规则键重复：memory"),
            "错误信息应指出重复的规则键，实际为：{error}"
        );
    }

    #[test]
    fn 插件设置读取损坏回退保存并恢复默认规则() {
        let manifest_raw = r#"{
  "api_version": 1,
  "id": "settings.plugin",
  "name": "设置插件",
  "version": "0.1.0",
  "entry": {"command": ["settings-plugin"]},
  "contributes": {
    "settings_tabs": [
      {
        "id": "settings",
        "title": "插件设置",
        "pattern_settings": [
          {"key": "memory", "label": "内存日志", "default": "memory_yyyy-MM-dd.log"},
          {"key": "stdout", "label": "输出日志", "default": "stdout.log"}
        ]
      }
    ]
  }
}"#;
        let manifest = parse_plugin_manifest(manifest_raw).expect("设置 manifest 应能解析");
        let defaults = plugin_settings_defaults(&manifest);
        let path = temp_dir("plugin-settings").join("settings.plugin.json");

        fs::write(&path, "{broken").expect("测试损坏设置应能写入");
        assert_eq!(
            load_plugin_settings_from_path(&path, defaults.clone()),
            defaults
        );

        let mut custom = BTreeMap::new();
        custom.insert("memory".to_string(), "custom.log".to_string());
        custom.insert("unknown".to_string(), "ignored".to_string());
        save_plugin_settings_to_path(&path, &custom).expect("测试设置应能保存");
        let merged = load_plugin_settings_from_path(&path, defaults.clone());
        assert_eq!(merged.get("memory").map(String::as_str), Some("custom.log"));
        assert_eq!(merged.get("stdout").map(String::as_str), Some("stdout.log"));
        assert!(!merged.contains_key("unknown"));

        save_plugin_settings_to_path(&path, &defaults).expect("默认设置应能写回");
        assert_eq!(
            load_plugin_settings_from_path(&path, defaults.clone()),
            defaults
        );
    }

    #[test]
    fn 拒绝未知插件协议版本() {
        let raw =
            sample_manifest("example.plugin").replace("\"api_version\": 1", "\"api_version\": 2");
        let manifest = parse_plugin_manifest(&raw).expect("JSON 本身应能解析");
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn 注册表_upsert_会更新已有插件() {
        let mut registry = PluginRegistry::default();
        registry.upsert(PluginRegistryEntry {
            id: "plugin".to_string(),
            source: PluginInstallSource::DevelopmentDirectory,
            path: Some(PathBuf::from("/a")),
            enabled: true,
        });
        registry.upsert(PluginRegistryEntry {
            id: "plugin".to_string(),
            source: PluginInstallSource::DevelopmentDirectory,
            path: Some(PathBuf::from("/b")),
            enabled: false,
        });
        assert_eq!(registry.entries.len(), 1);
        assert!(!registry.entries[0].enabled);
        assert_eq!(registry.entries[0].path, Some(PathBuf::from("/b")));
    }

    #[test]
    fn 注册表迁移会跳过旧来源条目并保留独立插件() {
        let raw = r#"{
  "entries": [
    {
      "id": "weaver-logext",
      "source": "LegacySource",
      "path": null,
      "enabled": false
    },
    {
      "id": "dev.plugin",
      "source": "DevelopmentDirectory",
      "path": "/tmp/dev.plugin",
      "enabled": true
    }
  ]
}"#;
        let registry = parse_plugin_registry_tolerant(raw);
        assert_eq!(registry.entries.len(), 1);
        assert_eq!(registry.entries[0].id, "dev.plugin");
        assert_eq!(
            registry.entries[0].source,
            PluginInstallSource::DevelopmentDirectory
        );
    }

    #[test]
    fn 插件_stdout_协议行支持进度事件和最终响应() {
        let progress_line = r#"{"event":"progress","progress":{"message":"正在处理","detail":"a.log","done":1,"total":3,"unit":"文件"}}"#;
        let parsed_progress = parse_plugin_stdout_line(progress_line).expect("进度事件应能解析");
        let Some(ParsedPluginStdoutLine::Progress(progress)) = parsed_progress else {
            panic!("应解析为进度事件");
        };
        assert_eq!(progress.done, 1);
        assert_eq!(progress.total, Some(3));

        let response_line = r#"{"action":"show_message","level":"info","message":"完成"}"#;
        let parsed_response = parse_plugin_stdout_line(response_line).expect("最终响应应能解析");
        let Some(ParsedPluginStdoutLine::Response(PluginCommandResponse::ShowMessage {
            message,
            ..
        })) = parsed_response
        else {
            panic!("应解析为最终响应");
        };
        assert_eq!(message, "完成");
    }

    #[test]
    fn 插件表格行动作响应可解析并保持旧表格兼容() {
        let response_line = serde_json::json!({
            "action": "open_window",
            "title": "汇总",
            "page": {
                "title": "汇总",
                "description": null,
                "stats": [],
                "table": {
                    "headers": ["请求地址", "操作"],
                    "rows": [["/api", ""]],
                    "row_actions": [[{
                        "label": "详情",
                        "title": "详情 - /api",
                        "page": {
                            "title": "详情",
                            "description": null,
                            "stats": [],
                            "table": {
                                "headers": ["耗时(ms)"],
                                "rows": [["100"]]
                            }
                        }
                    }]]
                }
            }
        })
        .to_string();
        let parsed_response = parse_plugin_stdout_line(&response_line).expect("行动作响应应能解析");
        let Some(ParsedPluginStdoutLine::Response(PluginCommandResponse::OpenWindow {
            page, ..
        })) = parsed_response
        else {
            panic!("应解析为打开窗口响应");
        };
        let table = page.table.expect("响应应包含表格");
        assert_eq!(table.row_actions.len(), 1);
        assert_eq!(table.row_actions[0][0].label, "详情");
        assert_eq!(
            table.row_actions[0][0]
                .page
                .as_ref()
                .map(|page| page.title.as_str()),
            Some("详情")
        );

        let command_response_line = serde_json::json!({
            "action": "open_window",
            "title": "明细",
            "page": {
                "title": "明细",
                "description": null,
                "stats": [],
                "table": {
                    "headers": ["请求", "操作"],
                    "rows": [["/api", ""]],
                    "row_actions": [[{
                        "label": "显示SQL",
                        "title": "SQL - /api",
                        "command": {
                            "command_id": "show_request_sql",
                            "context": {
                                "type": "log_file_action",
                                "action_id": "show_sql",
                                "file": {
                                    "source_key": "local:/tmp/a.log",
                                    "display_name": "a.log",
                                    "path_label": "/tmp",
                                    "source_kind": "local_file",
                                    "node_kind": "file",
                                    "read_path": "/tmp/a.log"
                                },
                                "data": {"route": "/api"}
                            }
                        }
                    }]]
                }
            }
        })
        .to_string();
        let command_response =
            parse_plugin_stdout_line(&command_response_line).expect("延迟命令响应应能解析");
        let Some(ParsedPluginStdoutLine::Response(PluginCommandResponse::OpenWindow {
            page, ..
        })) = command_response
        else {
            panic!("应解析为打开窗口响应");
        };
        let command_table = page.table.expect("响应应包含表格");
        let action = &command_table.row_actions[0][0];
        assert!(action.page.is_none());
        assert_eq!(
            action
                .command
                .as_ref()
                .map(|command| command.command_id.as_str()),
            Some("show_request_sql")
        );

        let old_response_line = r#"{"action":"open_window","title":"旧表格","page":{"title":"旧表格","description":null,"stats":[],"table":{"headers":["列"],"rows":[["值"]]}}}"#;
        let old_response =
            parse_plugin_stdout_line(old_response_line).expect("旧表格响应仍应能解析");
        let Some(ParsedPluginStdoutLine::Response(PluginCommandResponse::OpenWindow {
            page, ..
        })) = old_response
        else {
            panic!("应解析为打开窗口响应");
        };
        assert!(page.table.expect("旧响应应包含表格").row_actions.is_empty());
    }

    #[test]
    fn 插件日志正文路径会按权限剥离() {
        let mut manifest = parse_plugin_manifest(&sample_manifest("permission.plugin"))
            .expect("manifest 应能解析");
        let plugin = PluginDefinition {
            manifest: Some(manifest.clone()),
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            source: PluginInstallSource::DevelopmentDirectory,
            root_path: None,
            enabled: true,
            load_error: None,
        };
        let context = PluginCommandContext::LogFileAction {
            action_id: "show_sql".to_string(),
            file: PluginLogFile {
                source_key: "local:/tmp/a.log".to_string(),
                display_name: "a.log".to_string(),
                path_label: "/tmp".to_string(),
                source_kind: "local_file".to_string(),
                node_kind: "file".to_string(),
                read_path: Some("/tmp/a.log".to_string()),
                host_source: None,
            },
            data: BTreeMap::new(),
        };

        let stripped = sanitize_plugin_context_for_permissions(&plugin, context.clone());
        let PluginCommandContext::LogFileAction { file, .. } = stripped else {
            panic!("应保持日志文件行动作上下文");
        };
        assert!(file.read_path.is_none());

        manifest.permissions.push("logs.content".to_string());
        let content_plugin = PluginDefinition {
            manifest: Some(manifest.clone()),
            id: manifest.id,
            name: manifest.name,
            version: manifest.version,
            source: PluginInstallSource::DevelopmentDirectory,
            root_path: None,
            enabled: true,
            load_error: None,
        };
        let kept = sanitize_plugin_context_for_permissions(&content_plugin, context);
        let PluginCommandContext::LogFileAction { file, .. } = kept else {
            panic!("应保持日志文件行动作上下文");
        };
        assert_eq!(file.read_path.as_deref(), Some("/tmp/a.log"));
    }

    #[test]
    fn zip_manifest_支持顶层_plugin_json() {
        let path = temp_dir("zip-root").join("plugin.zip");
        {
            let file = File::create(&path).expect("测试 zip 应能创建");
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file(PLUGIN_MANIFEST_FILE_NAME, SimpleFileOptions::default())
                .unwrap();
            writer
                .write_all(sample_manifest("zip.root").as_bytes())
                .unwrap();
            writer.finish().unwrap();
        }
        let file = File::open(path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        assert!(find_zip_manifest_index(&mut archive).is_ok());
    }

    #[test]
    fn zip_manifest_支持单顶层目录_plugin_json() {
        let path = temp_dir("zip-dir").join("plugin.zip");
        {
            let file = File::create(&path).expect("测试 zip 应能创建");
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file("sample/plugin.json", SimpleFileOptions::default())
                .unwrap();
            writer
                .write_all(sample_manifest("zip.dir").as_bytes())
                .unwrap();
            writer.finish().unwrap();
        }
        let file = File::open(path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        assert!(find_zip_manifest_index(&mut archive).is_ok());
    }

    #[test]
    fn zip_manifest_单目录模式拒绝多个顶层条目() {
        let path = temp_dir("zip-multi-top").join("plugin.zip");
        {
            let file = File::create(&path).expect("测试 zip 应能创建");
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file("sample/plugin.json", SimpleFileOptions::default())
                .unwrap();
            writer
                .write_all(sample_manifest("zip.multi").as_bytes())
                .unwrap();
            writer
                .start_file("other/readme.txt", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"ignored").unwrap();
            writer.finish().unwrap();
        }
        let file = File::open(path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();
        assert!(find_zip_manifest_index(&mut archive).is_err());
    }

    #[test]
    fn zip_manifest_拒绝路径穿越() {
        assert!(safe_zip_entry_path("../plugin.json").is_err());
        assert!(safe_zip_entry_path("/plugin.json").is_err());
        assert!(safe_zip_entry_path("./plugin.json").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn zip解压会保留插件入口可执行权限() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp_dir("zip-exec-permission");
        let path = root.join("plugin.zip");
        {
            let file = File::create(&path).expect("测试 zip 应能创建");
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file(PLUGIN_MANIFEST_FILE_NAME, SimpleFileOptions::default())
                .expect("测试 manifest 条目应能写入");
            writer
                .write_all(sample_manifest("zip.exec").as_bytes())
                .expect("测试 manifest 内容应能写入");
            writer
                .start_file(
                    "test-plugin",
                    SimpleFileOptions::default().unix_permissions(0o755),
                )
                .expect("测试入口条目应能写入");
            writer
                .write_all(b"#!/bin/sh\nexit 0\n")
                .expect("测试入口内容应能写入");
            writer.finish().expect("测试 zip 应能结束写入");
        }

        let file = File::open(path).expect("测试 zip 应能打开");
        let mut archive = ZipArchive::new(file).expect("测试 zip 应能读取");
        let destination = root.join("out");
        fs::create_dir_all(&destination).expect("测试解压目录应能创建");
        extract_plugin_zip(&mut archive, None, &destination).expect("插件 zip 应能安全解压");
        let mode = fs::metadata(destination.join("test-plugin"))
            .expect("测试入口文件应存在")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
    }
}
