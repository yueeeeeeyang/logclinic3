//! 压缩包成员流式物化模块。
//!
//! 业务意图：
//! - 超大日志如果位于 ZIP/RAR/TAR.GZ/7Z 内部，不能先完整读入 `Vec<u8>` 再解码或搜索。
//! - 本模块把用户选中的压缩包成员用固定缓冲区写入应用临时目录，再复用本地分页日志管线。
//! - 临时文件路径只在当前 tab 生命周期内有效，关闭 tab、重新加载或应用启动清理时会删除。
//!
//! 关键约束：
//! - 所有压缩包内部路径必须先经过安全归一化，避免 `../`、绝对路径或盘符路径影响临时目录。
//! - ZIP、TAR.GZ、7Z 使用 reader 到 writer 的流式复制；RAR 受 `unrar` API 限制，使用库提供的 `extract_to` 直接写入目标文件。
//! - 加密、损坏、权限不足、磁盘空间不足等错误会转成中文 `LogContentError`，不会回退到内存读取。

use std::{
    fs::{self, File},
    io::{self, BufReader, BufWriter},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use flate2::read::GzDecoder;
use tar::Archive as TarArchive;
use zip::ZipArchive;

use crate::{
    log_content::{LogContentError, single_file_archive_member_path_from_path},
    log_loader::{ArchiveFormat, LogFileSource, normalize_archive_member_path},
};

/// 物化完成后的日志来源。
///
/// 业务意图：
/// - `original_source` 保留用户真实点击的文件，用于 tab 去重、搜索结果展示和错误上下文。
/// - `temp_path` 是分页读取实际访问的普通文件；它可能来自压缩包成员或单文件嵌套压缩包。
#[derive(Clone, Debug)]
pub struct MaterializedLogSource {
    /// 用户选择的原始来源。
    pub original_source: LogFileSource,
    /// 可 seek 的本地临时文件路径。
    pub temp_path: PathBuf,
    /// 物化后文件字节数。
    pub byte_len: u64,
}

/// 应用超大日志缓存根目录。
///
/// 边界条件：
/// - 使用系统临时目录，不依赖用户配置目录权限。
/// - 按进程 ID 分 session，异常退出遗留目录可在下次启动时清理。
pub fn large_log_cache_root() -> PathBuf {
    std::env::temp_dir()
        .join("LogClinic")
        .join("large-log-cache")
}

/// 当前进程的物化目录。
pub fn large_log_session_dir() -> PathBuf {
    large_log_cache_root().join(format!("session-{}", std::process::id()))
}

/// 清理旧的超大日志缓存目录。
///
/// 业务意图：
/// - 正常关闭 tab 会删除对应物化文件；异常退出可能留下 session 目录，过期清理可避免长期占用磁盘。
/// - 只清理明显过期的目录，避免多个 LogClinic 实例同时运行时误删其它实例正在使用的分页文件。
pub fn cleanup_stale_large_log_cache() {
    let root = large_log_cache_root();
    let current_session = large_log_session_dir();
    let stale_after = Duration::from_secs(24 * 60 * 60);
    let Ok(entries) = fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path != current_session && is_stale_cache_dir(&path, stale_after) {
            let _ = fs::remove_dir_all(path);
        }
    }
}

/// 判断缓存目录是否已过期。
fn is_stale_cache_dir(path: &Path, stale_after: Duration) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    modified
        .elapsed()
        .is_ok_and(|elapsed| elapsed >= stale_after)
}

/// 删除单个物化文件及其空父目录。
///
/// 边界条件：
/// - 清理失败不应影响用户关闭 tab 或退出应用，因此这里静默忽略错误。
pub fn cleanup_materialized_file(path: &Path) {
    let _ = fs::remove_file(path);
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent);
    }
}

/// 将需要分页处理的来源转换成可 seek 的本地文件。
///
/// 业务意图：
/// - 普通本地日志直接返回原路径，避免复制 10GB 文件。
/// - 压缩包成员和单文件压缩包必须先物化，因为压缩流本身无法按任意行 seek。
pub fn materialize_source_for_paging(
    source: &LogFileSource,
) -> Result<MaterializedLogSource, LogContentError> {
    match source {
        LogFileSource::LocalFile { path } => {
            if let Some(format) = ArchiveFormat::from_path(path) {
                let label = path.display().to_string();
                let member_path =
                    single_file_archive_member_path_from_path(path, format, label.as_str())?;
                let nested_source = LogFileSource::ArchiveMember {
                    archive_path: path.clone(),
                    archive_format: format,
                    member_path,
                };
                return materialize_archive_member(&nested_source);
            }

            let byte_len = fs::metadata(path)
                .map_err(|error| {
                    LogContentError::new(format!(
                        "无法读取日志文件 {} 的大小：{}",
                        path.display(),
                        error
                    ))
                })?
                .len();
            Ok(MaterializedLogSource {
                original_source: source.clone(),
                temp_path: path.clone(),
                byte_len,
            })
        }
        LogFileSource::ArchiveMember { .. } => materialize_archive_member(source),
        LogFileSource::MaterializedArchiveMember {
            member_path,
            temp_path,
            ..
        } => {
            // 7Z 加载阶段已经把成员写成可 seek 的本地临时文件；分页管线直接复用该文件。
            // 如果成员本身又是单文件压缩包，则沿用普通本地文件规则先物化唯一内部日志。
            if let Some(format) = ArchiveFormat::from_path(Path::new(member_path)) {
                let member_path =
                    single_file_archive_member_path_from_path(temp_path, format, member_path)?;
                let nested_source = LogFileSource::ArchiveMember {
                    archive_path: temp_path.clone(),
                    archive_format: format,
                    member_path,
                };
                let nested = materialize_archive_member(&nested_source)?;
                return Ok(MaterializedLogSource {
                    original_source: source.clone(),
                    temp_path: nested.temp_path,
                    byte_len: nested.byte_len,
                });
            }

            let byte_len = fs::metadata(temp_path)
                .map_err(|error| {
                    LogContentError::new(format!(
                        "无法读取 7Z 物化成员 {} 的大小：{}",
                        temp_path.display(),
                        error
                    ))
                })?
                .len();
            Ok(MaterializedLogSource {
                original_source: source.clone(),
                temp_path: temp_path.clone(),
                byte_len,
            })
        }
        LogFileSource::NestedArchiveMember { .. } => materialize_nested_archive_member(source),
    }
}

/// 物化外层压缩包中的内层压缩包成员。
///
/// 业务意图：
/// - 多文件内层压缩包在左侧树中会展开为目录；点击其中的大日志时仍应复用分页管线，而不是一次性读入内存。
/// - 先把内层压缩包物化成普通文件，再把内层目标成员物化成分页文件，两个步骤都使用固定缓冲区复制。
fn materialize_nested_archive_member(
    source: &LogFileSource,
) -> Result<MaterializedLogSource, LogContentError> {
    let LogFileSource::NestedArchiveMember {
        outer_archive_path,
        outer_archive_format,
        archive_member_path,
        nested_archive_format,
        nested_member_path,
    } = source
    else {
        return Err(LogContentError::new(
            "内部错误：非嵌套压缩包来源不能物化嵌套成员",
        ));
    };

    let inner_archive_source = LogFileSource::ArchiveMember {
        archive_path: outer_archive_path.clone(),
        archive_format: *outer_archive_format,
        member_path: archive_member_path.clone(),
    };
    let inner_archive = materialize_archive_member(&inner_archive_source)?;
    let nested_member_source = LogFileSource::ArchiveMember {
        archive_path: inner_archive.temp_path.clone(),
        archive_format: *nested_archive_format,
        member_path: nested_member_path.clone(),
    };
    let nested = match materialize_archive_member(&nested_member_source) {
        Ok(nested) => nested,
        Err(error) => {
            cleanup_materialized_file(&inner_archive.temp_path);
            return Err(error);
        }
    };
    cleanup_materialized_file(&inner_archive.temp_path);
    Ok(MaterializedLogSource {
        original_source: source.clone(),
        temp_path: nested.temp_path,
        byte_len: nested.byte_len,
    })
}

/// 流式物化压缩包成员。
fn materialize_archive_member(
    source: &LogFileSource,
) -> Result<MaterializedLogSource, LogContentError> {
    let LogFileSource::ArchiveMember {
        archive_path,
        archive_format,
        member_path,
    } = source
    else {
        return Err(LogContentError::new("内部错误：非压缩包来源不能物化成员"));
    };

    let temp_path = materialized_output_path(member_path)?;
    if let Some(parent) = temp_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            LogContentError::new(format!(
                "无法创建超大日志临时目录 {}：{}",
                parent.display(),
                error
            ))
        })?;
    }

    match archive_format {
        ArchiveFormat::Zip => materialize_zip_member(archive_path, member_path, &temp_path)?,
        ArchiveFormat::Rar => materialize_rar_member(archive_path, member_path, &temp_path)?,
        ArchiveFormat::TarGz => materialize_tar_gz_member(archive_path, member_path, &temp_path)?,
        ArchiveFormat::SevenZ => materialize_7z_member(archive_path, member_path, &temp_path)?,
    }

    let byte_len = fs::metadata(&temp_path)
        .map_err(|error| {
            LogContentError::new(format!(
                "无法读取物化日志 {} 的大小：{}",
                temp_path.display(),
                error
            ))
        })?
        .len();

    // 如果物化出来的成员本身仍是单文件压缩包，继续物化内部唯一普通文件。
    // 这样“单文件压缩包当作文件本身打开”的既有行为在分页模式下保持一致。
    if let Some(nested_format) = ArchiveFormat::from_path(Path::new(member_path)) {
        let label = member_path.to_string();
        let nested_member =
            single_file_archive_member_path_from_path(&temp_path, nested_format, label.as_str())?;
        let nested_source = LogFileSource::ArchiveMember {
            archive_path: temp_path.clone(),
            archive_format: nested_format,
            member_path: nested_member,
        };
        let nested = match materialize_archive_member(&nested_source) {
            Ok(nested) => nested,
            Err(error) => {
                cleanup_materialized_file(&temp_path);
                return Err(error);
            }
        };
        cleanup_materialized_file(&temp_path);
        return Ok(MaterializedLogSource {
            original_source: source.clone(),
            temp_path: nested.temp_path,
            byte_len: nested.byte_len,
        });
    }

    Ok(MaterializedLogSource {
        original_source: source.clone(),
        temp_path,
        byte_len,
    })
}

/// 生成安全的物化输出路径，并保留压缩包内部目录层级。
fn materialized_output_path(member_path: &str) -> Result<PathBuf, LogContentError> {
    let normalized = normalize_archive_member_path(member_path).map_err(|reason| {
        LogContentError::new(format!("压缩包成员路径非法，无法物化：{}", reason))
    })?;
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut path = large_log_session_dir().join(format!("member-{unique}"));
    for part in normalized.split('/') {
        path.push(part);
    }
    Ok(path)
}

/// 物化 ZIP 成员。
fn materialize_zip_member(
    archive_path: &Path,
    member_path: &str,
    temp_path: &Path,
) -> Result<(), LogContentError> {
    let file = File::open(archive_path).map_err(|error| {
        LogContentError::new(format!(
            "无法打开 ZIP 压缩包 {}：{}",
            archive_path.display(),
            error
        ))
    })?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|error| LogContentError::new(format!("无法读取 ZIP 目录：{}", error)))?;
    let mut matched_index = None;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            LogContentError::new(format!("无法读取 ZIP 条目 {}：{}", index, error))
        })?;
        let normalized = normalize_archive_member_path(entry.name()).map_err(|reason| {
            LogContentError::new(format!("ZIP 条目路径非法，无法物化：{}", reason))
        })?;
        if normalized == member_path {
            matched_index = Some(index);
            break;
        }
    }
    let Some(matched_index) = matched_index else {
        return Err(LogContentError::new(format!(
            "ZIP 压缩包中未找到成员 {}",
            member_path
        )));
    };
    let mut entry = archive.by_index(matched_index).map_err(|error| {
        LogContentError::new(format!("无法读取 ZIP 成员 {}：{}", member_path, error))
    })?;
    if entry.is_dir() {
        return Err(LogContentError::new(format!(
            "ZIP 成员 {} 是目录，不能作为日志打开",
            member_path
        )));
    }
    let mut writer = BufWriter::new(File::create(temp_path).map_err(|error| {
        LogContentError::new(format!(
            "无法创建临时日志文件 {}：{}",
            temp_path.display(),
            error
        ))
    })?);
    io::copy(&mut entry, &mut writer)
        .map_err(|error| LogContentError::new(format!("无法物化 ZIP 成员：{}", error)))?;
    Ok(())
}

/// 物化 RAR 成员。
fn materialize_rar_member(
    archive_path: &Path,
    member_path: &str,
    temp_path: &Path,
) -> Result<(), LogContentError> {
    let mut archive = unrar::Archive::new(archive_path)
        .open_for_processing()
        .map_err(|error| LogContentError::new(format!("无法打开 RAR 压缩包：{}", error)))?;
    while let Some(header) = archive
        .read_header()
        .map_err(|error| LogContentError::new(format!("无法读取 RAR 条目：{}", error)))?
    {
        let raw_name = header.entry().filename.to_string_lossy();
        let normalized = normalize_archive_member_path(&raw_name)
            .map_err(|reason| LogContentError::new(format!("RAR 条目路径非法：{}", reason)))?;
        if normalized != member_path {
            archive = header
                .skip()
                .map_err(|error| LogContentError::new(format!("无法跳过 RAR 条目：{}", error)))?;
            continue;
        }
        if header.entry().is_encrypted() {
            return Err(LogContentError::new(format!(
                "RAR 成员 {} 已加密，暂不支持读取",
                member_path
            )));
        }
        header
            .extract_to(temp_path)
            .map_err(|error| LogContentError::new(format!("无法物化 RAR 成员：{}", error)))?;
        return Ok(());
    }

    Err(LogContentError::new(format!(
        "RAR 压缩包中未找到成员 {}",
        member_path
    )))
}

/// 物化 TAR.GZ 成员。
fn materialize_tar_gz_member(
    archive_path: &Path,
    member_path: &str,
    temp_path: &Path,
) -> Result<(), LogContentError> {
    let file = File::open(archive_path).map_err(|error| {
        LogContentError::new(format!(
            "无法打开 TAR.GZ 压缩包 {}：{}",
            archive_path.display(),
            error
        ))
    })?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let mut archive = TarArchive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| LogContentError::new(format!("无法读取 TAR.GZ 目录：{}", error)))?;
    for entry in entries {
        let mut entry = entry
            .map_err(|error| LogContentError::new(format!("无法读取 TAR.GZ 条目：{}", error)))?;
        let entry_path = entry.path().map_err(|error| {
            LogContentError::new(format!("无法读取 TAR.GZ 条目路径：{}", error))
        })?;
        let raw_name = entry_path.to_string_lossy();
        let normalized = normalize_archive_member_path(&raw_name)
            .map_err(|reason| LogContentError::new(format!("TAR.GZ 条目路径非法：{}", reason)))?;
        if normalized != member_path {
            continue;
        }
        let mut writer = BufWriter::new(File::create(temp_path).map_err(|error| {
            LogContentError::new(format!(
                "无法创建临时日志文件 {}：{}",
                temp_path.display(),
                error
            ))
        })?);
        io::copy(&mut entry, &mut writer)
            .map_err(|error| LogContentError::new(format!("无法物化 TAR.GZ 成员：{}", error)))?;
        return Ok(());
    }

    Err(LogContentError::new(format!(
        "TAR.GZ 压缩包中未找到成员 {}",
        member_path
    )))
}

/// 物化 7Z 成员。
fn materialize_7z_member(
    archive_path: &Path,
    member_path: &str,
    temp_path: &Path,
) -> Result<(), LogContentError> {
    let mut found = false;
    let mut write_error = None;
    let mut reader = sevenz_rust::SevenZReader::open(archive_path, sevenz_rust::Password::empty())
        .map_err(|error| LogContentError::new(format!("无法打开 7Z 压缩包：{}", error)))?;
    reader
        .for_each_entries(|entry, reader| {
            if entry.is_directory() {
                return Ok(true);
            }
            let normalized = match normalize_archive_member_path(&entry.name) {
                Ok(path) => path,
                Err(reason) => {
                    write_error =
                        Some(LogContentError::new(format!("7Z 条目路径非法：{}", reason)));
                    return Ok(false);
                }
            };
            if normalized != member_path {
                io::copy(reader, &mut io::sink()).map_err(|error| sevenz_rust::Error::io(error))?;
                return Ok(true);
            }
            found = true;
            let result = File::create(temp_path)
                .and_then(|file| {
                    let mut writer = BufWriter::new(file);
                    io::copy(reader, &mut writer).map(|_| ())
                })
                .map_err(|error| {
                    LogContentError::new(format!("无法物化 7Z 成员 {}：{}", member_path, error))
                });
            if let Err(error) = result {
                write_error = Some(error);
            }
            Ok(false)
        })
        .map_err(|error| LogContentError::new(format!("无法读取 7Z 条目：{}", error)))?;

    if let Some(error) = write_error {
        return Err(error);
    }
    if found {
        Ok(())
    } else {
        Err(LogContentError::new(format!(
            "7Z 压缩包中未找到成员 {}",
            member_path
        )))
    }
}
