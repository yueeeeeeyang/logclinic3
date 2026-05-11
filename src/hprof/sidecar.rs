//! HPROF sidecar 缓存读写子模块。
//!
//! 业务意图：
//! - 该模块集中维护 HPROF 分析结果的磁盘缓存 schema、读写、校验和发布流程。
//! - 从历史大文件中物理拆出后，外部仍通过 `hprof_cache` 模块路径访问，避免改变分析入口和 UI 调用。
//!
//! 边界条件：
//! - sidecar schema、文件名、缓存目录和损坏回退策略保持不变；本次只调整代码组织。
use super::*;

const MANIFEST_FILE: &str = "manifest.json";
const OBJECTS_FILE: &str = "objects.bin";
const DOMINATOR_FILE: &str = "dominator.bin";
const CLASSES_FILE: &str = "classes.bin";
const THREADS_FILE: &str = "threads.bin";
const STRINGS_FILE: &str = "strings.bin";
const EDGES_OFFSETS_FILE: &str = "edges_offsets.bin";
const EDGES_TARGETS_FILE: &str = "edges_targets.bin";
const ROOT_INDICES_FILE: &str = "root_indices.bin";
const MAGIC_OBJECTS: &[u8] = b"LCHP-objects-v3";
const MAGIC_DOMINATOR: &[u8] = b"LCHP-dominator-v3";
const MAGIC_CLASSES: &[u8] = b"LCHP-classes-v2";
const MAGIC_THREADS: &[u8] = b"LCHP-threads-v2";
const MAGIC_EMPTY: &[u8] = b"LCHP-empty-v2";
const MAX_MAGIC_BYTES: usize = 256;
const MAX_STRING_BYTES: usize = 16 * 1024 * 1024;
/// `objects.bin` 固定摘要记录长度。
///
/// 业务意图：
/// - 缓存命中时需要按 summary index 随机读取可见行摘要；固定记录长度可直接 seek，避免第二次打开全量反序列化对象摘要。
const OBJECT_SUMMARY_RECORD_BYTES: u64 = 50;

/// sidecar manifest。
///
/// 边界条件：
/// - `state` 只有 `complete` 才允许命中；构建中或崩溃残留目录不参与缓存恢复。
#[derive(Clone, Debug, Serialize, Deserialize)]
struct HprofSidecarManifest {
    schema_version: u32,
    mat_semantics_version: u32,
    logclinic_version: String,
    state: String,
    source_file_name: String,
    source_len: u64,
    source_modified_millis: u128,
    head_crc32: u32,
    tail_crc32: u32,
    header: HprofHeader,
    size_model: HprofSizeModel,
    created_millis: u128,
}

/// 源 dump 快速身份信息。
struct SourceIdentity {
    file_name: String,
    len: u64,
    modified_millis: u128,
    head_crc32: u32,
    tail_crc32: u32,
}

/// 尝试从 sidecar 缓存恢复结果。
pub(super) fn try_load_cached_hprof_result<F>(
    path: &Path,
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    cancel_flag: &AtomicBool,
) -> Option<Result<HprofDominatorResult, HprofError>>
where
    F: FnMut(HprofProgress),
{
    progress.stage = HprofAnalysisStage::CheckingCache;
    progress.message = "正在检查 HPROF sidecar 缓存".to_string();
    progress.sub_message.clear();
    progress_reporter(progress.clone());

    let sidecar_dir = sidecar_dir_for_path(path);
    let manifest_path = sidecar_dir.join(MANIFEST_FILE);
    if !manifest_path.is_file() {
        progress.sub_message = "未发现可用 sidecar index，将重新解析".to_string();
        progress_reporter(progress.clone());
        return None;
    }

    let loaded = (|| {
        check_cancel(cancel_flag)?;
        let manifest = read_manifest(&manifest_path)?;
        let identity = source_identity(path)?;
        let invalid_reason = validate_manifest(path, &manifest, &identity);
        if let Some(reason) = invalid_reason {
            progress.sub_message = format!("sidecar index 失效：{reason}");
            progress_reporter(progress.clone());
            return Ok(None);
        }
        ensure_required_files(&sidecar_dir)?;
        progress.stage = HprofAnalysisStage::LoadingCache;
        progress.message = "正在加载 HPROF sidecar index".to_string();
        progress.total_bytes = sidecar_total_bytes(&sidecar_dir)?;
        progress.bytes_read = 0;
        progress.phase_done = 0;
        progress.phase_total = 1;
        progress.phase_unit = "文件";
        progress.sub_message = "准备读取 sidecar index".to_string();
        progress_reporter(progress.clone());
        let mut result = read_cached_result(
            path,
            &sidecar_dir,
            &manifest,
            progress,
            progress_reporter,
            cancel_flag,
        )?;
        result.cache_status = Some("sidecar index 命中".to_string());
        Ok(Some(result))
    })();

    match loaded {
        Ok(Some(result)) => Some(Ok(result)),
        Ok(None) => None,
        Err(HprofError::Canceled) => Some(Err(HprofError::Canceled)),
        Err(error) => {
            progress.sub_message = format!("sidecar index 无法读取，将重新解析：{error}");
            progress_reporter(progress.clone());
            None
        }
    }
}

/// 在大 dump 解析前确认 sidecar 目录可写。
pub(super) fn ensure_sidecar_writable(path: &Path, source_len: u64) -> Result<bool, HprofError> {
    let sidecar_dir = sidecar_dir_for_path(path);
    match std::fs::create_dir_all(&sidecar_dir) {
        Ok(()) => Ok(true),
        Err(error) if source_len >= HPROF_SIDECAR_REQUIRED_BYTES => Err(HprofError::Io(format!(
            "无法创建过程索引文件目录 {}：{}",
            sidecar_dir.display(),
            error
        ))),
        Err(_) => Ok(false),
    }
}

/// 写入完成结果到 sidecar。
pub(super) fn write_hprof_sidecar_result<F>(
    result: &HprofDominatorResult,
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    cancel_flag: &AtomicBool,
) -> Result<(), HprofError>
where
    F: FnMut(HprofProgress),
{
    let sidecar_dir = sidecar_dir_for_path(&result.file_path);
    std::fs::create_dir_all(&sidecar_dir).map_err(|error| {
        HprofError::Io(format!(
            "无法创建过程索引文件目录 {}：{}",
            sidecar_dir.display(),
            error
        ))
    })?;
    let tmp_dir = sidecar_dir.join(format!(".building-{}-{}", std::process::id(), now_millis()));
    std::fs::create_dir_all(&tmp_dir).map_err(|error| {
        HprofError::Io(format!(
            "无法创建过程索引临时目录 {}：{}",
            tmp_dir.display(),
            error
        ))
    })?;

    let write_result = (|| {
        let identity = source_identity(&result.file_path)?;
        let manifest = HprofSidecarManifest {
            schema_version: HPROF_SIDECAR_SCHEMA_VERSION,
            mat_semantics_version: HPROF_MAT_SEMANTICS_VERSION,
            logclinic_version: env!("CARGO_PKG_VERSION").to_string(),
            state: "complete".to_string(),
            source_file_name: identity.file_name,
            source_len: identity.len,
            source_modified_millis: identity.modified_millis,
            head_crc32: identity.head_crc32,
            tail_crc32: identity.tail_crc32,
            header: result.header.clone(),
            size_model: result.size_model.clone(),
            created_millis: now_millis(),
        };
        progress.stage = HprofAnalysisStage::WritingIndex;
        progress.message = "正在写入 HPROF sidecar index".to_string();

        report_cache_write(
            progress,
            progress_reporter,
            0,
            1,
            "文件",
            "准备写入对象摘要",
        )?;
        write_objects_file(
            &tmp_dir.join(OBJECTS_FILE),
            result,
            progress,
            progress_reporter,
            cancel_flag,
        )?;
        check_cancel(cancel_flag)?;
        report_cache_write(
            progress,
            progress_reporter,
            0,
            1,
            "文件",
            "准备写入 dominator 索引",
        )?;
        write_dominator_file(
            &tmp_dir.join(DOMINATOR_FILE),
            result,
            progress,
            progress_reporter,
            cancel_flag,
        )?;
        check_cancel(cancel_flag)?;
        report_cache_write(
            progress,
            progress_reporter,
            0,
            1,
            "文件",
            "准备写入类名索引",
        )?;
        write_classes_file(
            &tmp_dir.join(CLASSES_FILE),
            result,
            progress,
            progress_reporter,
            cancel_flag,
        )?;
        check_cancel(cancel_flag)?;
        report_cache_write(
            progress,
            progress_reporter,
            0,
            1,
            "文件",
            "准备写入线程详情索引",
        )?;
        write_threads_file(
            &tmp_dir.join(THREADS_FILE),
            result,
            progress,
            progress_reporter,
            cancel_flag,
        )?;
        check_cancel(cancel_flag)?;
        report_cache_write(
            progress,
            progress_reporter,
            0,
            4,
            "文件",
            "写入占位过程文件",
        )?;
        write_empty_file(&tmp_dir.join(STRINGS_FILE))?;
        report_cache_write(
            progress,
            progress_reporter,
            1,
            4,
            "文件",
            "写入 strings.bin",
        )?;
        write_empty_file(&tmp_dir.join(EDGES_OFFSETS_FILE))?;
        report_cache_write(
            progress,
            progress_reporter,
            2,
            4,
            "文件",
            "写入 edges_offsets.bin",
        )?;
        write_empty_file(&tmp_dir.join(EDGES_TARGETS_FILE))?;
        report_cache_write(
            progress,
            progress_reporter,
            3,
            4,
            "文件",
            "写入 edges_targets.bin",
        )?;
        write_empty_file(&tmp_dir.join(ROOT_INDICES_FILE))?;
        check_cancel(cancel_flag)?;
        report_cache_write(
            progress,
            progress_reporter,
            4,
            4,
            "文件",
            "写入 root_indices.bin",
        )?;
        report_cache_write(progress, progress_reporter, 0, 1, "文件", "写入完成标记")?;
        write_manifest(&tmp_dir.join(MANIFEST_FILE), &manifest)?;
        publish_tmp_dir(&sidecar_dir, &tmp_dir)
    })();

    write_result?;
    Ok(())
}

fn report_cache_write<F>(
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    done: u64,
    total: u64,
    unit: &'static str,
    sub_message: &str,
) -> Result<(), HprofError>
where
    F: FnMut(HprofProgress),
{
    progress.phase_done = done;
    progress.phase_total = total;
    progress.phase_unit = unit;
    progress.sub_message = sub_message.to_string();
    progress_reporter(progress.clone());
    Ok(())
}

fn sidecar_dir_for_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
        .unwrap_or_else(|| "dump".to_string());
    let sidecar_name = format!("{file_name}.logclinic-hprof");
    path.parent()
        .map(|parent| parent.join(&sidecar_name))
        .unwrap_or_else(|| PathBuf::from(sidecar_name))
}

fn read_manifest(path: &Path) -> Result<HprofSidecarManifest, HprofError> {
    let bytes = std::fs::read(path).map_err(|error| {
        HprofError::Io(format!(
            "读取 sidecar manifest {} 失败：{}",
            path.display(),
            error
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        HprofError::InvalidFormat(format!(
            "sidecar manifest {} 格式损坏：{}",
            path.display(),
            error
        ))
    })
}

fn write_manifest(path: &Path, manifest: &HprofSidecarManifest) -> Result<(), HprofError> {
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|error| {
        HprofError::InvalidFormat(format!("生成 sidecar manifest 失败：{error}"))
    })?;
    std::fs::write(path, bytes).map_err(|error| {
        HprofError::Io(format!(
            "写入 sidecar manifest {} 失败：{}",
            path.display(),
            error
        ))
    })
}

fn validate_manifest(
    path: &Path,
    manifest: &HprofSidecarManifest,
    identity: &SourceIdentity,
) -> Option<String> {
    if manifest.state != "complete" {
        return Some("索引未完成".to_string());
    }
    if manifest.schema_version != HPROF_SIDECAR_SCHEMA_VERSION {
        return Some("schema version 变化".to_string());
    }
    if manifest.mat_semantics_version != HPROF_MAT_SEMANTICS_VERSION {
        return Some("MAT 兼容语义版本变化".to_string());
    }
    if manifest.source_file_name
        != path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    {
        return Some("源文件名变化".to_string());
    }
    if manifest.source_len != identity.len {
        return Some("源文件大小变化".to_string());
    }
    if manifest.source_modified_millis != identity.modified_millis {
        return Some("源文件修改时间变化".to_string());
    }
    if manifest.head_crc32 != identity.head_crc32 || manifest.tail_crc32 != identity.tail_crc32 {
        return Some("源文件首尾校验不一致".to_string());
    }
    None
}

fn ensure_required_files(sidecar_dir: &Path) -> Result<(), HprofError> {
    // `objects.bin` 可能是最大的 sidecar 文件，缓存命中时改为可见行懒读取，不计入启动阶段总字节数。
    for file_name in [
        DOMINATOR_FILE,
        CLASSES_FILE,
        THREADS_FILE,
        STRINGS_FILE,
        EDGES_OFFSETS_FILE,
        EDGES_TARGETS_FILE,
        ROOT_INDICES_FILE,
    ] {
        let path = sidecar_dir.join(file_name);
        if !path.is_file() {
            return Err(HprofError::InvalidFormat(format!(
                "sidecar index 缺少文件：{}",
                path.display()
            )));
        }
    }
    Ok(())
}

/// 统计本次缓存命中需要读取的 sidecar 主体字节数。
///
/// 业务意图：
/// - 缓存加载阶段不再读取源 dump；进度条应反映 sidecar index 的读取进度，否则用户会看到 0 / dump 大小长时间不变。
fn sidecar_total_bytes(sidecar_dir: &Path) -> Result<u64, HprofError> {
    let mut total = 0u64;
    for file_name in [
        OBJECTS_FILE,
        DOMINATOR_FILE,
        CLASSES_FILE,
        THREADS_FILE,
        STRINGS_FILE,
        EDGES_OFFSETS_FILE,
        EDGES_TARGETS_FILE,
        ROOT_INDICES_FILE,
    ] {
        let path = sidecar_dir.join(file_name);
        let len = std::fs::metadata(&path)
            .map_err(|error| {
                HprofError::Io(format!(
                    "读取 sidecar 文件大小 {} 失败：{}",
                    path.display(),
                    error
                ))
            })?
            .len();
        total = total.saturating_add(len);
    }
    Ok(total.max(1))
}

fn publish_tmp_dir(sidecar_dir: &Path, tmp_dir: &Path) -> Result<(), HprofError> {
    for file_name in [
        MANIFEST_FILE,
        OBJECTS_FILE,
        DOMINATOR_FILE,
        CLASSES_FILE,
        THREADS_FILE,
        STRINGS_FILE,
        EDGES_OFFSETS_FILE,
        EDGES_TARGETS_FILE,
        ROOT_INDICES_FILE,
    ] {
        let final_path = sidecar_dir.join(file_name);
        if final_path.exists() {
            std::fs::remove_file(&final_path).map_err(|error| {
                HprofError::Io(format!(
                    "清理旧 sidecar 文件 {} 失败：{}",
                    final_path.display(),
                    error
                ))
            })?;
        }
    }
    for file_name in [
        OBJECTS_FILE,
        DOMINATOR_FILE,
        CLASSES_FILE,
        THREADS_FILE,
        STRINGS_FILE,
        EDGES_OFFSETS_FILE,
        EDGES_TARGETS_FILE,
        ROOT_INDICES_FILE,
        MANIFEST_FILE,
    ] {
        std::fs::rename(tmp_dir.join(file_name), sidecar_dir.join(file_name)).map_err(|error| {
            HprofError::Io(format!("发布 sidecar 文件 {} 失败：{}", file_name, error))
        })?;
    }
    std::fs::remove_dir(tmp_dir).map_err(|error| {
        HprofError::Io(format!(
            "清理 sidecar 临时目录 {} 失败：{}",
            tmp_dir.display(),
            error
        ))
    })
}

fn source_identity(path: &Path) -> Result<SourceIdentity, HprofError> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        HprofError::Io(format!(
            "读取源 dump 元数据 {} 失败：{}",
            path.display(),
            error
        ))
    })?;
    let modified_millis = metadata
        .modified()
        .ok()
        .and_then(system_time_millis)
        .unwrap_or(0);
    let (head_crc32, tail_crc32) = file_edge_crc32(path, metadata.len())?;
    Ok(SourceIdentity {
        file_name: path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_string())
            .unwrap_or_default(),
        len: metadata.len(),
        modified_millis,
        head_crc32,
        tail_crc32,
    })
}

fn file_edge_crc32(path: &Path, len: u64) -> Result<(u32, u32), HprofError> {
    let mut file = File::open(path).map_err(|error| {
        HprofError::Io(format!("打开源 dump {} 失败：{}", path.display(), error))
    })?;
    let head_len = len.min(HPROF_SIDECAR_CHECKSUM_BYTES);
    let head_crc32 = crc32_for_reader_slice(&mut file, head_len as usize)?;
    let tail_len = len.min(HPROF_SIDECAR_CHECKSUM_BYTES);
    file.seek(SeekFrom::Start(len.saturating_sub(tail_len)))?;
    let tail_crc32 = crc32_for_reader_slice(&mut file, tail_len as usize)?;
    Ok((head_crc32, tail_crc32))
}

fn crc32_for_reader_slice(file: &mut File, len: usize) -> Result<u32, HprofError> {
    let mut hasher = Crc32Hasher::new();
    let mut remaining = len;
    let mut buffer = vec![0u8; 64 * 1024];
    while remaining > 0 {
        let read_len = remaining.min(buffer.len());
        let n = file.read(&mut buffer[..read_len])?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        remaining -= n;
    }
    Ok(hasher.finalize())
}

/// 创建带大缓冲的 sidecar 文件写入器。
///
/// 业务意图：
/// - sidecar 主体文件属于顺序写入场景，使用 32MiB 缓冲可减少大 dump 末尾写索引阶段的小系统调用。
fn create_sidecar_writer(path: &Path) -> Result<BufWriter<File>, HprofError> {
    let file = File::create(path).map_err(|error| {
        HprofError::Io(format!(
            "创建 sidecar 文件 {} 失败：{}",
            path.display(),
            error
        ))
    })?;
    Ok(BufWriter::with_capacity(
        HPROF_SIDECAR_WRITER_BUFFER_BYTES,
        file,
    ))
}

/// 创建带大缓冲的 sidecar 文件读取器。
///
/// 业务意图：
/// - 缓存命中路径会按字段顺序读取数百万条对象摘要；如果直接对 `File` 做小块 `read_exact`，第二次打开仍会很慢。
///   使用大缓冲读取可以把大量小读合并成顺序读，明显降低系统调用开销。
fn create_sidecar_reader(path: &Path) -> Result<(BufReader<File>, u64), HprofError> {
    let file = File::open(path).map_err(|error| {
        HprofError::Io(format!(
            "打开 sidecar 文件 {} 失败：{}",
            path.display(),
            error
        ))
    })?;
    let file_len = file.metadata().map_err(|error| {
        HprofError::Io(format!(
            "读取 sidecar 文件元数据 {} 失败：{}",
            path.display(),
            error
        ))
    })?;
    Ok((
        BufReader::with_capacity(HPROF_SIDECAR_WRITER_BUFFER_BYTES, file),
        file_len.len(),
    ))
}

/// 刷新 sidecar 文件写入器。
fn finish_sidecar_writer(mut writer: BufWriter<File>) -> Result<(), HprofError> {
    writer.flush()?;
    Ok(())
}

/// 写入已编码的 sidecar chunk。
///
/// 边界条件：
/// - chunk 复用同一个 `Vec<u8>`，写完后清空但保留容量，避免数百万对象时反复分配。
fn flush_sidecar_chunk<W: Write>(writer: &mut W, chunk: &mut Vec<u8>) -> Result<(), HprofError> {
    if !chunk.is_empty() {
        writer.write_all(chunk)?;
        chunk.clear();
    }
    Ok(())
}

/// 报告 sidecar 文件内 chunk 处理进度。
///
/// 业务意图：
/// - sidecar 首次生成和二次打开缓存命中都会按 chunk 处理同一批文件；进度文案必须区分“写入”和“读取”，
///   否则用户在加载缓存时会看到“写入 dominator”这种误导性状态。
fn report_sidecar_chunk<F>(
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    action: &str,
    file_name: &str,
    done: usize,
    total: usize,
    unit: &'static str,
) -> Result<(), HprofError>
where
    F: FnMut(HprofProgress),
{
    progress.phase_done = done as u64;
    progress.phase_total = total as u64;
    progress.phase_unit = unit;
    progress.sub_message = format!("{action} {file_name} {done} / {total}");
    progress_reporter(progress.clone());
    Ok(())
}

/// 报告 sidecar 文件内 chunk 写入进度。
fn report_cache_write_chunk<F>(
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    file_name: &str,
    done: usize,
    total: usize,
    unit: &'static str,
) -> Result<(), HprofError>
where
    F: FnMut(HprofProgress),
{
    report_sidecar_chunk(
        progress,
        progress_reporter,
        "写入",
        file_name,
        done,
        total,
        unit,
    )
}

/// 报告 sidecar 文件内 chunk 读取进度。
fn report_cache_read_chunk<F>(
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    file_name: &str,
    done: usize,
    total: usize,
    unit: &'static str,
) -> Result<(), HprofError>
where
    F: FnMut(HprofProgress),
{
    report_sidecar_chunk(
        progress,
        progress_reporter,
        "读取",
        file_name,
        done,
        total,
        unit,
    )
}

/// 校验 sidecar 里的长度字段不会触发异常大分配。
///
/// 边界条件：
/// - sidecar 是过程文件，可能因为崩溃、手工修改或旧版本残留而损坏；任何来自磁盘的 count
///   都必须先和 manifest / 文件大小上限比对，再用于 `Vec::with_capacity`。
fn validate_sidecar_count(label: &str, count: usize, max_count: usize) -> Result<(), HprofError> {
    if count > max_count {
        return Err(HprofError::InvalidFormat(format!(
            "sidecar {label} 数量异常：{count}，上限 {max_count}"
        )));
    }
    Ok(())
}

/// 校验 sidecar 里的长度字段必须等于预期值。
fn validate_sidecar_count_eq(label: &str, count: usize, expected: usize) -> Result<(), HprofError> {
    if count != expected {
        return Err(HprofError::InvalidFormat(format!(
            "sidecar {label} 数量不匹配：{count}，预期 {expected}"
        )));
    }
    Ok(())
}

/// 按文件大小估算最多能容纳多少条 8 字节长度记录。
fn max_u64_records_for_file(file_len: u64) -> usize {
    usize::try_from(file_len / 8).unwrap_or(usize::MAX)
}

/// 把“读取完一个 sidecar 文件”计入字节进度。
fn finish_cache_file_progress<F>(
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    file_name: &str,
    file_len: u64,
) -> Result<(), HprofError>
where
    F: FnMut(HprofProgress),
{
    progress.bytes_read = progress.bytes_read.saturating_add(file_len);
    progress.phase_done = progress.phase_total;
    progress.sub_message = format!("读取 {file_name} 完成");
    progress_reporter(progress.clone());
    Ok(())
}

fn write_objects_file<F>(
    path: &Path,
    result: &HprofDominatorResult,
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    cancel_flag: &AtomicBool,
) -> Result<(), HprofError>
where
    F: FnMut(HprofProgress),
{
    let summary_count = result.summary_count();
    let mut writer = create_sidecar_writer(path)?;
    write_magic(&mut writer, MAGIC_OBJECTS)?;
    write_len(&mut writer, summary_count)?;
    report_cache_write_chunk(
        progress,
        progress_reporter,
        OBJECTS_FILE,
        0,
        summary_count,
        "对象",
    )?;
    let mut chunk = Vec::with_capacity(HPROF_SIDECAR_RECORD_CHUNK * 48);
    for chunk_start in (0..summary_count).step_by(HPROF_SIDECAR_RECORD_CHUNK) {
        check_cancel(cancel_flag)?;
        let chunk_end = (chunk_start + HPROF_SIDECAR_RECORD_CHUNK).min(summary_count);
        for summary_index in chunk_start..chunk_end {
            let Some(summary) = result.summary_at_index(summary_index) else {
                return Err(HprofError::InvalidFormat(format!(
                    "写入 sidecar 时缺少对象摘要：{summary_index}"
                )));
            };
            write_u64(&mut chunk, summary.object_id)?;
            write_u64(&mut chunk, summary.class_id)?;
            write_kind(&mut chunk, &summary.kind)?;
            write_u64(&mut chunk, summary.shallow_size)?;
            write_u64(&mut chunk, summary.retained_size)?;
            write_u32(&mut chunk, summary.retained_percent.to_bits())?;
            write_len(&mut chunk, summary.direct_child_count)?;
        }
        flush_sidecar_chunk(&mut writer, &mut chunk)?;
        report_cache_write_chunk(
            progress,
            progress_reporter,
            OBJECTS_FILE,
            chunk_end,
            summary_count,
            "对象",
        )?;
    }
    finish_sidecar_writer(writer)
}

/// 打开对象摘要懒加载存储。
fn open_object_summary_storage<F>(
    path: &Path,
    expected_count: usize,
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
) -> Result<HprofSummaryStorage, HprofError>
where
    F: FnMut(HprofProgress),
{
    let (mut file, file_len) = create_sidecar_reader(path)?;
    read_magic(&mut file, MAGIC_OBJECTS)?;
    let count = read_len(&mut file)?;
    validate_sidecar_count_eq("对象摘要", count, expected_count)?;
    validate_object_summary_file_len(count, file_len)?;
    progress.phase_done = 1;
    progress.phase_total = 1;
    progress.phase_unit = "文件";
    progress.sub_message = "建立对象摘要懒加载读取器".to_string();
    progress_reporter(progress.clone());
    Ok(HprofSummaryStorage::Sidecar {
        count,
        reader: std::cell::RefCell::new(file),
        cache: std::cell::RefCell::new(FxHashMap::default()),
        #[cfg(test)]
        object_index_cache: std::cell::RefCell::new(FxHashMap::default()),
    })
}

/// 从已打开的 `objects.bin` 随机读取单条对象摘要。
///
/// 业务意图：
/// - 缓存命中时 dominator tree 首屏只需要少量 top row；按固定记录长度 seek 可以避免全量读取数百万对象摘要。
/// - 调用方复用同一个 `BufReader<File>`，避免 UI 重绘时为每个可见行重复打开 sidecar 文件。
pub(super) fn read_object_summary_from_reader<R: Read + Seek>(
    reader: &mut R,
    summary_index: usize,
    expected_count: usize,
) -> Result<HprofDominatorObjectSummary, HprofError> {
    if summary_index >= expected_count {
        return Err(HprofError::InvalidFormat(format!(
            "sidecar 对象摘要下标越界：{summary_index} / {expected_count}"
        )));
    }
    let header_len = object_summary_data_offset();
    let offset = header_len
        .checked_add((summary_index as u64).saturating_mul(OBJECT_SUMMARY_RECORD_BYTES))
        .ok_or_else(|| HprofError::InvalidFormat("sidecar 对象摘要偏移溢出".to_string()))?;
    reader.seek(SeekFrom::Start(offset))?;
    read_object_summary_record(reader)
}

/// 校验 `objects.bin` 是否足以容纳固定长度摘要记录。
fn validate_object_summary_file_len(count: usize, file_len: u64) -> Result<(), HprofError> {
    let expected_len = object_summary_data_offset()
        .checked_add((count as u64).saturating_mul(OBJECT_SUMMARY_RECORD_BYTES))
        .ok_or_else(|| HprofError::InvalidFormat("sidecar 对象摘要文件长度溢出".to_string()))?;
    if file_len < expected_len {
        return Err(HprofError::InvalidFormat(format!(
            "sidecar 对象摘要文件截断：{} < {}",
            file_len, expected_len
        )));
    }
    Ok(())
}

/// 返回 `objects.bin` 固定摘要记录起始偏移。
fn object_summary_data_offset() -> u64 {
    8 + MAGIC_OBJECTS.len() as u64 + 8
}

/// 读取一条固定长度对象摘要记录。
fn read_object_summary_record<R: Read>(
    reader: &mut R,
) -> Result<HprofDominatorObjectSummary, HprofError> {
    Ok(HprofDominatorObjectSummary {
        object_id: read_u64(reader)?,
        class_id: read_u64(reader)?,
        kind: read_kind(reader)?,
        shallow_size: read_u64(reader)?,
        retained_size: read_u64(reader)?,
        retained_percent: f32::from_bits(read_u32(reader)?),
        direct_child_count: read_len(reader)?,
    })
}

fn write_dominator_file<F>(
    path: &Path,
    result: &HprofDominatorResult,
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    cancel_flag: &AtomicBool,
) -> Result<(), HprofError>
where
    F: FnMut(HprofProgress),
{
    let mut writer = create_sidecar_writer(path)?;
    write_magic(&mut writer, MAGIC_DOMINATOR)?;
    write_len(&mut writer, result.total_objects)?;
    write_len(&mut writer, result.total_classes)?;
    write_len(&mut writer, result.gc_root_count)?;
    write_len(&mut writer, result.edge_count)?;
    write_len(&mut writer, result.raw_edge_count)?;
    write_len(&mut writer, result.reference_edge_stats.weak_like)?;
    write_len(&mut writer, result.reference_edge_stats.soft_like)?;
    write_len(&mut writer, result.reference_edge_stats.phantom_like)?;
    write_len(&mut writer, result.reference_edge_stats.finalizer_like)?;
    write_len(&mut writer, result.synthetic_class_loader_edge_count)?;
    write_len(&mut writer, result.synthetic_bootstrap_class_root_count)?;
    write_u64(&mut writer, result.total_shallow_size)?;
    write_u64(&mut writer, result.reachable_shallow_size)?;
    write_len(&mut writer, result.reachable_object_count)?;
    write_len(&mut writer, result.unreachable_object_count)?;
    write_u64(&mut writer, result.unreachable_shallow_size)?;
    write_len(&mut writer, result.top_object_ids.len())?;

    let total_records = 1usize
        .saturating_add(result.top_object_ids.len())
        .saturating_add(result.top_summary_indices.len())
        .saturating_add(result.child_offsets.len())
        .saturating_add(result.child_summary_indices.len());
    let mut done = 1usize;
    report_cache_write_chunk(
        progress,
        progress_reporter,
        DOMINATOR_FILE,
        done,
        total_records,
        "条",
    )?;

    let mut chunk = Vec::with_capacity(HPROF_SIDECAR_RECORD_CHUNK * 8);
    for object_ids in result.top_object_ids.chunks(HPROF_SIDECAR_RECORD_CHUNK) {
        check_cancel(cancel_flag)?;
        for object_id in object_ids {
            write_u64(&mut chunk, *object_id)?;
        }
        flush_sidecar_chunk(&mut writer, &mut chunk)?;
        done = done.saturating_add(object_ids.len());
        report_cache_write_chunk(
            progress,
            progress_reporter,
            DOMINATOR_FILE,
            done,
            total_records,
            "条",
        )?;
    }

    write_len(&mut writer, result.top_summary_indices.len())?;
    for indices in result
        .top_summary_indices
        .chunks(HPROF_SIDECAR_RECORD_CHUNK)
    {
        check_cancel(cancel_flag)?;
        for index in indices {
            write_len(&mut chunk, *index)?;
        }
        flush_sidecar_chunk(&mut writer, &mut chunk)?;
        done = done.saturating_add(indices.len());
        report_cache_write_chunk(
            progress,
            progress_reporter,
            DOMINATOR_FILE,
            done,
            total_records,
            "条",
        )?;
    }

    write_len(&mut writer, result.child_offsets.len())?;
    for offsets in result.child_offsets.chunks(HPROF_SIDECAR_RECORD_CHUNK) {
        check_cancel(cancel_flag)?;
        for offset in offsets {
            write_len(&mut chunk, *offset)?;
        }
        flush_sidecar_chunk(&mut writer, &mut chunk)?;
        done = done.saturating_add(offsets.len());
        report_cache_write_chunk(
            progress,
            progress_reporter,
            DOMINATOR_FILE,
            done,
            total_records,
            "条",
        )?;
    }

    write_len(&mut writer, result.child_summary_indices.len())?;
    for indices in result
        .child_summary_indices
        .chunks(HPROF_SIDECAR_RECORD_CHUNK)
    {
        check_cancel(cancel_flag)?;
        for index in indices {
            write_len(&mut chunk, *index)?;
        }
        flush_sidecar_chunk(&mut writer, &mut chunk)?;
        done = done.saturating_add(indices.len());
        report_cache_write_chunk(
            progress,
            progress_reporter,
            DOMINATOR_FILE,
            done,
            total_records,
            "条",
        )?;
    }
    finish_sidecar_writer(writer)
}

fn read_dominator_file<F>(
    path: &Path,
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    cancel_flag: &AtomicBool,
) -> Result<CachedDominatorData, HprofError>
where
    F: FnMut(HprofProgress),
{
    let (mut file, file_len) = create_sidecar_reader(path)?;
    progress.phase_done = 0;
    progress.phase_total = 1;
    progress.phase_unit = "文件";
    progress.sub_message = "读取 dominator 元数据".to_string();
    progress_reporter(progress.clone());
    read_magic(&mut file, MAGIC_DOMINATOR)?;
    let total_objects = read_len(&mut file)?;
    let total_classes = read_len(&mut file)?;
    let gc_root_count = read_len(&mut file)?;
    let edge_count = read_len(&mut file)?;
    let raw_edge_count = read_len(&mut file)?;
    let reference_edge_stats = HprofReferenceEdgeStats {
        weak_like: read_len(&mut file)?,
        soft_like: read_len(&mut file)?,
        phantom_like: read_len(&mut file)?,
        finalizer_like: read_len(&mut file)?,
    };
    let synthetic_class_loader_edge_count = read_len(&mut file)?;
    let synthetic_bootstrap_class_root_count = read_len(&mut file)?;
    let total_shallow_size = read_u64(&mut file)?;
    let reachable_shallow_size = read_u64(&mut file)?;
    let reachable_object_count = read_len(&mut file)?;
    let unreachable_object_count = read_len(&mut file)?;
    let unreachable_shallow_size = read_u64(&mut file)?;
    // 缓存命中时不会重新扫描源 HPROF，因此摘要计数必须尽早从 dominator 文件恢复。
    // 这样用户在大 sidecar 读取期间也能看到对象、类、Root、边数，而不是长时间保持 0。
    progress.object_count = total_objects;
    progress.class_count = total_classes;
    progress.gc_root_count = gc_root_count;
    progress.edge_count = edge_count;
    progress_reporter(progress.clone());
    validate_sidecar_count("可达对象", reachable_object_count, total_objects)?;
    validate_sidecar_count(
        "不可达对象",
        unreachable_object_count,
        total_objects.saturating_sub(reachable_object_count),
    )?;
    let max_records_by_file = max_u64_records_for_file(file_len);
    let top_count = read_len(&mut file)?;
    validate_sidecar_count(
        "Top retained",
        top_count,
        HPROF_TOP_DOMINATOR_LIMIT.min(max_records_by_file),
    )?;
    let mut top_object_ids = Vec::with_capacity(top_count);
    for index in 0..top_count {
        check_cancel(cancel_flag)?;
        top_object_ids.push(read_u64(&mut file)?);
        let done = index + 1;
        if should_report_work(done, top_count) {
            report_cache_read_chunk(
                progress,
                progress_reporter,
                DOMINATOR_FILE,
                done,
                top_count,
                "Top",
            )?;
        }
    }
    let top_summary_count = read_len(&mut file)?;
    validate_sidecar_count_eq("Top summary indices", top_summary_count, top_count)?;
    validate_sidecar_count(
        "Top summary indices",
        top_summary_count,
        reachable_object_count,
    )?;
    let mut top_summary_indices = Vec::with_capacity(top_summary_count);
    for index in 0..top_summary_count {
        check_cancel(cancel_flag)?;
        let summary_index = read_len(&mut file)?;
        validate_sidecar_count(
            "Top summary index",
            summary_index,
            reachable_object_count.saturating_sub(1),
        )?;
        top_summary_indices.push(summary_index);
        let done = index + 1;
        if should_report_work(done, top_summary_count) {
            report_cache_read_chunk(
                progress,
                progress_reporter,
                DOMINATOR_FILE,
                done,
                top_summary_count,
                "Top",
            )?;
        }
    }
    let offset_count = read_len(&mut file)?;
    let expected_offsets = reachable_object_count
        .checked_add(1)
        .ok_or_else(|| HprofError::InvalidFormat("sidecar child offsets 数量溢出".to_string()))?;
    validate_sidecar_count_eq("child offsets", offset_count, expected_offsets)?;
    validate_sidecar_count("child offsets", offset_count, max_records_by_file)?;
    let mut child_offsets = Vec::with_capacity(offset_count);
    for index in 0..offset_count {
        check_cancel(cancel_flag)?;
        child_offsets.push(read_len(&mut file)?);
        let done = index + 1;
        if should_report_work(done, offset_count) {
            report_cache_read_chunk(
                progress,
                progress_reporter,
                DOMINATOR_FILE,
                done,
                offset_count,
                "offset",
            )?;
        }
    }
    let child_count = read_len(&mut file)?;
    validate_sidecar_count("child summary indices", child_count, reachable_object_count)?;
    validate_sidecar_count("child summary indices", child_count, max_records_by_file)?;
    let mut previous_offset = 0usize;
    for offset in &child_offsets {
        if *offset < previous_offset || *offset > child_count {
            return Err(HprofError::InvalidFormat(format!(
                "sidecar child offsets 内容损坏：offset={offset}, child_count={child_count}"
            )));
        }
        previous_offset = *offset;
    }
    let mut child_summary_indices = Vec::with_capacity(child_count);
    for index in 0..child_count {
        check_cancel(cancel_flag)?;
        let child_summary_index = read_len(&mut file)?;
        validate_sidecar_count(
            "child summary index",
            child_summary_index,
            reachable_object_count.saturating_sub(1),
        )?;
        child_summary_indices.push(child_summary_index);
        let done = index + 1;
        if should_report_work(done, child_count) {
            report_cache_read_chunk(
                progress,
                progress_reporter,
                DOMINATOR_FILE,
                done,
                child_count,
                "child",
            )?;
        }
    }
    finish_cache_file_progress(progress, progress_reporter, DOMINATOR_FILE, file_len)?;
    Ok(CachedDominatorData {
        total_objects,
        total_classes,
        gc_root_count,
        edge_count,
        raw_edge_count,
        reference_edge_stats,
        synthetic_class_loader_edge_count,
        synthetic_bootstrap_class_root_count,
        total_shallow_size,
        reachable_shallow_size,
        reachable_object_count,
        unreachable_object_count,
        unreachable_shallow_size,
        top_object_ids,
        top_summary_indices,
        child_offsets,
        child_summary_indices,
    })
}

struct CachedDominatorData {
    total_objects: usize,
    total_classes: usize,
    gc_root_count: usize,
    edge_count: usize,
    raw_edge_count: usize,
    reference_edge_stats: HprofReferenceEdgeStats,
    synthetic_class_loader_edge_count: usize,
    synthetic_bootstrap_class_root_count: usize,
    total_shallow_size: u64,
    reachable_shallow_size: u64,
    reachable_object_count: usize,
    unreachable_object_count: usize,
    unreachable_shallow_size: u64,
    top_object_ids: Vec<HprofObjectId>,
    top_summary_indices: Vec<usize>,
    child_offsets: Vec<usize>,
    child_summary_indices: Vec<usize>,
}

fn write_classes_file<F>(
    path: &Path,
    result: &HprofDominatorResult,
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    cancel_flag: &AtomicBool,
) -> Result<(), HprofError>
where
    F: FnMut(HprofProgress),
{
    let mut writer = create_sidecar_writer(path)?;
    write_magic(&mut writer, MAGIC_CLASSES)?;
    write_len(&mut writer, result.class_names.len())?;
    report_cache_write_chunk(
        progress,
        progress_reporter,
        CLASSES_FILE,
        0,
        result.class_names.len(),
        "类",
    )?;

    let mut chunk = Vec::with_capacity(HPROF_SIDECAR_RECORD_CHUNK * 64);
    let mut done = 0usize;
    for (class_id, name) in &result.class_names {
        write_u64(&mut chunk, *class_id)?;
        write_string(&mut chunk, name)?;
        done = done.saturating_add(1);
        if done.is_multiple_of(HPROF_SIDECAR_RECORD_CHUNK) {
            check_cancel(cancel_flag)?;
            flush_sidecar_chunk(&mut writer, &mut chunk)?;
            report_cache_write_chunk(
                progress,
                progress_reporter,
                CLASSES_FILE,
                done,
                result.class_names.len(),
                "类",
            )?;
        }
    }
    flush_sidecar_chunk(&mut writer, &mut chunk)?;
    report_cache_write_chunk(
        progress,
        progress_reporter,
        CLASSES_FILE,
        done,
        result.class_names.len(),
        "类",
    )?;
    finish_sidecar_writer(writer)
}

fn read_classes_file<F>(
    path: &Path,
    max_class_count: usize,
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    cancel_flag: &AtomicBool,
) -> Result<FxHashMap<HprofObjectId, String>, HprofError>
where
    F: FnMut(HprofProgress),
{
    let (mut file, file_len) = create_sidecar_reader(path)?;
    read_magic(&mut file, MAGIC_CLASSES)?;
    let count = read_len(&mut file)?;
    validate_sidecar_count("类名", count, max_class_count)?;
    validate_sidecar_count(
        "类名",
        count,
        usize::try_from(file_len / 16).unwrap_or(usize::MAX),
    )?;
    progress.phase_done = 0;
    progress.phase_total = count as u64;
    progress.phase_unit = "类";
    progress.sub_message = "读取类名索引".to_string();
    progress_reporter(progress.clone());
    let mut class_names = FxHashMap::default();
    class_names.reserve(count);
    for index in 0..count {
        check_cancel(cancel_flag)?;
        class_names.insert(read_u64(&mut file)?, read_string(&mut file)?);
        let done = index + 1;
        if should_report_work(done, count) {
            report_cache_read_chunk(progress, progress_reporter, CLASSES_FILE, done, count, "类")?;
        }
    }
    finish_cache_file_progress(progress, progress_reporter, CLASSES_FILE, file_len)?;
    Ok(class_names)
}

fn write_threads_file<F>(
    path: &Path,
    result: &HprofDominatorResult,
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    cancel_flag: &AtomicBool,
) -> Result<(), HprofError>
where
    F: FnMut(HprofProgress),
{
    let mut writer = create_sidecar_writer(path)?;
    write_magic(&mut writer, MAGIC_THREADS)?;
    write_len(&mut writer, result.thread_details.len())?;
    report_cache_write_chunk(
        progress,
        progress_reporter,
        THREADS_FILE,
        0,
        result.thread_details.len(),
        "线程",
    )?;

    let mut chunk = Vec::with_capacity(HPROF_SIDECAR_RECORD_CHUNK * 256);
    let mut done = 0usize;
    for (object_id, details) in &result.thread_details {
        write_u64(&mut chunk, *object_id)?;
        write_u64(&mut chunk, details.object_id)?;
        write_string(&mut chunk, &details.class_name)?;
        write_string(&mut chunk, &details.thread_name)?;
        write_len(&mut chunk, details.properties.len())?;
        for property in &details.properties {
            write_string(&mut chunk, &property.name)?;
            write_string(&mut chunk, &property.value)?;
        }
        write_len(&mut chunk, details.stack_frames.len())?;
        for frame in &details.stack_frames {
            write_string(&mut chunk, &frame.class_name)?;
            write_string(&mut chunk, &frame.method_name)?;
            write_string(&mut chunk, &frame.method_signature)?;
            write_string(&mut chunk, &frame.source)?;
            write_i32(&mut chunk, frame.line_number)?;
            write_string(&mut chunk, &frame.display)?;
        }
        write_option_string(&mut chunk, details.stack_message.as_deref())?;
        done = done.saturating_add(1);
        if done.is_multiple_of(HPROF_SIDECAR_RECORD_CHUNK)
            || chunk.len() >= HPROF_SIDECAR_WRITER_BUFFER_BYTES
        {
            check_cancel(cancel_flag)?;
            flush_sidecar_chunk(&mut writer, &mut chunk)?;
            report_cache_write_chunk(
                progress,
                progress_reporter,
                THREADS_FILE,
                done,
                result.thread_details.len(),
                "线程",
            )?;
        }
    }
    flush_sidecar_chunk(&mut writer, &mut chunk)?;
    report_cache_write_chunk(
        progress,
        progress_reporter,
        THREADS_FILE,
        done,
        result.thread_details.len(),
        "线程",
    )?;
    finish_sidecar_writer(writer)
}

fn read_threads_file<F>(
    path: &Path,
    max_thread_count: usize,
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    cancel_flag: &AtomicBool,
) -> Result<FxHashMap<HprofObjectId, HprofThreadDetails>, HprofError>
where
    F: FnMut(HprofProgress),
{
    let (mut file, file_len) = create_sidecar_reader(path)?;
    read_magic(&mut file, MAGIC_THREADS)?;
    let count = read_len(&mut file)?;
    validate_sidecar_count("线程详情", count, max_thread_count)?;
    validate_sidecar_count(
        "线程详情",
        count,
        usize::try_from(file_len / 32).unwrap_or(usize::MAX),
    )?;
    progress.phase_done = 0;
    progress.phase_total = count as u64;
    progress.phase_unit = "线程";
    progress.sub_message = "读取线程详情".to_string();
    progress_reporter(progress.clone());
    let mut thread_details = FxHashMap::default();
    thread_details.reserve(count);
    let max_records_by_file = max_u64_records_for_file(file_len);
    for index in 0..count {
        check_cancel(cancel_flag)?;
        let key = read_u64(&mut file)?;
        let object_id = read_u64(&mut file)?;
        let class_name = read_string(&mut file)?;
        let thread_name = read_string(&mut file)?;
        let property_count = read_len(&mut file)?;
        validate_sidecar_count("线程属性", property_count, 128)?;
        let mut properties = Vec::with_capacity(property_count);
        for _ in 0..property_count {
            properties.push(HprofThreadProperty {
                name: read_string(&mut file)?,
                value: read_string(&mut file)?,
            });
        }
        let frame_count = read_len(&mut file)?;
        validate_sidecar_count("线程栈帧", frame_count, max_records_by_file)?;
        let mut stack_frames = Vec::with_capacity(frame_count);
        for _ in 0..frame_count {
            stack_frames.push(HprofThreadStackFrame {
                class_name: read_string(&mut file)?,
                method_name: read_string(&mut file)?,
                method_signature: read_string(&mut file)?,
                source: read_string(&mut file)?,
                line_number: read_i32(&mut file)?,
                display: read_string(&mut file)?,
            });
        }
        let stack_message = read_option_string(&mut file)?;
        thread_details.insert(
            key,
            HprofThreadDetails {
                object_id,
                class_name,
                thread_name,
                properties,
                stack_frames,
                stack_message,
            },
        );
        let done = index + 1;
        if should_report_work(done, count) {
            report_cache_read_chunk(
                progress,
                progress_reporter,
                THREADS_FILE,
                done,
                count,
                "线程",
            )?;
        }
    }
    finish_cache_file_progress(progress, progress_reporter, THREADS_FILE, file_len)?;
    Ok(thread_details)
}

fn read_cached_result<F>(
    path: &Path,
    sidecar_dir: &Path,
    manifest: &HprofSidecarManifest,
    progress: &mut HprofProgress,
    progress_reporter: &mut F,
    cancel_flag: &AtomicBool,
) -> Result<HprofDominatorResult, HprofError>
where
    F: FnMut(HprofProgress),
{
    let dominator_path = sidecar_dir.join(DOMINATOR_FILE);
    let dominator = read_dominator_file(&dominator_path, progress, progress_reporter, cancel_flag)?;
    let objects_path = sidecar_dir.join(OBJECTS_FILE);
    let summary_storage = open_object_summary_storage(
        &objects_path,
        dominator.reachable_object_count,
        progress,
        progress_reporter,
    )?;
    check_cancel(cancel_flag)?;
    let classes_path = sidecar_dir.join(CLASSES_FILE);
    let class_names = read_classes_file(
        &classes_path,
        dominator.total_classes,
        progress,
        progress_reporter,
        cancel_flag,
    )?;
    let threads_path = sidecar_dir.join(THREADS_FILE);
    let thread_details = read_threads_file(
        &threads_path,
        dominator.reachable_object_count,
        progress,
        progress_reporter,
        cancel_flag,
    )?;
    Ok(HprofDominatorResult {
        file_path: path.to_path_buf(),
        header: manifest.header.clone(),
        total_objects: dominator.total_objects,
        total_classes: dominator.total_classes,
        gc_root_count: dominator.gc_root_count,
        edge_count: dominator.edge_count,
        raw_edge_count: dominator.raw_edge_count,
        reference_edge_stats: dominator.reference_edge_stats,
        synthetic_class_loader_edge_count: dominator.synthetic_class_loader_edge_count,
        synthetic_bootstrap_class_root_count: dominator.synthetic_bootstrap_class_root_count,
        total_shallow_size: dominator.total_shallow_size,
        reachable_shallow_size: dominator.reachable_shallow_size,
        reachable_object_count: dominator.reachable_object_count,
        unreachable_object_count: dominator.unreachable_object_count,
        unreachable_shallow_size: dominator.unreachable_shallow_size,
        size_model: manifest.size_model.clone(),
        cache_status: None,
        top_object_ids: dominator.top_object_ids,
        top_summary_indices: dominator.top_summary_indices,
        summary_storage,
        class_names,
        thread_details,
        child_offsets: dominator.child_offsets,
        child_summary_indices: dominator.child_summary_indices,
    })
}

fn write_empty_file(path: &Path) -> Result<(), HprofError> {
    let mut writer = create_sidecar_writer(path)?;
    write_magic(&mut writer, MAGIC_EMPTY)?;
    write_u64(&mut writer, 0)?;
    finish_sidecar_writer(writer)
}

fn write_magic<W: Write>(writer: &mut W, magic: &[u8]) -> Result<(), HprofError> {
    write_len(writer, magic.len())?;
    writer.write_all(magic)?;
    Ok(())
}

fn read_magic<R: Read>(reader: &mut R, expected: &[u8]) -> Result<(), HprofError> {
    let len = read_len(reader)?;
    validate_sidecar_count("magic", len, MAX_MAGIC_BYTES)?;
    let mut magic = vec![0u8; len];
    reader.read_exact(&mut magic)?;
    if magic == expected {
        Ok(())
    } else {
        Err(HprofError::InvalidFormat(
            "sidecar 二进制文件 magic 不匹配".to_string(),
        ))
    }
}

fn write_kind<W: Write>(writer: &mut W, kind: &HprofObjectKind) -> Result<(), HprofError> {
    match kind {
        HprofObjectKind::Class => {
            write_u8(writer, 0)?;
            write_u32(writer, 0)?;
            write_u8(writer, 0)
        }
        HprofObjectKind::Instance => {
            write_u8(writer, 1)?;
            write_u32(writer, 0)?;
            write_u8(writer, 0)
        }
        HprofObjectKind::ObjectArray { length } => {
            write_u8(writer, 2)?;
            write_u32(writer, *length)?;
            write_u8(writer, 0)
        }
        HprofObjectKind::PrimitiveArray {
            element_type,
            length,
        } => {
            write_u8(writer, 3)?;
            write_u32(writer, *length)?;
            write_u8(writer, *element_type)
        }
    }
}

fn read_kind<R: Read>(reader: &mut R) -> Result<HprofObjectKind, HprofError> {
    let tag = read_u8(reader)?;
    let length = read_u32(reader)?;
    let element_type = read_u8(reader)?;
    match tag {
        0 => Ok(HprofObjectKind::Class),
        1 => Ok(HprofObjectKind::Instance),
        2 => Ok(HprofObjectKind::ObjectArray { length }),
        3 => Ok(HprofObjectKind::PrimitiveArray {
            element_type,
            length,
        }),
        tag => Err(HprofError::InvalidFormat(format!(
            "未知 sidecar 对象类型：{tag}"
        ))),
    }
}

fn write_string<W: Write>(writer: &mut W, value: &str) -> Result<(), HprofError> {
    write_len(writer, value.len())?;
    writer.write_all(value.as_bytes())?;
    Ok(())
}

fn read_string<R: Read>(reader: &mut R) -> Result<String, HprofError> {
    let len = read_len(reader)?;
    validate_sidecar_count("字符串字节", len, MAX_STRING_BYTES)?;
    let mut bytes = vec![0u8; len];
    reader.read_exact(&mut bytes)?;
    String::from_utf8(bytes)
        .map_err(|_| HprofError::InvalidFormat("sidecar 字符串不是有效 UTF-8".to_string()))
}

fn write_option_string<W: Write>(writer: &mut W, value: Option<&str>) -> Result<(), HprofError> {
    match value {
        Some(value) => {
            write_u8(writer, 1)?;
            write_string(writer, value)
        }
        None => write_u8(writer, 0),
    }
}

fn read_option_string<R: Read>(reader: &mut R) -> Result<Option<String>, HprofError> {
    match read_u8(reader)? {
        0 => Ok(None),
        1 => Ok(Some(read_string(reader)?)),
        tag => Err(HprofError::InvalidFormat(format!(
            "未知 sidecar Option<String> 标记：{tag}"
        ))),
    }
}

fn write_len<W: Write>(writer: &mut W, value: usize) -> Result<(), HprofError> {
    write_u64(writer, value as u64)
}

fn read_len<R: Read>(reader: &mut R) -> Result<usize, HprofError> {
    usize::try_from(read_u64(reader)?)
        .map_err(|_| HprofError::InvalidFormat("sidecar 长度超过当前平台限制".to_string()))
}

fn write_u8<W: Write>(writer: &mut W, value: u8) -> Result<(), HprofError> {
    writer.write_all(&[value])?;
    Ok(())
}

fn read_u8<R: Read>(reader: &mut R) -> Result<u8, HprofError> {
    let mut bytes = [0u8; 1];
    reader.read_exact(&mut bytes)?;
    Ok(bytes[0])
}

fn write_u32<W: Write>(writer: &mut W, value: u32) -> Result<(), HprofError> {
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn read_u32<R: Read>(reader: &mut R) -> Result<u32, HprofError> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn write_i32<W: Write>(writer: &mut W, value: i32) -> Result<(), HprofError> {
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn read_i32<R: Read>(reader: &mut R) -> Result<i32, HprofError> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(i32::from_le_bytes(bytes))
}

fn write_u64<W: Write>(writer: &mut W, value: u64) -> Result<(), HprofError> {
    writer.write_all(&value.to_le_bytes())?;
    Ok(())
}

fn read_u64<R: Read>(reader: &mut R) -> Result<u64, HprofError> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn now_millis() -> u128 {
    system_time_millis(SystemTime::now()).unwrap_or(0)
}

fn system_time_millis(time: SystemTime) -> Option<u128> {
    time.duration_since(UNIX_EPOCH).ok().map(|duration| {
        u128::from(duration.as_secs()) * 1000 + u128::from(duration.subsec_millis())
    })
}
