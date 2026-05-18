//! GZIP 单文件成员读取。
//!
//! 业务意图：
//! - 从压缩包成员读取父模块拆出具体格式 reader，避免 ZIP/RAR/TAR/GZIP/7Z 的库调用细节堆在同一个文件。
//! - 函数签名、错误文案、读取上限和成员路径安全校验保持原样，父模块仍负责统一格式分发。

use super::*;

/// 从 GZIP 文件中返回唯一虚拟成员路径。
///
/// 业务意图：
/// - `.gz` 只包含一个压缩后的日志流，没有真实目录；为了复用压缩包成员读取、tab 去重和分页物化，需要生成稳定虚拟成员路径。
/// - 这里先探测 gzip 层可读性，避免把损坏文件伪装成日志文本。
///
/// 边界条件：
/// - 空 gzip 文件可以作为空日志打开；只要解码器没有返回错误，就认为压缩层有效。
pub(super) fn single_file_gzip_member_path(
    archive_path: &Path,
    label: &str,
) -> Result<String, ArchiveReadError> {
    if single_gzip_payload_is_readable_from_path(archive_path) {
        return Ok(single_gzip_member_path_for_archive(archive_path));
    }

    Err(ArchiveReadError::new(format!(
        "无法读取 GZIP 日志 {}：文件不是有效 gzip 内容或已损坏",
        label
    )))
}

/// 从 GZIP 压缩日志中读取唯一虚拟成员。
///
/// 业务意图：
/// - GZIP 不是多文件归档，目录树中的内部文件节点是应用生成的虚拟成员。
/// - 明确校验虚拟成员前缀可以避免调用方把任意路径误当成 gzip 内部文件读取。
pub(super) fn read_gzip_member(
    archive_path: &Path,
    member_path: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    if is_single_gzip_member_path(member_path) {
        return read_single_gzip_payload_from_path(archive_path, member_path);
    }

    Err(ArchiveReadError::new(format!(
        "GZIP 日志中未找到虚拟成员：{}",
        member_path
    )))
}

/// 从 GZIP 压缩日志中把唯一虚拟成员直接写入 writer。
///
/// 业务意图：
/// - `.gz` 日志没有目录结构，插件 SQL 下钻可以让 gzip 解码器直接把解压字节写入内容流。
pub(super) fn stream_gzip_member_to_writer<W: Write + ?Sized>(
    archive_path: &Path,
    member_path: &str,
    writer: &mut W,
) -> Result<(), ArchiveReadError> {
    if is_single_gzip_member_path(member_path) {
        return stream_single_gzip_payload_from_path_to_writer(archive_path, member_path, writer);
    }

    Err(ArchiveReadError::new(format!(
        "GZIP 日志中未找到虚拟成员：{}",
        member_path
    )))
}

/// 从内存 GZIP 字节中读取唯一虚拟成员。
///
/// 业务意图：
/// - 外层压缩包或嵌套压缩包中的 `.gz` 成员会先被读取成字节，再在这里解压成日志正文。
/// - 成员路径必须是加载层生成的虚拟路径，避免把多文件归档成员路径错误套用到单文件 gzip。
pub(super) fn read_gzip_member_from_bytes(
    archive_bytes: &[u8],
    member_path: &str,
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    if is_single_gzip_member_path(member_path) {
        return read_single_gzip_payload_from_bytes(archive_bytes, member_path);
    }

    Err(ArchiveReadError::new(format!(
        "嵌套 GZIP {} 中未找到虚拟成员：{}",
        label, member_path
    )))
}

/// 从内存 GZIP 字节中把唯一虚拟成员直接写入 writer。
pub(super) fn stream_gzip_member_from_bytes_to_writer<W: Write + ?Sized>(
    archive_bytes: &[u8],
    member_path: &str,
    label: &str,
    writer: &mut W,
) -> Result<(), ArchiveReadError> {
    if is_single_gzip_member_path(member_path) {
        return stream_single_gzip_payload_from_bytes_to_writer(archive_bytes, member_path, writer);
    }

    Err(ArchiveReadError::new(format!(
        "嵌套 GZIP {} 中未找到虚拟成员：{}",
        label, member_path
    )))
}

/// 从本地 gzip 文件中按“单个 gzip 日志”读取解压内容。
///
/// 业务意图：
/// - 兼容标准 `.gz` 日志，以及扩展名是 `.tar.gz` 但实际没有 tar 目录、只包含一个 gzip 日志流的文件。
/// - 读取结果仍受 200MB 内存模式上限保护；超过阈值的文件会在分页路径中走物化读取。
pub(super) fn read_single_gzip_payload_from_path(
    archive_path: &Path,
    member_path: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let file = File::open(archive_path).map_err(|error| {
        ArchiveReadError::new(format!(
            "无法打开 GZIP 日志 {}：{}",
            archive_path.display(),
            error
        ))
    })?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let label = single_gzip_member_display_name(member_path);
    read_reader_to_vec_with_limit(decoder, None, &label)
}

/// 从本地 gzip 文件中按“单个 gzip 日志”流式写出解压内容。
pub(super) fn stream_single_gzip_payload_from_path_to_writer<W: Write + ?Sized>(
    archive_path: &Path,
    member_path: &str,
    writer: &mut W,
) -> Result<(), ArchiveReadError> {
    let file = File::open(archive_path).map_err(|error| {
        ArchiveReadError::new(format!(
            "无法打开 GZIP 日志 {}：{}",
            archive_path.display(),
            error
        ))
    })?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let label = single_gzip_member_display_name(member_path);
    copy_reader_to_writer(decoder, writer, &label)
}

/// 从内存字节中按“单个 gzip 日志”读取解压内容。
///
/// 业务意图：
/// - 外层压缩包内可能包含单文件 gzip 日志并使用 `.gz` 或 `.tar.gz` 命名；该路径不能依赖本地文件 seek。
/// - 使用 `Cursor` 保持读取逻辑纯内存、无临时文件副作用。
pub(super) fn read_single_gzip_payload_from_bytes(
    archive_bytes: &[u8],
    label: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let decoder = GzDecoder::new(Cursor::new(archive_bytes));
    let display_label = if is_single_gzip_member_path(label) {
        single_gzip_member_display_name(label)
    } else {
        label.to_string()
    };
    read_reader_to_vec_with_limit(decoder, None, &display_label)
}

/// 从内存字节中按“单个 gzip 日志”流式写出解压内容。
pub(super) fn stream_single_gzip_payload_from_bytes_to_writer<W: Write + ?Sized>(
    archive_bytes: &[u8],
    label: &str,
    writer: &mut W,
) -> Result<(), ArchiveReadError> {
    let decoder = GzDecoder::new(Cursor::new(archive_bytes));
    let display_label = if is_single_gzip_member_path(label) {
        single_gzip_member_display_name(label)
    } else {
        label.to_string()
    };
    copy_reader_to_writer(decoder, writer, &display_label)
}

/// 判断本地 gzip 层是否可以开始解压。
///
/// 业务意图：
/// - 单文件 gzip 只应处理“gzip 有效”的文件，不能把损坏压缩包误判为普通日志。
pub(super) fn single_gzip_payload_is_readable_from_path(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut decoder = GzDecoder::new(BufReader::new(file));
    let mut probe = [0_u8; 1];
    decoder.read(&mut probe).is_ok()
}
