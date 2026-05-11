//! 压缩包成员路径归一化子模块。
//!
//! 业务意图：
//! - 集中维护压缩包内部成员路径的安全拆分、统一连接和单文件 GZIP 虚拟成员路径。
//! - 所有来源模型都使用同一套 `/` 分隔成员路径，避免目录树展示、点击读取、搜索和另存为之间出现不一致。
//!
//! 边界条件：
//! - 本模块只生成内部成员定位字符串，不把成员路径直接拼接为本地文件系统路径。
//! - 绝对路径、Windows 盘符、上级目录、空路径和 NUL 字符必须被拒绝。

use std::path::Path;

/// 单文件 gzip fallback 在内部来源路径中使用的虚拟前缀。
///
/// 业务意图：
/// - 一些用户文件使用 `.tar.gz` 扩展名但实际只是单个 gzip 日志，另一些文件直接使用 `.gz`。
/// - 目录树仍需要为它生成一个可点击的文件节点，因此用一个不会来自正常压缩包路径的内部前缀标记该虚拟成员。
///
/// 边界条件：
/// - 该前缀只在进程内作为 `LogFileSource::ArchiveMember::member_path` 使用，不展示给用户，也不写回磁盘。
/// - 正常 tar 条目路径不会经过该分支；真实条目仍使用安全归一化后的 `/` 分隔路径。
pub(crate) const SINGLE_GZIP_MEMBER_PREFIX: &str = "__logclinic_single_gzip__/";

/// 为“单个 gzip 日志”生成稳定的虚拟成员路径。
///
/// 业务意图：
/// - 用户看到的根节点仍是原始 `.tar.gz` 或 `.gz` 文件，子文件节点使用去掉压缩扩展名后的名称，避免显示内部实现前缀。
/// - 虚拟路径需要稳定，才能让 tab 去重、分页物化和另存为逻辑复用现有压缩包成员入口。
pub(crate) fn single_gzip_member_path_for_archive(path: &Path) -> String {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "解压内容".to_string());
    let lower_name = file_name.to_ascii_lowercase();
    let display_name = if lower_name.ends_with(".tar.gz") {
        file_name
            .get(..file_name.len().saturating_sub(".tar.gz".len()))
            .unwrap_or("解压内容")
            .to_string()
    } else if lower_name.ends_with(".tgz") {
        file_name
            .get(..file_name.len().saturating_sub(".tgz".len()))
            .unwrap_or("解压内容")
            .to_string()
    } else if lower_name.ends_with(".gz") {
        file_name
            .get(..file_name.len().saturating_sub(".gz".len()))
            .unwrap_or("解压内容")
            .to_string()
    } else {
        file_name
    };
    let display_name = if display_name.trim().is_empty() {
        "解压内容".to_string()
    } else {
        display_name
    };

    format!("{SINGLE_GZIP_MEMBER_PREFIX}{display_name}")
}

/// 判断成员路径是否表示单文件 gzip fallback。
///
/// 边界条件：
/// - 该判断只看内部前缀，不读取文件内容；真实 gzip 可读性由扫描或读取阶段分别校验。
pub(crate) fn is_single_gzip_member_path(member_path: &str) -> bool {
    member_path.starts_with(SINGLE_GZIP_MEMBER_PREFIX)
}

/// 返回单文件 gzip fallback 的用户可见文件名。
pub(crate) fn single_gzip_member_display_name(member_path: &str) -> String {
    member_path
        .strip_prefix(SINGLE_GZIP_MEMBER_PREFIX)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("解压内容")
        .to_string()
}

/// 将压缩包原始条目名拆为安全、稳定的路径片段。
///
/// 业务意图：
/// - ZIP/RAR/TAR/7Z 的条目分隔符和原始路径规则不完全一致，统一拆分后目录树和读取逻辑可以共享成员路径。
///
/// 安全边界：
/// - 拒绝绝对路径，避免 `/var/log/a.log` 这类条目看起来像真实本机路径。
/// - 拒绝盘符路径，避免 `C:\logs\a.log` 在 Windows 语境下产生歧义。
/// - 拒绝 `..`，避免后续接入读取或解压能力时遗留路径穿越风险。
/// - 拒绝空片段和 NUL 字符，避免展示异常或底层 API 解析差异。
pub(crate) fn split_archive_entry_path(raw_name: &str) -> Result<Vec<String>, String> {
    let normalized = raw_name.replace('\\', "/");
    let trimmed = normalized.trim_matches('/');

    if normalized.starts_with('/') {
        return Err("压缩包条目是绝对路径".to_string());
    }

    if normalized.len() >= 2 && normalized.as_bytes()[1] == b':' {
        return Err("压缩包条目包含 Windows 盘符路径".to_string());
    }

    if trimmed.is_empty() {
        return Err("压缩包条目路径为空".to_string());
    }

    let mut segments = Vec::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }

        if segment == ".." {
            return Err("压缩包条目包含上级目录片段".to_string());
        }

        if segment.contains('\0') {
            return Err("压缩包条目包含非法 NUL 字符".to_string());
        }

        segments.push(segment.to_string());
    }

    if segments.is_empty() {
        return Err("压缩包条目路径为空".to_string());
    }

    Ok(segments)
}

/// 将压缩包内部路径归一化为安全、稳定、跨平台的成员路径。
///
/// 业务意图：
/// - 日志目录树和日志内容读取必须使用同一套路径安全规则，否则树中可见的条目可能无法被点击打开。
/// - 归一化结果统一使用 `/` 分隔，便于作为 `LogFileSource::ArchiveMember::member_path` 的稳定定位。
///
/// 边界条件：
/// - 该函数只接受安全相对路径；绝对路径、盘符路径、上级目录和 NUL 字符都会返回错误。
/// - 返回值用于匹配压缩包条目，不会直接拼接到本地文件系统路径，因此不会触发实际解压写入。
pub fn normalize_archive_member_path(raw_name: &str) -> Result<String, String> {
    split_archive_entry_path(raw_name).map(|segments| join_archive_segments(&segments))
}

/// 将已经安全拆分的压缩包路径片段重新连接成成员路径。
///
/// 业务意图：
/// - 集中使用 `/` 作为压缩包内部路径分隔符，避免 ZIP、RAR、TAR 和 7Z 各自保留不同原始分隔符。
pub(crate) fn join_archive_segments(segments: &[String]) -> String {
    segments.join("/")
}
