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
