// 连接文件管理后端。
//
// 业务意图：
// - 终端页的“文件管理”需要同时支持本地终端和 SSH 终端，但 UI 只应面对统一的文件命令/事件抽象。
// - 本模块不依赖 GPUI，所有本地文件 IO 与 SSH SFTP IO 都在后台线程执行，避免阻塞主窗口渲染和终端输入。
//
// 跨平台约束：
// - 本地文件路径在 macOS/Windows 上使用 `PathBuf`，只在 UI 文本和事件中转换成展示字符串。
// - SSH 文件路径按远端 POSIX 风格字符串处理，不假设远端与本机路径分隔符一致。
// - 文件预览最多读取 1 MiB + 1 byte；超过上限或疑似二进制时不尝试解码，避免大文件和不可见字节拖垮 UI。

use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use russh::{
    client::{self, Handler},
    keys::{HashAlg, ssh_key},
};
use russh_sftp::{client::SftpSession, protocol::FileType as SftpFileType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroize::Zeroizing;

use crate::log_document::{EncodingChoice, decode_log_bytes};

use super::*;

/// 文件预览最大读取字节数。
///
/// 边界条件：
/// - 读取时会额外读取 1 byte 用于判断是否超过上限，真正展示内容仍限制在 1 MiB 内。
pub(crate) const CONNECTION_FILE_PREVIEW_LIMIT_BYTES: usize = 1024 * 1024;

/// 文件管理后台命令通道句柄。
///
/// 业务意图：
/// - 独立文件管理窗口只持有该句柄，不保存 SSH 密码明文；所有认证信息在后台线程中短生命周期使用。
pub(crate) struct ConnectionFileBackendHandle {
    /// UI 发给文件后端的命令通道。
    pub(crate) command_sender: Sender<ConnectionFileCommand>,
    /// 文件后端发给 UI 的事件通道。
    pub(crate) event_receiver: Receiver<ConnectionFileEvent>,
    /// 当前上传或下载任务的取消标记。
    ///
    /// 业务意图：
    /// - 文件复制期间后台线程正在阻塞式读写同一个命令队列，无法及时处理新的“取消”命令。
    /// - UI 直接设置该原子标记，复制循环每个数据块检查一次，保证大文件传输可以尽快停止。
    cancel_requested: Arc<AtomicBool>,
}

impl ConnectionFileBackendHandle {
    /// 请求取消当前上传或下载任务。
    pub(crate) fn cancel_transfer(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
    }

    /// 请求关闭文件后端。
    pub(crate) fn shutdown(&self) {
        self.cancel_transfer();
        let _ = self.command_sender.send(ConnectionFileCommand::Shutdown);
    }
}

/// 文件管理后端启动目标。
pub(crate) enum ConnectionFileBackendTarget {
    /// 本地文件系统目标。
    Local,
    /// SSH SFTP 目标，密码只在后台认证使用。
    Ssh {
        /// SSH 连接配置快照。
        profile: ConnectionProfile,
        /// 解密后的 SSH 密码；后台线程使用 `Zeroizing` 缩短明文驻留时间。
        password: Zeroizing<String>,
        /// 打开 SFTP 会话时需要复用的可信主机指纹。
        trusted_fingerprint: Option<String>,
    },
}

/// 文件管理后台命令。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionFileCommand {
    /// 列出指定目录。
    List { path: String },
    /// 预览指定普通文件。
    Preview { path: String },
    /// 上传本地普通文件到当前目录。
    Upload(ConnectionFileTransferRequest),
    /// 下载远端/本地普通文件到本地目标目录。
    Download(ConnectionFileTransferRequest),
    /// 删除指定文件或空目录。
    Delete(ConnectionFileDeleteRequest),
    /// 关闭后台线程。
    Shutdown,
}

/// 文件传输请求。
///
/// 业务意图：
/// - 上传和下载共用冲突策略模型，后端发现同名文件时可以把同一个请求带回 UI，用户选择覆盖或跳过后原样重发。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionFileTransferRequest {
    /// 源文件路径；上传时是本机路径字符串，下载时是当前后端路径字符串。
    pub(crate) source_paths: Vec<String>,
    /// 目标目录；上传时是当前后端目录，下载时是本机目录。
    pub(crate) target_dir: String,
    /// 同名冲突策略。
    pub(crate) conflict_policy: ConnectionFileConflictPolicy,
}

/// 文件删除请求。
///
/// 业务意图：
/// - 删除入口来自右键菜单，UI 会先弹窗确认；后端仍根据列表中的文件类型限制能力，避免误删目录树或特殊文件。
/// - 第一版只删除普通文件、符号链接和空目录，非空目录由系统 API 返回错误，后续如需递归删除必须单独确认需求。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionFileDeleteRequest {
    /// 待删除路径；本地为本机路径字符串，SSH 为远端路径字符串。
    pub(crate) path: String,
    /// 用户可见名称，用于确认弹窗和完成提示。
    pub(crate) name: String,
    /// 列表读取时识别出的文件类型。
    pub(crate) kind: ConnectionFileEntryKind,
}

/// 同名文件冲突策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionFileConflictPolicy {
    /// 首次执行时遇到冲突要回到 UI 确认。
    Ask,
    /// 覆盖目标文件。
    Overwrite,
    /// 跳过已存在文件。
    Skip,
}

/// 文件管理后台事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionFileEvent {
    /// 目录列表刷新完成。
    Listed {
        /// 实际列出的目录路径。
        path: String,
        /// 排序后的目录项。
        entries: Vec<ConnectionFileEntry>,
    },
    /// 文件预览完成。
    Previewed(ConnectionFilePreview),
    /// 上传或下载过程中的进度快照。
    TransferProgress(ConnectionFileTransferProgress),
    /// 上传或下载完成后的简短中文提示。
    OperationFinished(String),
    /// 需要用户确认同名文件冲突。
    Conflict(ConnectionFileConflict),
    /// 用户可见错误；不得包含 SSH 密码或密文。
    Error(String),
}

/// 文件传输方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionFileTransferOperation {
    /// 上传到当前文件后端。
    Upload,
    /// 下载到本地目录。
    Download,
}

impl ConnectionFileTransferOperation {
    /// 返回用户可见动词。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Upload => "上传",
            Self::Download => "下载",
        }
    }
}

/// 文件传输进度快照。
///
/// 业务意图：
/// - 后端复制大文件时持续上报已传输字节和耗时，UI 据此显示进度条和速度。
/// - 字节总数只统计普通文件；目录、不可读取文件和被跳过的同名文件不计入总量，避免进度超过 100%。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionFileTransferProgress {
    /// 上传或下载。
    pub(crate) operation: ConnectionFileTransferOperation,
    /// 已成功传输的普通文件数量。
    pub(crate) completed_files: usize,
    /// 本次需要处理的普通文件数量。
    pub(crate) total_files: usize,
    /// 已传输字节数。
    pub(crate) transferred_bytes: u64,
    /// 总字节数。
    pub(crate) total_bytes: u64,
    /// 从本次传输开始计算的耗时毫秒。
    pub(crate) elapsed_ms: u64,
}

impl ConnectionFileTransferProgress {
    /// 返回 0.0 到 1.0 的进度比例。
    pub(crate) fn ratio(&self) -> f32 {
        if self.total_bytes == 0 {
            return 1.0;
        }
        (self.transferred_bytes as f32 / self.total_bytes as f32).clamp(0.0, 1.0)
    }

    /// 返回每秒传输字节数。
    pub(crate) fn bytes_per_second(&self) -> f64 {
        if self.elapsed_ms == 0 {
            return 0.0;
        }
        self.transferred_bytes as f64 * 1000.0 / self.elapsed_ms as f64
    }
}

/// 文件列表中的单项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionFileEntry {
    /// 文件或目录名。
    pub(crate) name: String,
    /// 完整路径；本地为本机路径字符串，SSH 为远端路径字符串。
    pub(crate) path: String,
    /// 文件类型。
    pub(crate) kind: ConnectionFileEntryKind,
    /// 普通文件大小；目录或未知大小为 `None`。
    pub(crate) size: Option<u64>,
    /// 修改时间，Unix epoch 毫秒；远端未返回时为 `None`。
    pub(crate) modified_at_ms: Option<i64>,
}

/// 文件类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionFileEntryKind {
    /// 目录，可双击进入。
    Directory,
    /// 普通文件，可预览、上传和下载。
    File,
    /// 符号链接；第一版只展示，不递归追踪。
    Symlink,
    /// 其它类型，例如 socket、设备文件等。
    Other,
}

impl ConnectionFileEntryKind {
    /// 返回用户可见类型标签。
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Directory => "目录",
            Self::File => "文件",
            Self::Symlink => "链接",
            Self::Other => "其它",
        }
    }

    /// 是否可作为普通文件传输或预览。
    pub(crate) fn is_file(self) -> bool {
        matches!(self, Self::File)
    }

    /// 是否可进入的目录。
    pub(crate) fn is_directory(self) -> bool {
        matches!(self, Self::Directory)
    }

    /// 是否允许由文件管理器第一版删除。
    pub(crate) fn is_deletable(self) -> bool {
        matches!(self, Self::File | Self::Symlink | Self::Directory)
    }
}

/// 文件预览结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionFilePreview {
    /// 被预览文件路径。
    pub(crate) path: String,
    /// 文件名。
    pub(crate) name: String,
    /// 文件大小。
    pub(crate) size: Option<u64>,
    /// 修改时间，Unix epoch 毫秒。
    pub(crate) modified_at_ms: Option<i64>,
    /// 文本预览；二进制或超限时为 `None`。
    pub(crate) text: Option<String>,
    /// 不能展示文本时的中文原因。
    pub(crate) message: Option<String>,
}

/// 同名文件冲突。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConnectionFileConflict {
    /// 冲突对应的原始命令。
    pub(crate) command: ConnectionFileCommand,
    /// 冲突文件名列表，UI 只展示前若干项即可。
    pub(crate) names: Vec<String>,
}

/// 启动连接文件管理后端。
pub(crate) fn start_connection_file_backend(
    target: ConnectionFileBackendTarget,
) -> ConnectionFileBackendHandle {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    let cancel_requested = Arc::new(AtomicBool::new(false));
    let backend_cancel_requested = Arc::clone(&cancel_requested);
    thread::spawn(move || {
        run_connection_file_backend(
            target,
            command_receiver,
            event_sender,
            backend_cancel_requested,
        )
    });
    ConnectionFileBackendHandle {
        command_sender,
        event_receiver,
        cancel_requested,
    }
}

/// 返回当前平台的用户 home 目录。
pub(crate) fn local_home_directory_text() -> String {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .to_string_lossy()
        .to_string()
}

/// 返回 SSH 文件管理默认目录。
///
/// 业务意图：
/// - 文件管理当前固定从远端当前用户家目录进入，不再依赖终端 OSC 7 当前目录。
/// - UI 使用 `~` 表达“用户家目录”，后端会把它规范化为 SFTP 会话的 canonical 目录，避免服务器不支持 tilde 展开。
pub(crate) fn ssh_default_directory_text() -> String {
    "~".to_string()
}

/// 从终端输出流中提取最后一个 OSC 7 当前目录。
///
/// 业务意图：
/// - Shell integration 会用 OSC 7 报告当前目录；这里只解析输出，不向终端注入 `pwd`，避免污染用户的命令历史和屏幕内容。
/// - 输出 chunk 可能切断 ESC 序列，因此调用方持有 `buffer`，本函数会保留尚未闭合的尾部数据。
pub(crate) fn update_osc7_cwd_from_output(buffer: &mut Vec<u8>, bytes: &[u8]) -> Option<String> {
    const MAX_OSC_BUFFER: usize = 8192;
    buffer.extend_from_slice(bytes);
    if buffer.len() > MAX_OSC_BUFFER {
        let drain_to = buffer.len().saturating_sub(MAX_OSC_BUFFER);
        buffer.drain(..drain_to);
    }

    let mut latest = None;
    while let Some(start) = find_bytes(buffer, b"\x1b]7;") {
        let payload_start = start + 4;
        let Some((end, terminator_len)) = find_osc_terminator(&buffer[payload_start..]) else {
            if start > 0 {
                buffer.drain(..start);
            }
            break;
        };
        let payload_end = payload_start + end;
        if let Ok(payload) = std::str::from_utf8(&buffer[payload_start..payload_end]) {
            if let Some(path) = parse_osc7_payload(payload) {
                latest = Some(path);
            }
        }
        buffer.drain(..payload_end + terminator_len);
    }
    latest
}

/// 返回指定路径的父目录文本。
pub(crate) fn connection_file_parent_path(path: &str, is_remote: bool) -> Option<String> {
    if is_remote {
        remote_parent_path(path)
    } else {
        let parent = Path::new(path).parent()?;
        Some(parent.to_string_lossy().to_string())
    }
}

/// 运行连接文件管理后端。
fn run_connection_file_backend(
    target: ConnectionFileBackendTarget,
    command_receiver: Receiver<ConnectionFileCommand>,
    event_sender: Sender<ConnectionFileEvent>,
    cancel_requested: Arc<AtomicBool>,
) {
    match target {
        ConnectionFileBackendTarget::Local => {
            run_local_file_backend(command_receiver, event_sender, cancel_requested);
        }
        ConnectionFileBackendTarget::Ssh {
            profile,
            password,
            trusted_fingerprint,
        } => {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    send_file_event(
                        &event_sender,
                        ConnectionFileEvent::Error(format!("启动 SFTP 运行时失败：{error}")),
                    );
                    return;
                }
            };
            runtime.block_on(run_ssh_file_backend(
                profile,
                password,
                trusted_fingerprint,
                command_receiver,
                event_sender,
                cancel_requested,
            ));
        }
    }
}

/// 运行本地文件后端。
fn run_local_file_backend(
    command_receiver: Receiver<ConnectionFileCommand>,
    event_sender: Sender<ConnectionFileEvent>,
    cancel_requested: Arc<AtomicBool>,
) {
    while let Ok(command) = command_receiver.recv() {
        match command {
            ConnectionFileCommand::List { path } => {
                send_file_result(&event_sender, list_local_directory(&path));
            }
            ConnectionFileCommand::Preview { path } => {
                send_file_result(&event_sender, preview_local_file(&path));
            }
            ConnectionFileCommand::Upload(request) => {
                send_file_result(
                    &event_sender,
                    upload_local_files(request, &event_sender, &cancel_requested),
                );
            }
            ConnectionFileCommand::Download(request) => {
                send_file_result(
                    &event_sender,
                    download_local_files(request, &event_sender, &cancel_requested),
                );
            }
            ConnectionFileCommand::Delete(request) => {
                send_file_result(&event_sender, delete_local_entry(request));
            }
            ConnectionFileCommand::Shutdown => break,
        }
    }
}

/// 运行 SSH SFTP 文件后端。
async fn run_ssh_file_backend(
    profile: ConnectionProfile,
    password: Zeroizing<String>,
    trusted_fingerprint: Option<String>,
    command_receiver: Receiver<ConnectionFileCommand>,
    event_sender: Sender<ConnectionFileEvent>,
    cancel_requested: Arc<AtomicBool>,
) {
    let sftp = match open_sftp_session(profile, password, trusted_fingerprint, event_sender.clone())
        .await
    {
        Ok(sftp) => sftp,
        Err(error) => {
            send_file_event(&event_sender, ConnectionFileEvent::Error(error));
            return;
        }
    };

    while let Ok(command) = command_receiver.recv() {
        match command {
            ConnectionFileCommand::List { path } => {
                let result = list_ssh_directory(&sftp, &path).await;
                send_file_result(&event_sender, result);
            }
            ConnectionFileCommand::Preview { path } => {
                let result = preview_ssh_file(&sftp, &path).await;
                send_file_result(&event_sender, result);
            }
            ConnectionFileCommand::Upload(request) => {
                let result =
                    upload_ssh_files(&sftp, request, &event_sender, &cancel_requested).await;
                send_file_result(&event_sender, result);
            }
            ConnectionFileCommand::Download(request) => {
                let result =
                    download_ssh_files(&sftp, request, &event_sender, &cancel_requested).await;
                send_file_result(&event_sender, result);
            }
            ConnectionFileCommand::Delete(request) => {
                let result = delete_ssh_entry(&sftp, request).await;
                send_file_result(&event_sender, result);
            }
            ConnectionFileCommand::Shutdown => {
                let _ = sftp.close().await;
                break;
            }
        }
    }
}

/// 建立独立 SFTP 会话。
async fn open_sftp_session(
    profile: ConnectionProfile,
    password: Zeroizing<String>,
    trusted_fingerprint: Option<String>,
    event_sender: Sender<ConnectionFileEvent>,
) -> Result<SftpSession, String> {
    let host_key_blocked = Arc::new(AtomicBool::new(false));
    let handler = SftpClientHandler {
        trusted_fingerprint,
        event_sender,
        host_key_blocked: Arc::clone(&host_key_blocked),
    };
    let config = Arc::new(client::Config::default());
    let address = (profile.host.as_str(), profile.port);
    let mut session = client::connect(config, address, handler)
        .await
        .map_err(|error| {
            if host_key_blocked.load(Ordering::SeqCst) {
                "SFTP 主机指纹未通过校验，文件管理已阻止连接".to_string()
            } else {
                format!("连接 SFTP 失败：{error}")
            }
        })?;

    let auth = session
        .authenticate_password(profile.username.as_str(), password.as_str())
        .await
        .map_err(|error| format!("SFTP 密码认证失败：{error}"))?;
    if !auth.success() {
        return Err("SFTP 密码认证失败：用户名或密码不正确".to_string());
    }

    let channel = session
        .channel_open_session()
        .await
        .map_err(|error| format!("打开 SFTP channel 失败：{error}"))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|error| format!("启动 SFTP 子系统失败：{error}"))?;
    SftpSession::new(channel.into_stream())
        .await
        .map_err(|error| format!("初始化 SFTP 会话失败：{error}"))
}

/// SFTP client handler，只负责主机指纹校验。
struct SftpClientHandler {
    /// 本地保存的可信指纹。
    trusted_fingerprint: Option<String>,
    /// 主机指纹失败时返回 UI 的事件通道。
    event_sender: Sender<ConnectionFileEvent>,
    /// 标记连接失败是否由指纹阻断导致，避免再追加低层网络错误。
    host_key_blocked: Arc<AtomicBool>,
}

impl Handler for SftpClientHandler {
    type Error = russh::Error;

    /// 校验服务器公钥指纹。
    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fingerprint = server_public_key.fingerprint(HashAlg::Sha256).to_string();
        match verify_connection_host_key(self.trusted_fingerprint.as_deref(), &fingerprint) {
            HostKeyVerification::Trusted => Ok(true),
            HostKeyVerification::Unknown { actual } => {
                self.host_key_blocked.store(true, Ordering::SeqCst);
                send_file_event(
                    &self.event_sender,
                    ConnectionFileEvent::Error(format!(
                        "SFTP 主机指纹尚未信任，请先通过终端连接确认：{actual}"
                    )),
                );
                Ok(false)
            }
            HostKeyVerification::Mismatch { expected, actual } => {
                self.host_key_blocked.store(true, Ordering::SeqCst);
                send_file_event(
                    &self.event_sender,
                    ConnectionFileEvent::Error(format!(
                        "SFTP 主机指纹不匹配，已阻止连接。已信任：{expected}，本次：{actual}"
                    )),
                );
                Ok(false)
            }
        }
    }
}

/// 列出本地目录。
fn list_local_directory(path: &str) -> Result<ConnectionFileEvent, String> {
    let path = normalize_local_path(path);
    let read_dir = fs::read_dir(&path)
        .map_err(|error| format!("读取目录 {} 失败：{error}", path.display()))?;
    let mut entries = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|error| format!("读取目录项失败：{error}"))?;
        let metadata = entry
            .metadata()
            .map_err(|error| format!("读取文件元信息失败：{error}"))?;
        let kind = local_entry_kind(&metadata);
        let name = entry.file_name().to_string_lossy().to_string();
        entries.push(ConnectionFileEntry {
            name,
            path: entry.path().to_string_lossy().to_string(),
            kind,
            size: kind.is_file().then_some(metadata.len()),
            modified_at_ms: metadata.modified().ok().and_then(system_time_to_millis),
        });
    }
    sort_file_entries(&mut entries);
    Ok(ConnectionFileEvent::Listed {
        path: path.to_string_lossy().to_string(),
        entries,
    })
}

/// 列出 SSH 目录。
async fn list_ssh_directory(sftp: &SftpSession, path: &str) -> Result<ConnectionFileEvent, String> {
    let canonical = canonicalize_ssh_path(sftp, path).await?;
    let read_dir = sftp
        .read_dir(canonical.clone())
        .await
        .map_err(|error| format!("读取远端目录 {canonical} 失败：{error}"))?;
    let mut entries = Vec::new();
    for entry in read_dir {
        let metadata = entry.metadata();
        let kind = ssh_entry_kind(metadata.file_type());
        let name = entry.file_name();
        let path = join_remote_path(&canonical, &name);
        entries.push(ConnectionFileEntry {
            name,
            path,
            kind,
            size: kind.is_file().then_some(metadata.len()),
            modified_at_ms: metadata.modified().ok().and_then(system_time_to_millis),
        });
    }
    sort_file_entries(&mut entries);
    Ok(ConnectionFileEvent::Listed {
        path: canonical,
        entries,
    })
}

/// 预览本地普通文件。
fn preview_local_file(path: &str) -> Result<ConnectionFileEvent, String> {
    let path = normalize_local_path(path);
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("读取文件 {} 元信息失败：{error}", path.display()))?;
    if !metadata.is_file() {
        return Err("只能预览普通文件".to_string());
    }
    let bytes = read_preview_bytes_local(&path, metadata.len())?;
    let preview = build_preview(
        path.to_string_lossy().to_string(),
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string()),
        Some(metadata.len()),
        metadata.modified().ok().and_then(system_time_to_millis),
        bytes,
    );
    Ok(ConnectionFileEvent::Previewed(preview))
}

/// 预览 SSH 普通文件。
async fn preview_ssh_file(sftp: &SftpSession, path: &str) -> Result<ConnectionFileEvent, String> {
    let metadata = sftp
        .metadata(path.to_string())
        .await
        .map_err(|error| format!("读取远端文件 {path} 元信息失败：{error}"))?;
    if !metadata.file_type().is_file() {
        return Err("只能预览普通文件".to_string());
    }
    let bytes = if metadata.len() as usize > CONNECTION_FILE_PREVIEW_LIMIT_BYTES {
        PreviewBytes::Oversize
    } else {
        let mut file = sftp
            .open(path.to_string())
            .await
            .map_err(|error| format!("打开远端文件 {path} 失败：{error}"))?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)
            .await
            .map_err(|error| format!("读取远端文件 {path} 失败：{error}"))?;
        PreviewBytes::Bytes(buffer)
    };
    let preview = build_preview(
        path.to_string(),
        remote_file_name(path).unwrap_or_else(|| path.to_string()),
        Some(metadata.len()),
        metadata.modified().ok().and_then(system_time_to_millis),
        bytes,
    );
    Ok(ConnectionFileEvent::Previewed(preview))
}

/// 删除本地普通文件、符号链接或空目录。
fn delete_local_entry(request: ConnectionFileDeleteRequest) -> Result<ConnectionFileEvent, String> {
    if !request.kind.is_deletable() {
        return Err("仅支持删除普通文件、符号链接或空目录".to_string());
    }
    let path = normalize_local_path(&request.path);
    match request.kind {
        ConnectionFileEntryKind::File | ConnectionFileEntryKind::Symlink => {
            fs::remove_file(&path)
                .map_err(|error| format!("删除文件 {} 失败：{error}", path.display()))?;
        }
        ConnectionFileEntryKind::Directory => {
            fs::remove_dir(&path)
                .map_err(|error| format!("删除空目录 {} 失败：{error}", path.display()))?;
        }
        ConnectionFileEntryKind::Other => {
            return Err("仅支持删除普通文件、符号链接或空目录".to_string());
        }
    }
    Ok(ConnectionFileEvent::OperationFinished(format!(
        "已删除 {}",
        request.name
    )))
}

/// 删除 SSH 端普通文件、符号链接或空目录。
async fn delete_ssh_entry(
    sftp: &SftpSession,
    request: ConnectionFileDeleteRequest,
) -> Result<ConnectionFileEvent, String> {
    if !request.kind.is_deletable() {
        return Err("仅支持删除普通文件、符号链接或空目录".to_string());
    }
    match request.kind {
        ConnectionFileEntryKind::File | ConnectionFileEntryKind::Symlink => {
            sftp.remove_file(request.path.clone())
                .await
                .map_err(|error| format!("删除远端文件 {} 失败：{error}", request.path))?;
        }
        ConnectionFileEntryKind::Directory => {
            sftp.remove_dir(request.path.clone())
                .await
                .map_err(|error| format!("删除远端空目录 {} 失败：{error}", request.path))?;
        }
        ConnectionFileEntryKind::Other => {
            return Err("仅支持删除普通文件、符号链接或空目录".to_string());
        }
    }
    Ok(ConnectionFileEvent::OperationFinished(format!(
        "已删除 {}",
        request.name
    )))
}

/// 上传本地文件到本地目录。
fn upload_local_files(
    request: ConnectionFileTransferRequest,
    event_sender: &Sender<ConnectionFileEvent>,
    cancel_requested: &Arc<AtomicBool>,
) -> Result<ConnectionFileEvent, String> {
    let target_dir = normalize_local_path(&request.target_dir);
    let conflicts = transfer_conflicts_local(&request.source_paths, &target_dir)?;
    if request.conflict_policy == ConnectionFileConflictPolicy::Ask && !conflicts.is_empty() {
        return Ok(ConnectionFileEvent::Conflict(ConnectionFileConflict {
            command: ConnectionFileCommand::Upload(request),
            names: conflicts,
        }));
    }
    cancel_requested.store(false, Ordering::SeqCst);
    let result = copy_to_local_dir(
        &request.source_paths,
        &target_dir,
        request.conflict_policy,
        ConnectionFileTransferOperation::Upload,
        event_sender,
        cancel_requested,
    )?;
    if result.cancelled {
        return Ok(cancelled_transfer_event(
            ConnectionFileTransferOperation::Upload,
        ));
    }
    Ok(ConnectionFileEvent::OperationFinished(format!(
        "已上传 {} 个文件",
        result.copied
    )))
}

/// 下载本地文件到本地目录。
fn download_local_files(
    request: ConnectionFileTransferRequest,
    event_sender: &Sender<ConnectionFileEvent>,
    cancel_requested: &Arc<AtomicBool>,
) -> Result<ConnectionFileEvent, String> {
    let target_dir = normalize_local_path(&request.target_dir);
    let conflicts = transfer_conflicts_local(&request.source_paths, &target_dir)?;
    if request.conflict_policy == ConnectionFileConflictPolicy::Ask && !conflicts.is_empty() {
        return Ok(ConnectionFileEvent::Conflict(ConnectionFileConflict {
            command: ConnectionFileCommand::Download(request),
            names: conflicts,
        }));
    }
    cancel_requested.store(false, Ordering::SeqCst);
    let result = copy_to_local_dir(
        &request.source_paths,
        &target_dir,
        request.conflict_policy,
        ConnectionFileTransferOperation::Download,
        event_sender,
        cancel_requested,
    )?;
    if result.cancelled {
        return Ok(cancelled_transfer_event(
            ConnectionFileTransferOperation::Download,
        ));
    }
    Ok(ConnectionFileEvent::OperationFinished(format!(
        "已下载 {} 个文件",
        result.copied
    )))
}

/// 上传本地文件到 SSH 目录。
async fn upload_ssh_files(
    sftp: &SftpSession,
    request: ConnectionFileTransferRequest,
    event_sender: &Sender<ConnectionFileEvent>,
    cancel_requested: &Arc<AtomicBool>,
) -> Result<ConnectionFileEvent, String> {
    let target_dir = canonicalize_ssh_path(sftp, &request.target_dir).await?;
    let conflicts = transfer_conflicts_ssh(sftp, &request.source_paths, &target_dir).await?;
    if request.conflict_policy == ConnectionFileConflictPolicy::Ask && !conflicts.is_empty() {
        return Ok(ConnectionFileEvent::Conflict(ConnectionFileConflict {
            command: ConnectionFileCommand::Upload(request),
            names: conflicts,
        }));
    }

    let mut transfer_items = Vec::new();
    for source in &request.source_paths {
        let source_path = PathBuf::from(source);
        let metadata = fs::metadata(&source_path)
            .map_err(|error| format!("读取本地文件 {} 失败：{error}", source_path.display()))?;
        if !metadata.is_file() {
            continue;
        }
        let file_name = source_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .ok_or_else(|| "本地文件名无效，无法上传".to_string())?;
        let dest = join_remote_path(&target_dir, &file_name);
        if request.conflict_policy == ConnectionFileConflictPolicy::Skip
            && sftp
                .try_exists(dest.clone())
                .await
                .map_err(|error| format!("检查远端文件 {dest} 是否存在失败：{error}"))?
        {
            continue;
        }
        transfer_items.push((source_path, dest, metadata.len()));
    }

    let total_files = transfer_items.len();
    let total_bytes = transfer_items.iter().map(|(_, _, size)| *size).sum::<u64>();
    let mut progress = ConnectionFileProgressReporter::new(
        event_sender,
        ConnectionFileTransferOperation::Upload,
        total_files,
        total_bytes,
    );
    cancel_requested.store(false, Ordering::SeqCst);
    progress.emit_if_due(true);

    let mut copied = 0usize;
    for (source_path, dest, _) in transfer_items {
        if cancel_requested.load(Ordering::SeqCst) {
            return Ok(cancelled_transfer_event(
                ConnectionFileTransferOperation::Upload,
            ));
        }
        let mut source_file = fs::File::open(&source_path)
            .map_err(|error| format!("打开本地文件 {} 失败：{error}", source_path.display()))?;
        let temp = remote_transfer_sidecar_path(&dest, "tmp");
        let mut dest_file = sftp
            .create(temp.clone())
            .await
            .map_err(|error| format!("创建远端临时文件 {temp} 失败：{error}"))?;
        let mut buffer = [0u8; 64 * 1024];
        let mut cancelled = false;
        loop {
            if cancel_requested.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            let read = source_file
                .read(&mut buffer)
                .map_err(|error| format!("读取本地文件 {} 失败：{error}", source_path.display()))?;
            if read == 0 {
                break;
            }
            if cancel_requested.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            dest_file
                .write_all(&buffer[..read])
                .await
                .map_err(|error| format!("写入远端临时文件 {temp} 失败：{error}"))?;
            progress.add_bytes(read as u64);
        }
        dest_file
            .shutdown()
            .await
            .map_err(|error| format!("关闭远端临时文件 {temp} 失败：{error}"))?;
        if cancelled {
            let _ = sftp.remove_file(temp.clone()).await;
            return Ok(cancelled_transfer_event(
                ConnectionFileTransferOperation::Upload,
            ));
        }
        if cancel_requested.load(Ordering::SeqCst) {
            let _ = sftp.remove_file(temp.clone()).await;
            return Ok(cancelled_transfer_event(
                ConnectionFileTransferOperation::Upload,
            ));
        }
        replace_ssh_file_with_temp(sftp, &temp, &dest).await?;
        copied = copied.saturating_add(1);
        progress.complete_file();
    }
    progress.finish();
    Ok(ConnectionFileEvent::OperationFinished(format!(
        "已上传 {copied} 个文件"
    )))
}

/// 下载 SSH 文件到本地目录。
async fn download_ssh_files(
    sftp: &SftpSession,
    request: ConnectionFileTransferRequest,
    event_sender: &Sender<ConnectionFileEvent>,
    cancel_requested: &Arc<AtomicBool>,
) -> Result<ConnectionFileEvent, String> {
    let target_dir = normalize_local_path(&request.target_dir);
    let conflicts =
        transfer_conflicts_download_ssh(sftp, &request.source_paths, &target_dir).await?;
    if request.conflict_policy == ConnectionFileConflictPolicy::Ask && !conflicts.is_empty() {
        return Ok(ConnectionFileEvent::Conflict(ConnectionFileConflict {
            command: ConnectionFileCommand::Download(request),
            names: conflicts,
        }));
    }

    let mut transfer_items = Vec::new();
    for source in &request.source_paths {
        let metadata = sftp
            .metadata(source.clone())
            .await
            .map_err(|error| format!("读取远端文件 {source} 元信息失败：{error}"))?;
        if !metadata.file_type().is_file() {
            continue;
        }
        let file_name =
            remote_file_name(source).ok_or_else(|| "远端文件名无效，无法下载".to_string())?;
        let dest = target_dir.join(file_name);
        if request.conflict_policy == ConnectionFileConflictPolicy::Skip && dest.exists() {
            continue;
        }
        transfer_items.push((source.clone(), dest, metadata.len()));
    }

    let total_files = transfer_items.len();
    let total_bytes = transfer_items.iter().map(|(_, _, size)| *size).sum::<u64>();
    let mut progress = ConnectionFileProgressReporter::new(
        event_sender,
        ConnectionFileTransferOperation::Download,
        total_files,
        total_bytes,
    );
    cancel_requested.store(false, Ordering::SeqCst);
    progress.emit_if_due(true);

    let mut copied = 0usize;
    for (source, dest, _) in transfer_items {
        if cancel_requested.load(Ordering::SeqCst) {
            return Ok(cancelled_transfer_event(
                ConnectionFileTransferOperation::Download,
            ));
        }
        let mut source_file = sftp
            .open(source.clone())
            .await
            .map_err(|error| format!("打开远端文件 {source} 失败：{error}"))?;
        let temp = local_transfer_sidecar_path(&dest, "tmp");
        let mut dest_file = fs::File::create(&temp)
            .map_err(|error| format!("创建本地临时文件 {} 失败：{error}", temp.display()))?;
        let mut buffer = [0u8; 64 * 1024];
        let mut cancelled = false;
        loop {
            if cancel_requested.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            let read = source_file
                .read(&mut buffer)
                .await
                .map_err(|error| format!("读取远端文件 {source} 失败：{error}"))?;
            if read == 0 {
                break;
            }
            if cancel_requested.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            dest_file
                .write_all(&buffer[..read])
                .map_err(|error| format!("写入本地临时文件 {} 失败：{error}", temp.display()))?;
            progress.add_bytes(read as u64);
        }
        dest_file
            .flush()
            .map_err(|error| format!("刷新本地临时文件 {} 失败：{error}", temp.display()))?;
        drop(dest_file);
        if cancelled {
            let _ = fs::remove_file(&temp);
            return Ok(cancelled_transfer_event(
                ConnectionFileTransferOperation::Download,
            ));
        }
        if cancel_requested.load(Ordering::SeqCst) {
            let _ = fs::remove_file(&temp);
            return Ok(cancelled_transfer_event(
                ConnectionFileTransferOperation::Download,
            ));
        }
        replace_local_file_with_temp(&temp, &dest)?;
        copied = copied.saturating_add(1);
        progress.complete_file();
    }
    progress.finish();
    Ok(ConnectionFileEvent::OperationFinished(format!(
        "已下载 {copied} 个文件"
    )))
}

/// 构建文件预览结果。
fn build_preview(
    path: String,
    name: String,
    size: Option<u64>,
    modified_at_ms: Option<i64>,
    bytes: PreviewBytes,
) -> ConnectionFilePreview {
    match bytes {
        PreviewBytes::Oversize => ConnectionFilePreview {
            path,
            name,
            size,
            modified_at_ms,
            text: None,
            message: Some("文件过大，仅支持下载".to_string()),
        },
        PreviewBytes::Bytes(bytes) if looks_like_binary(&bytes) => ConnectionFilePreview {
            path,
            name,
            size,
            modified_at_ms,
            text: None,
            message: Some("疑似二进制文件，仅显示元信息".to_string()),
        },
        PreviewBytes::Bytes(bytes) => match decode_log_bytes(&bytes, EncodingChoice::Auto, &name) {
            Ok(document) => ConnectionFilePreview {
                path,
                name,
                size,
                modified_at_ms,
                text: Some(document.lines.join("\n")),
                message: Some(format!("文本预览，编码：{}", document.encoding.label())),
            },
            Err(error) => ConnectionFilePreview {
                path,
                name,
                size,
                modified_at_ms,
                text: None,
                message: Some(format!("无法解码为文本：{error}")),
            },
        },
    }
}

/// 预览读取结果。
enum PreviewBytes {
    /// 文件超过预览上限。
    Oversize,
    /// 已读取的预览字节。
    Bytes(Vec<u8>),
}

/// 读取本地预览字节。
fn read_preview_bytes_local(path: &Path, size: u64) -> Result<PreviewBytes, String> {
    if size as usize > CONNECTION_FILE_PREVIEW_LIMIT_BYTES {
        return Ok(PreviewBytes::Oversize);
    }
    let mut file = fs::File::open(path)
        .map_err(|error| format!("打开文件 {} 失败：{error}", path.display()))?;
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer)
        .map_err(|error| format!("读取文件 {} 失败：{error}", path.display()))?;
    Ok(PreviewBytes::Bytes(buffer))
}

/// 判断字节内容是否疑似二进制。
fn looks_like_binary(bytes: &[u8]) -> bool {
    if bytes.contains(&0) {
        return true;
    }
    let sample_len = bytes.len().min(4096);
    if sample_len == 0 {
        return false;
    }
    let control_count = bytes[..sample_len]
        .iter()
        .filter(|byte| byte.is_ascii_control() && !matches!(byte, b'\n' | b'\r' | b'\t' | 0x0C))
        .count();
    control_count * 100 / sample_len > 15
}

/// 文件传输进度上报器。
///
/// 业务意图：
/// - 大文件复制可能持续数秒，后端需要周期性把字节进度送回 UI，但不能每个小块都刷新主线程。
/// - 这里按 100ms 节流，并在开始、单文件完成和全部完成时强制上报，保证进度条及时且不会过度占用事件队列。
struct ConnectionFileProgressReporter<'a> {
    /// 事件发送通道。
    event_sender: &'a Sender<ConnectionFileEvent>,
    /// 上传或下载。
    operation: ConnectionFileTransferOperation,
    /// 需要传输的普通文件数量。
    total_files: usize,
    /// 本次传输总字节数。
    total_bytes: u64,
    /// 已完成文件数量。
    completed_files: usize,
    /// 已传输字节数。
    transferred_bytes: u64,
    /// 传输开始时间。
    started_at: Instant,
    /// 最近一次上报时间。
    last_emit_at: Instant,
}

/// 本地复制任务结果。
///
/// 业务意图：
/// - 本地上传和本地下载都复用同一套复制逻辑，但取消时既不能报红色错误，也不能当作全部完成。
/// - 因此返回已完成文件数和取消标记，由上层决定展示“已取消上传/下载”还是正常完成消息。
struct ConnectionFileLocalCopyResult {
    /// 已完整复制的普通文件数量。
    copied: usize,
    /// 是否由用户主动取消。
    cancelled: bool,
}

impl<'a> ConnectionFileProgressReporter<'a> {
    /// 创建新的进度上报器。
    fn new(
        event_sender: &'a Sender<ConnectionFileEvent>,
        operation: ConnectionFileTransferOperation,
        total_files: usize,
        total_bytes: u64,
    ) -> Self {
        let now = Instant::now();
        Self {
            event_sender,
            operation,
            total_files,
            total_bytes,
            completed_files: 0,
            transferred_bytes: 0,
            started_at: now,
            last_emit_at: now.checked_sub(Duration::from_millis(200)).unwrap_or(now),
        }
    }

    /// 累加字节进度，并在节流窗口到达时上报。
    fn add_bytes(&mut self, bytes: u64) {
        self.transferred_bytes = self.transferred_bytes.saturating_add(bytes);
        self.emit_if_due(false);
    }

    /// 标记一个普通文件已完成。
    fn complete_file(&mut self) {
        self.completed_files = self.completed_files.saturating_add(1);
        self.emit_if_due(true);
    }

    /// 强制发送最终进度。
    fn finish(&mut self) {
        self.completed_files = self.total_files;
        self.transferred_bytes = self.total_bytes;
        self.emit_if_due(true);
    }

    /// 按节流规则发送进度。
    fn emit_if_due(&mut self, force: bool) {
        let now = Instant::now();
        if !force && now.duration_since(self.last_emit_at) < Duration::from_millis(100) {
            return;
        }
        self.last_emit_at = now;
        let elapsed_ms = now.duration_since(self.started_at).as_millis();
        let elapsed_ms = u64::try_from(elapsed_ms).unwrap_or(u64::MAX).max(1);
        send_file_event(
            self.event_sender,
            ConnectionFileEvent::TransferProgress(ConnectionFileTransferProgress {
                operation: self.operation,
                completed_files: self.completed_files.min(self.total_files),
                total_files: self.total_files,
                transferred_bytes: self.transferred_bytes.min(self.total_bytes),
                total_bytes: self.total_bytes,
                elapsed_ms,
            }),
        );
    }
}

/// 复制普通文件到本地目录。
fn copy_to_local_dir(
    source_paths: &[String],
    target_dir: &Path,
    policy: ConnectionFileConflictPolicy,
    operation: ConnectionFileTransferOperation,
    event_sender: &Sender<ConnectionFileEvent>,
    cancel_requested: &Arc<AtomicBool>,
) -> Result<ConnectionFileLocalCopyResult, String> {
    let mut transfer_items = Vec::new();
    for source in source_paths {
        let source_path = PathBuf::from(source);
        let metadata = fs::metadata(&source_path)
            .map_err(|error| format!("读取文件 {} 失败：{error}", source_path.display()))?;
        if !metadata.is_file() {
            continue;
        }
        let file_name = source_path
            .file_name()
            .ok_or_else(|| "文件名无效，无法复制".to_string())?
            .to_os_string();
        let dest = target_dir.join(file_name);
        // 本地文件管理的“上传/下载”在本地终端场景下本质都是本机文件复制。
        // 如果用户把文件复制回自身所在目录，源和目标会是同一个文件；这种情况不能进入覆盖流程，
        // 否则目标创建会先截断源文件。这里在目标存在时用规范路径判断并直接跳过。
        if same_local_file(&source_path, &dest) {
            continue;
        }
        if policy == ConnectionFileConflictPolicy::Skip && dest.exists() {
            continue;
        }
        transfer_items.push((source_path, dest, metadata.len()));
    }

    let total_files = transfer_items.len();
    let total_bytes = transfer_items.iter().map(|(_, _, size)| *size).sum::<u64>();
    let mut progress =
        ConnectionFileProgressReporter::new(event_sender, operation, total_files, total_bytes);
    progress.emit_if_due(true);

    let mut copied = 0usize;
    for (source_path, dest, _) in transfer_items {
        if cancel_requested.load(Ordering::SeqCst) {
            return Ok(ConnectionFileLocalCopyResult {
                copied,
                cancelled: true,
            });
        }
        let completed =
            copy_file_to_local_with_progress(&source_path, &dest, &mut progress, cancel_requested)?;
        if !completed {
            return Ok(ConnectionFileLocalCopyResult {
                copied,
                cancelled: true,
            });
        }
        copied = copied.saturating_add(1);
        progress.complete_file();
    }
    progress.finish();
    Ok(ConnectionFileLocalCopyResult {
        copied,
        cancelled: false,
    })
}

/// 以分块方式复制本地文件并上报字节进度。
fn copy_file_to_local_with_progress(
    source_path: &Path,
    dest: &Path,
    progress: &mut ConnectionFileProgressReporter<'_>,
    cancel_requested: &Arc<AtomicBool>,
) -> Result<bool, String> {
    let temp = local_transfer_sidecar_path(dest, "tmp");
    let mut source_file = fs::File::open(source_path)
        .map_err(|error| format!("打开文件 {} 失败：{error}", source_path.display()))?;
    let mut dest_file = fs::File::create(&temp)
        .map_err(|error| format!("创建临时文件 {} 失败：{error}", temp.display()))?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if cancel_requested.load(Ordering::SeqCst) {
            drop(dest_file);
            let _ = fs::remove_file(&temp);
            return Ok(false);
        }
        let read = source_file
            .read(&mut buffer)
            .map_err(|error| format!("读取文件 {} 失败：{error}", source_path.display()))?;
        if read == 0 {
            break;
        }
        if cancel_requested.load(Ordering::SeqCst) {
            drop(dest_file);
            let _ = fs::remove_file(&temp);
            return Ok(false);
        }
        dest_file
            .write_all(&buffer[..read])
            .map_err(|error| format!("写入临时文件 {} 失败：{error}", temp.display()))?;
        progress.add_bytes(read as u64);
    }
    dest_file
        .flush()
        .map_err(|error| format!("刷新临时文件 {} 失败：{error}", temp.display()))?;
    drop(dest_file);
    if cancel_requested.load(Ordering::SeqCst) {
        let _ = fs::remove_file(&temp);
        return Ok(false);
    }
    replace_local_file_with_temp(&temp, dest)?;
    Ok(true)
}

/// 判断两个本地路径是否指向同一个普通文件。
///
/// 业务意图：
/// - 本地文件管理允许把文件下载/上传到任意本机目录，用户选择当前目录时会形成自复制。
/// - 自复制如果继续走覆盖写入，会把源文件截断；因此在生成传输任务前需要用规范路径识别并跳过。
fn same_local_file(source: &Path, dest: &Path) -> bool {
    let Ok(source) = fs::canonicalize(source) else {
        return false;
    };
    let Ok(dest) = fs::canonicalize(dest) else {
        return false;
    };
    source == dest
}

/// 构造与目标文件同目录的本地临时/备份路径。
///
/// 跨平台约束：
/// - Windows 上 `rename` 通常不能直接覆盖已存在目标，因此覆盖前需要同目录备份。
/// - 临时文件必须放在目标同目录，保证最终 rename 尽量是同文件系统内操作。
fn local_transfer_sidecar_path(dest: &Path, kind: &str) -> PathBuf {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let file_name = dest
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for attempt in 0..1000u16 {
        let candidate = parent.join(format!(
            ".{file_name}.logclinic-{kind}-{}-{nonce}-{attempt}",
            std::process::id()
        ));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!(
        ".{file_name}.logclinic-{kind}-{}-{nonce}-fallback",
        std::process::id()
    ))
}

/// 用已经完整写入的临时文件替换目标文件。
///
/// 业务意图：
/// - 覆盖传输必须在完整写入后才触碰原目标；用户取消或写入失败时，原文件应保持不变。
/// - macOS/Linux 多数情况下可直接 rename 覆盖；Windows 需要先把旧文件挪到备份路径，再放入新文件。
fn replace_local_file_with_temp(temp: &Path, dest: &Path) -> Result<(), String> {
    match fs::rename(temp, dest) {
        Ok(()) => Ok(()),
        Err(_rename_error) if dest.exists() => {
            let backup = local_transfer_sidecar_path(dest, "backup");
            fs::rename(dest, &backup)
                .map_err(|error| format!("备份旧文件 {} 失败：{error}", dest.display()))?;
            match fs::rename(temp, dest) {
                Ok(()) => {
                    let _ = fs::remove_file(&backup);
                    Ok(())
                }
                Err(error) => {
                    let _ = fs::rename(&backup, dest);
                    let _ = fs::remove_file(temp);
                    Err(format!("替换文件 {} 失败：{error}", dest.display()))
                }
            }
        }
        Err(error) => Err(format!(
            "移动临时文件 {} 到 {} 失败：{error}",
            temp.display(),
            dest.display()
        )),
    }
}

/// 构造远端同目录的临时/备份路径。
///
/// 业务意图：
/// - SFTP 覆盖上传也必须先写临时文件，避免取消或写入失败时破坏原远端文件。
/// - 远端路径按 POSIX 语义拼接，不使用本机 `PathBuf`，避免 Windows 分隔符污染 SSH 路径。
fn remote_transfer_sidecar_path(dest: &str, kind: &str) -> String {
    let parent = remote_parent_path(dest).unwrap_or_else(|| ".".to_string());
    let file_name = remote_file_name(dest).unwrap_or_else(|| "file".to_string());
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    join_remote_path(
        &parent,
        &format!(
            ".{file_name}.logclinic-{kind}-{}-{nonce}",
            std::process::id()
        ),
    )
}

/// 用远端临时文件替换目标文件。
///
/// 边界条件：
/// - 不同 SFTP 服务端对 rename 覆盖的支持不一致；先尝试直接 rename，失败且目标存在时再用备份回退方案。
/// - 如果最终替换失败，会尽力恢复旧文件并删除临时文件，避免取消/错误路径误删用户已有文件。
async fn replace_ssh_file_with_temp(
    sftp: &SftpSession,
    temp: &str,
    dest: &str,
) -> Result<(), String> {
    match sftp.rename(temp.to_string(), dest.to_string()).await {
        Ok(()) => Ok(()),
        Err(first_error) => {
            let exists = sftp
                .try_exists(dest.to_string())
                .await
                .map_err(|error| format!("检查远端文件 {dest} 是否存在失败：{error}"))?;
            if !exists {
                let _ = sftp.remove_file(temp.to_string()).await;
                return Err(format!("替换远端文件 {dest} 失败：{first_error}"));
            }

            let backup = remote_transfer_sidecar_path(dest, "backup");
            sftp.rename(dest.to_string(), backup.clone())
                .await
                .map_err(|error| format!("备份远端文件 {dest} 失败：{error}"))?;
            match sftp.rename(temp.to_string(), dest.to_string()).await {
                Ok(()) => {
                    let _ = sftp.remove_file(backup).await;
                    Ok(())
                }
                Err(error) => {
                    let _ = sftp.rename(backup.clone(), dest.to_string()).await;
                    let _ = sftp.remove_file(temp.to_string()).await;
                    Err(format!("替换远端文件 {dest} 失败：{error}"))
                }
            }
        }
    }
}

/// 构建用户主动取消后的完成事件。
fn cancelled_transfer_event(operation: ConnectionFileTransferOperation) -> ConnectionFileEvent {
    ConnectionFileEvent::OperationFinished(format!("已取消{}", operation.label()))
}

/// 检查本地目标目录下的同名冲突。
fn transfer_conflicts_local(
    source_paths: &[String],
    target_dir: &Path,
) -> Result<Vec<String>, String> {
    let mut conflicts = Vec::new();
    for source in source_paths {
        let source_path = PathBuf::from(source);
        let Some(file_name) = source_path.file_name() else {
            continue;
        };
        if target_dir.join(file_name).exists() {
            conflicts.push(file_name.to_string_lossy().to_string());
        }
    }
    Ok(conflicts)
}

/// 检查 SSH 上传的远端同名冲突。
async fn transfer_conflicts_ssh(
    sftp: &SftpSession,
    source_paths: &[String],
    target_dir: &str,
) -> Result<Vec<String>, String> {
    let mut conflicts = Vec::new();
    for source in source_paths {
        let source_path = PathBuf::from(source);
        let Some(file_name) = source_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
        else {
            continue;
        };
        let dest = join_remote_path(target_dir, &file_name);
        if sftp
            .try_exists(dest.clone())
            .await
            .map_err(|error| format!("检查远端文件 {dest} 是否存在失败：{error}"))?
        {
            conflicts.push(file_name);
        }
    }
    Ok(conflicts)
}

/// 检查 SSH 下载到本地时的同名冲突。
async fn transfer_conflicts_download_ssh(
    sftp: &SftpSession,
    source_paths: &[String],
    target_dir: &Path,
) -> Result<Vec<String>, String> {
    let mut conflicts = Vec::new();
    for source in source_paths {
        let metadata = sftp
            .metadata(source.clone())
            .await
            .map_err(|error| format!("读取远端文件 {source} 元信息失败：{error}"))?;
        if !metadata.file_type().is_file() {
            continue;
        }
        let Some(file_name) = remote_file_name(source) else {
            continue;
        };
        if target_dir.join(&file_name).exists() {
            conflicts.push(file_name);
        }
    }
    Ok(conflicts)
}

/// 将本地路径文本规范化。
fn normalize_local_path(path: &str) -> PathBuf {
    let expanded = if path == "~" {
        local_home_directory_text()
    } else if let Some(rest) = path.strip_prefix("~/") {
        PathBuf::from(local_home_directory_text())
            .join(rest)
            .to_string_lossy()
            .to_string()
    } else {
        path.to_string()
    };
    let path = PathBuf::from(expanded);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

/// 规范化 SSH 路径。
///
/// 业务意图：
/// - SFTP 协议没有可靠的“当前 shell 目录”，文件管理默认进入 SFTP 会话所属用户的 home。
/// - 部分 SFTP 服务端不支持 `~` 字符串展开，因此把 `~`/空路径映射到 `.`，由服务端返回登录用户的 canonical 目录。
async fn canonicalize_ssh_path(sftp: &SftpSession, path: &str) -> Result<String, String> {
    let normalized = if path.trim().is_empty() || path == "~" {
        "."
    } else {
        path
    };
    sftp.canonicalize(normalized.to_string())
        .await
        .map_err(|error| format!("解析远端路径 {path} 失败：{error}"))
}

/// 本地文件类型映射。
fn local_entry_kind(metadata: &fs::Metadata) -> ConnectionFileEntryKind {
    if metadata.is_dir() {
        ConnectionFileEntryKind::Directory
    } else if metadata.is_file() {
        ConnectionFileEntryKind::File
    } else {
        ConnectionFileEntryKind::Other
    }
}

/// SFTP 文件类型映射。
fn ssh_entry_kind(file_type: SftpFileType) -> ConnectionFileEntryKind {
    if file_type.is_dir() {
        ConnectionFileEntryKind::Directory
    } else if file_type.is_file() {
        ConnectionFileEntryKind::File
    } else if file_type.is_symlink() {
        ConnectionFileEntryKind::Symlink
    } else {
        ConnectionFileEntryKind::Other
    }
}

/// 排序目录项：目录优先，名称升序。
fn sort_file_entries(entries: &mut [ConnectionFileEntry]) {
    entries.sort_by(|left, right| {
        let left_dir = left.kind.is_directory();
        let right_dir = right.kind.is_directory();
        right_dir
            .cmp(&left_dir)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
}

/// 拼接远端路径。
fn join_remote_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

/// 返回远端路径的文件名。
fn remote_file_name(path: &str) -> Option<String> {
    path.rsplit('/')
        .find(|segment| !segment.is_empty())
        .map(str::to_string)
}

/// 返回远端父路径。
fn remote_parent_path(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "/" || trimmed == "~" {
        return None;
    }
    let index = trimmed.rfind('/')?;
    if index == 0 {
        Some("/".to_string())
    } else {
        Some(trimmed[..index].to_string())
    }
}

/// SystemTime 转 Unix epoch 毫秒。
fn system_time_to_millis(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
}

/// 查找字节子串。
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// 查找 OSC 结束符，支持 BEL 和 ST。
fn find_osc_terminator(bytes: &[u8]) -> Option<(usize, usize)> {
    let bel = bytes.iter().position(|byte| *byte == 0x07);
    let st = find_bytes(bytes, b"\x1b\\");
    match (bel, st) {
        (Some(bel), Some(st)) if bel < st => Some((bel, 1)),
        (Some(_), Some(st)) => Some((st, 2)),
        (Some(bel), None) => Some((bel, 1)),
        (None, Some(st)) => Some((st, 2)),
        (None, None) => None,
    }
}

/// 解析 OSC 7 payload。
fn parse_osc7_payload(payload: &str) -> Option<String> {
    let uri = payload.strip_prefix("file://")?;
    let path_start = uri.find('/').unwrap_or(0);
    let path = &uri[path_start..];
    if path.is_empty() {
        return None;
    }
    percent_decode(path)
}

/// URL percent decode。
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = bytes.get(index + 1).copied().and_then(hex_value)?;
            let low = bytes.get(index + 2).copied().and_then(hex_value)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

/// 十六进制字符转数值。
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 发送文件事件。
fn send_file_event(sender: &Sender<ConnectionFileEvent>, event: ConnectionFileEvent) {
    let _ = sender.send(event);
}

/// 发送 Result 包装的文件事件。
fn send_file_result(
    sender: &Sender<ConnectionFileEvent>,
    result: Result<ConnectionFileEvent, String>,
) {
    match result {
        Ok(event) => send_file_event(sender, event),
        Err(error) => send_file_event(sender, ConnectionFileEvent::Error(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 为文件管理后端测试创建唯一临时目录。
    ///
    /// 业务意图：
    /// - 测试本地上传/下载时必须真实触发文件系统语义，尤其是 rename、覆盖和取消清理。
    /// - 目录名带进程号和时间戳，避免并发测试之间互相覆盖。
    fn file_manager_test_dir(name: &str) -> Result<PathBuf, std::io::Error> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "logclinic3-file-manager-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// 验证 OSC 7 BEL 结束符能解析并做 percent decode。
    #[test]
    fn osc7_bel_结束符解析当前目录() {
        let mut buffer = Vec::new();
        let cwd = update_osc7_cwd_from_output(
            &mut buffer,
            b"\x1b]7;file://host/Users/me/%E6%97%A5%E5%BF%97\x07",
        );
        assert_eq!(cwd.as_deref(), Some("/Users/me/日志"));
    }

    /// 验证 OSC 7 ST 结束符可跨输出 chunk 解析。
    #[test]
    fn osc7_st_结束符支持跨_chunk() {
        let mut buffer = Vec::new();
        assert!(update_osc7_cwd_from_output(&mut buffer, b"\x1b]7;file://host/tmp").is_none());
        let cwd = update_osc7_cwd_from_output(&mut buffer, b"/work\x1b\\");
        assert_eq!(cwd.as_deref(), Some("/tmp/work"));
    }

    /// 验证非 file scheme 不会污染当前目录。
    #[test]
    fn osc7_非_file_scheme_忽略() {
        let mut buffer = Vec::new();
        let cwd = update_osc7_cwd_from_output(&mut buffer, b"\x1b]7;ssh://host/tmp\x07");
        assert!(cwd.is_none());
    }

    /// 验证文件列表排序满足目录优先、名称升序。
    #[test]
    fn 文件列表排序目录优先且名称升序() {
        let mut entries = vec![
            ConnectionFileEntry {
                name: "b.txt".to_string(),
                path: "/b.txt".to_string(),
                kind: ConnectionFileEntryKind::File,
                size: Some(1),
                modified_at_ms: None,
            },
            ConnectionFileEntry {
                name: "a".to_string(),
                path: "/a".to_string(),
                kind: ConnectionFileEntryKind::Directory,
                size: None,
                modified_at_ms: None,
            },
        ];
        sort_file_entries(&mut entries);
        assert_eq!(entries[0].name, "a");
        assert_eq!(entries[1].name, "b.txt");
    }

    /// 验证删除能力只覆盖第一版确认过的安全范围。
    #[test]
    fn 文件类型删除能力只允许文件链接和空目录() {
        assert!(ConnectionFileEntryKind::File.is_deletable());
        assert!(ConnectionFileEntryKind::Symlink.is_deletable());
        assert!(ConnectionFileEntryKind::Directory.is_deletable());
        assert!(!ConnectionFileEntryKind::Other.is_deletable());
    }

    /// 验证取消传输走普通完成事件，避免 UI 把用户主动取消展示成错误。
    #[test]
    fn 取消传输事件使用完成状态() {
        let event = cancelled_transfer_event(ConnectionFileTransferOperation::Download);
        assert_eq!(
            event,
            ConnectionFileEvent::OperationFinished("已取消下载".to_string())
        );
    }

    /// 验证本地文件复制到自身目录时不会截断源文件。
    #[test]
    fn 本地同路径复制会跳过并保留原文件内容() -> Result<(), Box<dyn std::error::Error>> {
        let dir = file_manager_test_dir("same-file")?;
        let file = dir.join("same.txt");
        fs::write(&file, "原始内容")?;
        let (sender, _receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));

        let result = copy_to_local_dir(
            &[file.to_string_lossy().to_string()],
            &dir,
            ConnectionFileConflictPolicy::Overwrite,
            ConnectionFileTransferOperation::Download,
            &sender,
            &cancel,
        )
        .map_err(std::io::Error::other)?;

        assert_eq!(result.copied, 0);
        assert!(!result.cancelled);
        assert_eq!(fs::read_to_string(&file)?, "原始内容");
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    /// 验证用户取消覆盖传输时不会删除或截断已经存在的目标文件。
    #[test]
    fn 本地覆盖传输取消会保留旧目标文件() -> Result<(), Box<dyn std::error::Error>> {
        let dir = file_manager_test_dir("cancel-overwrite")?;
        let source = dir.join("source.txt");
        let dest = dir.join("target.txt");
        fs::write(&source, vec![b'a'; 128 * 1024])?;
        fs::write(&dest, "旧目标内容")?;
        let (sender, _receiver) = mpsc::channel();
        let mut progress = ConnectionFileProgressReporter::new(
            &sender,
            ConnectionFileTransferOperation::Download,
            1,
            128 * 1024,
        );
        let cancel = Arc::new(AtomicBool::new(true));

        let completed = copy_file_to_local_with_progress(&source, &dest, &mut progress, &cancel)
            .map_err(std::io::Error::other)?;

        assert!(!completed);
        assert_eq!(fs::read_to_string(&dest)?, "旧目标内容");
        fs::remove_dir_all(dir)?;
        Ok(())
    }
}
