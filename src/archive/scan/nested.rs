//! 嵌套压缩包扫描辅助。
//!
//! 业务意图：
//! - 从压缩包扫描父模块拆出内层压缩包读取、临时物化和嵌套树合并逻辑。
//! - 顶层格式扫描入口、扫描结果结构和错误文案保持在父模块，确保日志加载适配行为不变。

use super::*;

/// 为扫描内层压缩包创建会话级临时根目录。
///
/// 业务意图：
/// - RAR 的目录扫描和读取 API 都需要真实文件路径，无法直接从内存 reader 扫描。
/// - 外层 RAR 或 ZIP/TAR.GZ 中的内层 RAR 需要先物化为临时文件，再复用 `scan_archive`。
///
/// 边界条件：
/// - 临时目录加入 `temporary_paths`，由 UI 在重新加载或退出时清理。
/// - 目录名包含时间戳和来源标签，避免同一进程多次加载同名归档互相覆盖。
pub(super) fn nested_archive_scan_materialized_root(
    label: &str,
) -> Result<PathBuf, ArchiveScanError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ArchiveScanError::new(format!("无法生成内层压缩包临时时间戳：{}", error)))?
        .as_nanos();
    let safe_label = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    Ok(std::env::temp_dir()
        .join("LogClinic")
        .join("large-log-cache")
        .join(format!("session-{}", std::process::id()))
        .join(format!("nested-archive-{}-{}", nanos, safe_label)))
}

/// 计算物化后的内层压缩包文件路径。
pub(super) fn materialized_nested_archive_path(
    root: &Path,
    raw_name: &str,
) -> Result<PathBuf, ArchiveScanError> {
    let segments = split_archive_entry_path(raw_name).map_err(|reason| {
        ArchiveScanError::new(format!("内层压缩包路径非法，无法物化：{}", reason))
    })?;
    let mut path = root.to_path_buf();
    for segment in segments {
        path.push(segment);
    }
    Ok(path)
}

/// 将 RAR 成员物化为本地文件，供内层压缩包目录扫描使用。
///
/// 业务意图：
/// - `unrar` 的列表接口只能告诉我们条目名和大小，不能直接把条目 reader 交给 ZIP/7Z/TAR 扫描。
/// - 只在条目扩展名已经确认是受支持压缩包时调用，避免对普通日志额外解压。
pub(super) fn materialize_rar_member_for_nested_scan(
    archive_path: &Path,
    member_path: &str,
    materialized_root: &mut Option<PathBuf>,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<PathBuf, ArchiveScanError> {
    if materialized_root.is_none() {
        let label = archive_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "archive.rar".to_string());
        let root = nested_archive_scan_materialized_root(&label)?;
        fs::create_dir_all(&root).map_err(|error| {
            ArchiveScanError::new(format!(
                "无法创建内层压缩包临时目录 {}：{}",
                root.display(),
                error
            ))
        })?;
        temporary_paths.push(root.clone());
        *materialized_root = Some(root);
    }

    let root = materialized_root
        .as_ref()
        .ok_or_else(|| ArchiveScanError::new("内部错误：内层压缩包临时目录未初始化"))?;
    let temp_path = materialized_nested_archive_path(root, member_path)?;
    if let Some(parent) = temp_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            ArchiveScanError::new(format!(
                "无法创建内层压缩包父目录 {}：{}",
                parent.display(),
                error
            ))
        })?;
    }

    let mut archive = unrar::Archive::new(archive_path)
        .open_for_processing()
        .map_err(|error| ArchiveScanError::new(format!("无法打开 RAR 内容：{}", error)))?;
    while let Some(header) = archive
        .read_header()
        .map_err(|error| ArchiveScanError::new(format!("无法读取 RAR 条目：{}", error)))?
    {
        let raw_name = header.entry().filename.to_string_lossy();
        let normalized = match normalize_archive_member_path(&raw_name) {
            Ok(path) => path,
            Err(_) => {
                archive = header.skip().map_err(|error| {
                    ArchiveScanError::new(format!("无法跳过非法 RAR 条目：{}", error))
                })?;
                continue;
            }
        };
        if normalized != member_path {
            archive = header
                .skip()
                .map_err(|error| ArchiveScanError::new(format!("无法跳过 RAR 条目：{}", error)))?;
            continue;
        }
        if header.entry().is_encrypted() {
            return Err(ArchiveScanError::new(format!(
                "RAR 成员 {} 已加密，暂不支持展开为目录",
                member_path
            )));
        }
        if header.entry().is_directory() {
            return Err(ArchiveScanError::new(format!(
                "RAR 成员 {} 是目录，不能作为内层压缩包扫描",
                member_path
            )));
        }
        header.extract_to(&temp_path).map_err(|error| {
            ArchiveScanError::new(format!("无法物化 RAR 内层压缩包：{}", error))
        })?;
        return Ok(temp_path);
    }

    Err(ArchiveScanError::new(format!(
        "RAR 压缩包中未找到内层压缩包 {}",
        member_path
    )))
}

/// 从已经物化成本地文件的内层压缩包读取压缩包扫描树。
///
/// 业务意图：
/// - 顶层 7Z 成员已经落到临时目录后，扫描内层压缩包不再需要再次从顶层 7Z 顺序解压。
/// - 如果内层压缩包包含多个普通文件，继续作为目录挂载；单文件压缩包仍按既有规则作为文件本身打开。
pub(super) fn read_nested_archive_tree_from_path(
    archive_path: &Path,
    nested_format: ArchiveFormat,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    error_count: &mut usize,
    temporary_paths: &mut Vec<PathBuf>,
) -> Option<ArchiveScanNode> {
    let nested_format = if nested_format == ArchiveFormat::TarGz {
        ArchiveFormat::from_file(archive_path).unwrap_or(nested_format)
    } else {
        nested_format
    };
    let segments = split_archive_entry_path(archive_member_path).ok()?;
    let label = segments.last()?.clone();
    let mut nested_root = ArchiveScanNode::new(label, ArchiveScanEntryKind::Directory);
    // 内层压缩包扫描发生在外层条目处理期间；当前加载进度已经由外层条目推进负责展示。
    // 这里使用空回调，避免把内层条目数误算成顶层压缩包的总进度，导致中央进度条来回跳动。
    let mut ignore_nested_progress = |_progress| {};
    scan_archive_into(
        archive_path,
        nested_format,
        &mut nested_root,
        error_count,
        temporary_paths,
        &mut ignore_nested_progress,
    )
    .ok()?;

    if nested_root.descendant_file_count() <= 1 {
        return None;
    }

    if outer_archive_format != ArchiveFormat::SevenZ {
        rewrite_nested_local_sources(
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
        );
    }
    Some(nested_root)
}

/// 将内层压缩包压缩包扫描树中的普通来源改写为嵌套来源。
///
/// 业务意图：
/// - ZIP/TAR.GZ 内层压缩包扫描会产生 `ArchiveMember`，需要保留“外层压缩包 + 内层压缩包路径 + 内层成员路径”的语义。
/// - 7Z 内层压缩包会先物化为本地文件，此时保留 `LocalFile` 来源即可避免再次随机读取 7Z。
pub(super) fn rewrite_nested_local_sources(
    node: &mut ArchiveScanNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
) {
    if let Some(ArchiveMemberSource::Direct { member_path, .. }) = node.source.clone() {
        node.source = Some(ArchiveMemberSource::Nested {
            outer_archive_path: outer_archive_path.to_path_buf(),
            outer_archive_format,
            archive_member_path: archive_member_path.to_string(),
            nested_archive_format,
            nested_member_path: member_path,
        });
    }

    for child in &mut node.children {
        rewrite_nested_local_sources(
            child,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_archive_format,
        );
    }
}

/// 尝试从外层压缩包条目中读取内层压缩包压缩包扫描树。
///
/// 业务意图：
/// - 外层压缩包内如果包含多文件压缩包，用户希望把该内层压缩包当作目录展开并选择具体文件。
/// - 只有内层压缩包包含两个及以上普通文件时才替换成目录；单文件内层压缩包继续保留“当作文件本身打开”的既有行为。
///
/// 边界条件：
/// - 加载树阶段不能为巨大内层压缩包无上限占用内存，超过 `NESTED_ARCHIVE_SCAN_MAX_BYTES` 时直接回退为普通文件节点。
/// - RAR 内层压缩包需要路径型 API，因此会先写入临时文件，再复用普通压缩包扫描逻辑。
/// - 参数对象分别描述外层来源、错误计数和临时路径归属，避免嵌套扫描入口继续拉长签名。
pub(super) struct NestedArchiveReaderContext<'a> {
    /// 外层压缩包路径。
    pub(super) outer_archive_path: &'a Path,
    /// 外层压缩包格式。
    pub(super) outer_archive_format: ArchiveFormat,
    /// 外层压缩包中内层压缩包成员路径。
    pub(super) archive_member_path: &'a str,
    /// 当前压缩包扫描树加载的错误计数。
    pub(super) error_count: &'a mut usize,
    /// 当前加载会话需要清理的临时路径。
    pub(super) temporary_paths: &'a mut Vec<PathBuf>,
}

/// 尝试从外层压缩包条目中读取内层压缩包压缩包扫描树。
pub(super) fn read_nested_archive_tree_from_reader(
    reader: &mut dyn Read,
    declared_size: Option<u64>,
    nested_format: ArchiveFormat,
    context: NestedArchiveReaderContext<'_>,
) -> Option<ArchiveScanNode> {
    let NestedArchiveReaderContext {
        outer_archive_path,
        outer_archive_format,
        archive_member_path,
        error_count,
        temporary_paths,
    } = context;

    let size = declared_size?;
    if size > NESTED_ARCHIVE_SCAN_MAX_BYTES {
        return None;
    }

    let mut bytes = Vec::with_capacity(size.min(1024 * 1024) as usize);
    let mut limited_reader = reader.take(NESTED_ARCHIVE_SCAN_MAX_BYTES + 1);
    if limited_reader.read_to_end(&mut bytes).is_err()
        || bytes.len() as u64 > NESTED_ARCHIVE_SCAN_MAX_BYTES
    {
        return None;
    }

    let nested_format = nested_format.resolve_from_bytes(&bytes);

    if nested_format == ArchiveFormat::Rar {
        let temp_path =
            materialize_nested_archive_bytes_for_scan(&bytes, archive_member_path, temporary_paths)
                .ok()?;
        return read_nested_archive_tree_from_path(
            &temp_path,
            nested_format,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            error_count,
            temporary_paths,
        );
    }

    let segments = split_archive_entry_path(archive_member_path).ok()?;
    let label = segments.last()?.clone();
    let mut nested_root = ArchiveScanNode::new(label, ArchiveScanEntryKind::Directory);
    match nested_format {
        ArchiveFormat::Zip => scan_nested_zip_archive(
            &bytes,
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
            error_count,
        ),
        ArchiveFormat::Tar => scan_nested_tar_archive(
            &bytes,
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
            error_count,
        ),
        ArchiveFormat::TarGz => scan_nested_tar_gz_archive(
            &bytes,
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
            error_count,
        ),
        ArchiveFormat::SevenZ => scan_nested_7z_archive(
            &bytes,
            &mut nested_root,
            outer_archive_path,
            outer_archive_format,
            archive_member_path,
            nested_format,
            error_count,
        ),
        ArchiveFormat::Gzip => return None,
        ArchiveFormat::Rar => return None,
    }
    .ok()?;

    (nested_root.descendant_file_count() > 1).then_some(nested_root)
}

/// 将内存中的内层压缩包字节写入临时文件。
///
/// 业务意图：
/// - RAR 只能从路径读取目录；当 RAR 位于 ZIP/TAR.GZ 这类 reader 型外层压缩包中时，需要先落盘。
/// - 只写入小于扫描阈值的内层压缩包，避免目录构建阶段消耗大量磁盘和内存。
pub(super) fn materialize_nested_archive_bytes_for_scan(
    bytes: &[u8],
    archive_member_path: &str,
    temporary_paths: &mut Vec<PathBuf>,
) -> Result<PathBuf, ArchiveScanError> {
    let root = nested_archive_scan_materialized_root(archive_member_path)?;
    fs::create_dir_all(&root).map_err(|error| {
        ArchiveScanError::new(format!(
            "无法创建内存内层压缩包临时目录 {}：{}",
            root.display(),
            error
        ))
    })?;
    temporary_paths.push(root.clone());
    let temp_path = materialized_nested_archive_path(&root, archive_member_path)?;
    if let Some(parent) = temp_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            ArchiveScanError::new(format!(
                "无法创建内存内层压缩包父目录 {}：{}",
                parent.display(),
                error
            ))
        })?;
    }
    fs::write(&temp_path, bytes).map_err(|error| {
        ArchiveScanError::new(format!(
            "无法写入内存内层压缩包临时文件 {}：{}",
            temp_path.display(),
            error
        ))
    })?;
    Ok(temp_path)
}

/// 扫描内存中的 ZIP 内层压缩包。
pub(super) fn scan_nested_zip_archive(
    archive_bytes: &[u8],
    root: &mut ArchiveScanNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    error_count: &mut usize,
) -> Result<(), ArchiveScanError> {
    let mut archive = ZipArchive::new(Cursor::new(archive_bytes))
        .map_err(|error| ArchiveScanError::new(format!("无法读取嵌套 ZIP 目录：{}", error)))?;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|error| {
            ArchiveScanError::new(format!("无法读取嵌套 ZIP 条目 {}：{}", index, error))
        })?;
        add_nested_archive_entry(
            root,
            NestedArchiveEntryContext {
                outer_archive_path,
                outer_archive_format,
                archive_member_path,
                nested_archive_format,
            },
            NestedArchiveEntryInfo {
                raw_name: entry.name(),
                is_directory: entry.is_dir(),
                size: Some(entry.size()),
            },
            error_count,
        );
    }
    Ok(())
}

/// 扫描内存中的 TAR 内层压缩包。
///
/// 业务意图：
/// - 外层压缩包中的 `.tar` 或误命名 `.tar.gz` 成员，如果包含多个文件，需要在左侧树中作为目录展开。
/// - 该函数不经过 gzip 解码，专门处理纯 tar 字节。
pub(super) fn scan_nested_tar_archive(
    archive_bytes: &[u8],
    root: &mut ArchiveScanNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    error_count: &mut usize,
) -> Result<(), ArchiveScanError> {
    let mut archive = TarArchive::new(Cursor::new(archive_bytes));
    let entries = archive
        .entries()
        .map_err(|error| ArchiveScanError::new(format!("无法读取嵌套 TAR 目录：{}", error)))?;
    for entry in entries {
        let mut entry = entry
            .map_err(|error| ArchiveScanError::new(format!("无法读取嵌套 TAR 条目：{}", error)))?;
        let entry_path = entry.path().map_err(|error| {
            ArchiveScanError::new(format!("无法读取嵌套 TAR 条目路径：{}", error))
        })?;
        let raw_name = entry_path.to_string_lossy().to_string();
        let is_directory = entry.header().entry_type().is_dir();
        let size = entry.size();
        if !is_directory {
            drain_tar_entry(&mut entry).map_err(|error| {
                ArchiveScanError::new(format!("无法跳过嵌套 TAR 条目内容：{}", error))
            })?;
        }
        add_nested_archive_entry(
            root,
            NestedArchiveEntryContext {
                outer_archive_path,
                outer_archive_format,
                archive_member_path,
                nested_archive_format,
            },
            NestedArchiveEntryInfo {
                raw_name: &raw_name,
                is_directory,
                size: Some(size),
            },
            error_count,
        );
    }
    Ok(())
}

/// 扫描内存中的 TAR.GZ 内层压缩包。
pub(super) fn scan_nested_tar_gz_archive(
    archive_bytes: &[u8],
    root: &mut ArchiveScanNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    error_count: &mut usize,
) -> Result<(), ArchiveScanError> {
    let decoder = GzDecoder::new(Cursor::new(archive_bytes));
    let mut archive = TarArchive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| ArchiveScanError::new(format!("无法读取嵌套 TAR.GZ 目录：{}", error)))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| {
            ArchiveScanError::new(format!("无法读取嵌套 TAR.GZ 条目：{}", error))
        })?;
        let entry_path = entry.path().map_err(|error| {
            ArchiveScanError::new(format!("无法读取嵌套 TAR.GZ 条目路径：{}", error))
        })?;
        let raw_name = entry_path.to_string_lossy().to_string();
        let is_directory = entry.header().entry_type().is_dir();
        let size = entry.size();
        if !is_directory {
            drain_tar_entry(&mut entry).map_err(|error| {
                ArchiveScanError::new(format!("无法跳过嵌套 TAR.GZ 条目内容：{}", error))
            })?;
        }
        add_nested_archive_entry(
            root,
            NestedArchiveEntryContext {
                outer_archive_path,
                outer_archive_format,
                archive_member_path,
                nested_archive_format,
            },
            NestedArchiveEntryInfo {
                raw_name: &raw_name,
                is_directory,
                size: Some(size),
            },
            error_count,
        );
    }
    Ok(())
}

/// 扫描内存中的 7Z 内层压缩包。
pub(super) fn scan_nested_7z_archive(
    archive_bytes: &[u8],
    root: &mut ArchiveScanNode,
    outer_archive_path: &Path,
    outer_archive_format: ArchiveFormat,
    archive_member_path: &str,
    nested_archive_format: ArchiveFormat,
    error_count: &mut usize,
) -> Result<(), ArchiveScanError> {
    let reader = sevenz_rust::SevenZReader::new(
        Cursor::new(archive_bytes),
        archive_bytes.len() as u64,
        sevenz_rust::Password::empty(),
    )
    .map_err(|error| ArchiveScanError::new(format!("无法读取嵌套 7Z 目录：{}", error)))?;
    for entry in &reader.archive().files {
        add_nested_archive_entry(
            root,
            NestedArchiveEntryContext {
                outer_archive_path,
                outer_archive_format,
                archive_member_path,
                nested_archive_format,
            },
            NestedArchiveEntryInfo {
                raw_name: entry.name(),
                is_directory: entry.is_directory(),
                size: Some(entry.size),
            },
            error_count,
        );
    }
    Ok(())
}

/// 把已扫描出的内层压缩包压缩包扫描树挂到外层压缩包压缩包扫描树中。
pub(super) fn add_nested_archive_tree(
    root: &mut ArchiveScanNode,
    archive_member_path: &str,
    nested_tree: ArchiveScanNode,
    error_count: &mut usize,
) {
    let segments = match split_archive_entry_path(archive_member_path) {
        Ok(segments) => segments,
        Err(reason) => {
            *error_count += 1;
            root.add_archive_error_entry(archive_member_path, "非法路径", reason);
            return;
        }
    };
    root.add_subtree_path(&segments, nested_tree);
}

/// 把内层压缩包条目添加到内层压缩包扫描树。
///
/// 业务意图：
/// - 内层压缩包被当作目录显示，但其子文件仍需要保留“外层压缩包 + 内层压缩包成员 + 内层文件成员”的完整读取来源。
/// - 这里同时构建树节点和完整读取来源，外层上下文和条目元数据分别通过参数对象传入，避免位置参数误传。
pub(super) struct NestedArchiveEntryContext<'a> {
    /// 外层压缩包路径。
    outer_archive_path: &'a Path,
    /// 外层压缩包格式。
    outer_archive_format: ArchiveFormat,
    /// 外层压缩包中的内层压缩包成员路径。
    archive_member_path: &'a str,
    /// 内层压缩包格式。
    nested_archive_format: ArchiveFormat,
}

/// 内层压缩包条目的轻量元数据。
pub(super) struct NestedArchiveEntryInfo<'a> {
    /// 内层条目原始路径。
    raw_name: &'a str,
    /// 是否目录条目。
    is_directory: bool,
    /// 条目未压缩大小。
    size: Option<u64>,
}

/// 把内层压缩包条目添加到内层压缩包扫描树。
pub(super) fn add_nested_archive_entry(
    root: &mut ArchiveScanNode,
    context: NestedArchiveEntryContext<'_>,
    entry: NestedArchiveEntryInfo<'_>,
    error_count: &mut usize,
) {
    let NestedArchiveEntryContext {
        outer_archive_path,
        outer_archive_format,
        archive_member_path,
        nested_archive_format,
    } = context;
    let NestedArchiveEntryInfo {
        raw_name,
        is_directory,
        size,
    } = entry;

    let segments = match split_archive_entry_path(raw_name) {
        Ok(segments) => segments,
        Err(reason) => {
            *error_count += 1;
            root.add_archive_error_entry(raw_name, "非法路径", reason);
            return;
        }
    };

    let kind = if is_directory {
        ArchiveScanEntryKind::Directory
    } else {
        ArchiveScanEntryKind::File
    };
    let meta = if is_directory {
        None
    } else {
        size.map(format_byte_size)
    };
    let source = if is_directory {
        None
    } else {
        Some(ArchiveMemberSource::Nested {
            outer_archive_path: outer_archive_path.to_path_buf(),
            outer_archive_format,
            archive_member_path: archive_member_path.to_string(),
            nested_archive_format,
            nested_member_path: join_archive_segments(&segments),
        })
    };

    root.add_leaf_path(&segments, kind, meta, source, None);
}
