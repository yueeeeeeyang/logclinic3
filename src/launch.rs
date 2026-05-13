//! LogClinic 启动路径解析模块。
//!
//! 业务意图：
//! - 集中处理命令行参数和平台 open-url 回调传入的文件、目录、压缩包路径。
//! - 该模块不访问磁盘、不判断文件是否存在，只把平台输入转换为后续加载模块可处理的 `PathBuf`。
//!
//! 跨平台约束：
//! - Windows 拖拽到程序图标通常走命令行参数；macOS Finder/Dock 打开通常走 `file://` URL。
//! - URL 百分号编码只按 UTF-8 尝试解码，损坏编码保留原文本交给加载层显示错误。

use std::{env, path::PathBuf};

/// 启动或系统“用 LogClinic 打开”传入路径后的分流计划。
///
/// 业务意图：
/// - 系统右键菜单会把任意文件路径交给同一个进程入口，日志查看和 HPROF 解析需要在启动层先分流。
/// - 日志分析页可以一次加载多个文件、目录或压缩包；HPROF 分析页当前一次只能解析一个 dump，因此只保留第一个 `.hprof/.bin`。
///
/// 边界条件：
/// - 扩展名大小写不敏感，兼容 Windows 和现场导出的 `DUMP.HPROF`、`heap.BIN`。
/// - 多个 HPROF 候选同时传入时忽略第二个及之后的候选，避免当前单视图 HPROF 页被连续启动任务覆盖。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LaunchOpenPlan {
    /// 进入日志分析页的路径集合。
    pub(crate) log_paths: Vec<PathBuf>,
    /// 进入 HPROF 解析页的首个 dump 路径。
    pub(crate) hprof_path: Option<PathBuf>,
}

/// 从进程启动参数中提取可加载路径。
///
/// 业务意图：
/// - Windows 上把文件、目录或压缩包拖到程序图标时，通常会以命令行参数形式启动进程。
/// - macOS/Linux 的命令行手动启动也可复用该入口，方便开发期验证“启动即加载”行为。
///
/// 边界条件：
/// - 第一个参数是可执行文件路径，必须跳过。
/// - 这里只保留非空参数，不强制要求路径存在；不存在时由加载模块生成可见错误节点。
pub(crate) fn log_source_paths_from_launch_arguments() -> Vec<PathBuf> {
    env::args_os()
        .skip(1)
        .filter(|argument| !argument.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// 从平台 open-url 回调中提取本地文件路径。
///
/// 业务意图：
/// - macOS 把文件拖到 Dock 图标或双击关联文件时，GPUI 会通过 `on_open_urls` 传入 `file://` URL。
/// - 部分平台可能直接传入普通路径，因此这里同时兼容 file URL 和裸路径。
///
/// 边界条件：
/// - 非 `file://` URL 不属于本地日志来源，直接忽略。
/// - URL 百分号编码只在 UTF-8 成功时解码；损坏编码保留原片段，让后续加载层显示路径错误。
pub(crate) fn log_source_paths_from_open_urls(urls: Vec<String>) -> Vec<PathBuf> {
    urls.into_iter()
        .filter_map(|url| log_source_path_from_open_url(&url))
        .collect()
}

/// 将系统传入路径拆分为日志分析和 HPROF 解析两类入口。
///
/// 业务意图：
/// - 右键菜单注册为“所有文件”后，应用不能再假设启动参数一定是日志文本或压缩包。
/// - `.hprof` 与 `.bin` 按用户确认进入 HPROF 解析，其余普通文件、目录、压缩包和未知扩展继续走日志分析。
pub(crate) fn classify_launch_paths(paths: Vec<PathBuf>) -> LaunchOpenPlan {
    let mut plan = LaunchOpenPlan::default();
    for path in paths {
        if is_hprof_launch_path(&path) {
            if plan.hprof_path.is_none() {
                plan.hprof_path = Some(path);
            }
        } else {
            plan.log_paths.push(path);
        }
    }
    plan
}

/// 判断路径是否应从启动入口进入 HPROF 解析页。
///
/// 边界条件：
/// - 这里只根据文件名扩展名判断，不访问磁盘；无扩展名、目录和压缩包都应继续交给日志分析加载链路。
fn is_hprof_launch_path(path: &std::path::Path) -> bool {
    path.extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|extension| extension == "hprof" || extension == "bin")
}

/// 解析单个平台传入的本地文件 URL 或裸路径。
pub(crate) fn log_source_path_from_open_url(url: &str) -> Option<PathBuf> {
    if let Some(rest) = url.strip_prefix("file://") {
        let decoded = percent_decode_utf8_lossy(rest);
        #[cfg(windows)]
        {
            let path = decoded.strip_prefix('/').unwrap_or(&decoded);
            return Some(PathBuf::from(path));
        }
        #[cfg(not(windows))]
        {
            return Some(PathBuf::from(decoded));
        }
    }

    if url.contains("://") || url.is_empty() {
        None
    } else {
        Some(PathBuf::from(url))
    }
}

/// 解码 file URL 中常见的百分号编码。
///
/// 边界条件：
/// - 只处理完整的 `%XX` 字节；不完整或非法十六进制片段按原字符保留。
/// - 解码后的字节如果不是合法 UTF-8，则回退到原文本，避免构造平台相关的非法路径字节。
fn percent_decode_utf8_lossy(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            decoded.push((high << 4) | low);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(decoded).unwrap_or_else(|_| input.to_string())
}

/// 解析一个 ASCII 十六进制字符。
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! 启动入口路径分流测试。
    //!
    //! 业务意图：
    //! - 系统右键菜单注册为所有文件后，启动层必须稳定地区分 HPROF dump 和普通日志/压缩包来源。
    //! - 测试只验证扩展名分流，不访问磁盘，避免平台文件系统差异影响纯业务规则。

    use super::*;

    /// 验证 HPROF 与 BIN 扩展名会进入 HPROF 解析计划。
    ///
    /// 边界条件：
    /// - 扩展名大小写不敏感，兼容 Windows 和 JVM 工具导出的不同命名习惯。
    #[test]
    fn 启动路径会把_hprof_和_bin_分流到_hprof_解析() {
        let plan = classify_launch_paths(vec![
            PathBuf::from("heap.HPROF"),
            PathBuf::from("second.bin"),
            PathBuf::from("app.log"),
        ]);

        assert_eq!(plan.hprof_path, Some(PathBuf::from("heap.HPROF")));
        assert_eq!(plan.log_paths, vec![PathBuf::from("app.log")]);
    }

    /// 验证普通文件、目录和压缩包都继续进入日志分析计划。
    ///
    /// 业务意图：
    /// - 除 `.hprof/.bin` 之外，启动入口不应按扩展名过滤；真实可读性继续交给日志加载和压缩包模块处理。
    #[test]
    fn 启动路径会把普通来源保留到日志分析() {
        let paths = vec![
            PathBuf::from("app.log"),
            PathBuf::from("logs.zip"),
            PathBuf::from("config.xml"),
            PathBuf::from("directory"),
        ];
        let plan = classify_launch_paths(paths.clone());

        assert_eq!(plan.hprof_path, None);
        assert_eq!(plan.log_paths, paths);
    }
}
