// SSH 连接终端后端实现。
//
// 业务意图：
// - 独立“终端”页使用本地 PTY，“连接”页使用远程 SSH channel；终端运行层必须抽象二者，
//   让渲染层、tab 状态和输入处理都只面对统一的 `TerminalBackend`。
// - 后台线程不直接读写 GPUI 状态，只通过标准库通道发送命令和事件，避免跨线程触碰 UI 生命周期。
//
// 跨平台约束：
// - 本地 PTY 使用 portable-pty，macOS 走系统 shell，Windows 走 PowerShell。
// - SSH 使用 russh 和 tokio 独立 runtime，避免把网络 IO 压到 GPUI 主线程。
// - 终端尺寸同时携带字符列行和像素大小，便于本地 PTY 与远程 PTY 都能按实际面板 resize。

use std::{
    io::{Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::Duration,
};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use russh::{
    ChannelMsg,
    client::{self, Handler},
    keys::{HashAlg, ssh_key},
};
use tokio::time::sleep;
use zeroize::Zeroizing;

use super::*;

/// 终端后端尺寸。
///
/// 业务意图：
/// - `cols`/`rows` 驱动终端模拟器和 PTY 行列；像素宽高传给 SSH remote pty，便于远端程序获取窗口尺寸。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalSize {
    /// 终端字符列数，保存入口保证至少为 1。
    pub(crate) cols: u16,
    /// 终端字符行数，保存入口保证至少为 1。
    pub(crate) rows: u16,
    /// 终端面板像素宽度；未知或未布局时为 0，远端通常仍以列数为准。
    pub(crate) pixel_width: u16,
    /// 终端面板像素高度；未知或未布局时为 0，远端通常仍以行数为准。
    pub(crate) pixel_height: u16,
}

impl Default for TerminalSize {
    /// 创建安全默认尺寸。
    fn default() -> Self {
        Self {
            cols: DEFAULT_TERMINAL_COLUMNS,
            rows: DEFAULT_TERMINAL_ROWS,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

#[allow(dead_code)]
impl TerminalSize {
    /// 根据 UI 计算结果构造终端尺寸。
    ///
    /// 边界条件：
    /// - GPUI 布局尚未完成时可能给出 0；这里夹到 1，避免 alacritty_terminal、PTY 和 SSH 请求收到非法尺寸。
    pub(crate) fn new(cols: u16, rows: u16, pixel_width: u16, pixel_height: u16) -> Self {
        Self {
            cols: cols.max(1),
            rows: rows.max(1),
            pixel_width,
            pixel_height,
        }
    }

    /// 转成 portable-pty 尺寸。
    fn to_portable_pty_size(self) -> PtySize {
        PtySize {
            rows: self.rows,
            cols: self.cols,
            pixel_width: self.pixel_width,
            pixel_height: self.pixel_height,
        }
    }
}

/// UI 发给终端后端的命令。
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalBackendCommand {
    /// 写入终端输入字节，可能来自键盘、粘贴或鼠标上报。
    Input(Vec<u8>),
    /// 同步终端尺寸变化。
    Resize(TerminalSize),
    /// 关闭终端会话；后端必须幂等处理重复关闭。
    Shutdown,
}

/// 终端后端发回 UI 的事件。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalBackendEvent {
    /// 后端连接和 shell 已准备好。
    Connected,
    /// 终端输出字节，UI 需要喂给 alacritty_terminal。
    Output(Vec<u8>),
    /// 后端正常结束，字符串是用户可见的简短结束原因。
    Exited(String),
    /// 后端错误，必须是中文且不能包含密码明文。
    Error(String),
    /// 首次连接到未知主机指纹，需要 UI 弹窗确认。
    HostKeyUnknown {
        /// 服务器本次返回的主机公钥指纹。
        fingerprint: String,
    },
    /// 已信任指纹和本次指纹不一致，必须阻止连接。
    HostKeyMismatch {
        /// 本地保存的可信指纹。
        expected: String,
        /// 服务器本次返回的主机公钥指纹。
        actual: String,
    },
}

/// 一个已经启动的终端后端句柄。
///
/// 业务意图：
/// - UI tab 持有句柄后只需要非阻塞轮询 `event_receiver`，所有写入和关闭都通过 `command_sender` 发送。
pub(crate) struct TerminalBackendHandle {
    /// UI 发给后端的命令通道。
    pub(crate) command_sender: Sender<TerminalBackendCommand>,
    /// 后端发给 UI 的事件通道。
    pub(crate) event_receiver: Receiver<TerminalBackendEvent>,
}

impl TerminalBackendHandle {
    /// 请求关闭后端，发送失败说明后台线程已经退出，可视为关闭完成。
    pub(crate) fn shutdown(&self) {
        let _ = self.command_sender.send(TerminalBackendCommand::Shutdown);
    }
}

/// 终端后端统一抽象。
///
/// 业务意图：
/// - SSH channel 和本地 PTY 的启动参数完全不同，但 UI 只关心输入、resize、关闭和输出事件。
pub(crate) trait TerminalBackend {
    /// 启动后端线程并返回命令/事件句柄。
    fn start(self) -> TerminalBackendHandle;
}

/// 本地 PTY 后端。
///
/// 业务意图：
/// - 独立“终端”主导航页直接启动该后端，不保存连接配置、不走 SSH 主机指纹流程。
/// - 连接页仅保留 SSH/SMB 等外部连接入口，但文件管理和测试仍会复用同一后端抽象。
pub(crate) struct LocalPtyBackend {
    /// 启动时申请的终端尺寸。
    size: TerminalSize,
}

impl LocalPtyBackend {
    /// 创建本地 PTY 后端。
    pub(crate) fn new(size: TerminalSize) -> Self {
        Self { size }
    }
}

impl TerminalBackend for LocalPtyBackend {
    /// 启动本地 PTY。
    fn start(self) -> TerminalBackendHandle {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::spawn(move || run_local_pty_backend(self.size, command_receiver, event_sender));
        TerminalBackendHandle {
            command_sender,
            event_receiver,
        }
    }
}

/// SSH 远程终端后端。
///
/// 业务意图：
/// - 使用 russh 建立密码认证 session，申请 `xterm-256color` remote pty 后启动用户默认 shell。
pub(crate) struct SshTerminalBackend {
    /// 连接配置快照；后台线程只读取快照，避免 UI 编辑配置时影响已打开 tab。
    profile: ConnectionProfile,
    /// 解密后的密码明文；使用 `Zeroizing` 缩短认证失败、主机指纹阻断等路径上的明文残留时间。
    password: Zeroizing<String>,
    /// 当前保存的可信主机指纹；`None` 表示需要首次信任确认。
    trusted_fingerprint: Option<String>,
    /// 启动时申请的终端尺寸。
    size: TerminalSize,
}

impl SshTerminalBackend {
    /// 创建 SSH 终端后端。
    pub(crate) fn new(
        profile: ConnectionProfile,
        password: String,
        trusted_fingerprint: Option<String>,
        size: TerminalSize,
    ) -> Self {
        Self {
            profile,
            password: Zeroizing::new(password),
            trusted_fingerprint,
            size,
        }
    }
}

impl TerminalBackend for SshTerminalBackend {
    /// 启动 SSH 后端线程。
    fn start(self) -> TerminalBackendHandle {
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        thread::spawn(move || run_ssh_backend(self, command_receiver, event_sender));
        TerminalBackendHandle {
            command_sender,
            event_receiver,
        }
    }
}

/// 运行本地 PTY 后端。
#[allow(dead_code)]
fn run_local_pty_backend(
    size: TerminalSize,
    command_receiver: Receiver<TerminalBackendCommand>,
    event_sender: Sender<TerminalBackendEvent>,
) {
    let pty_system = native_pty_system();
    let pair = match pty_system.openpty(size.to_portable_pty_size()) {
        Ok(pair) => pair,
        Err(error) => {
            send_terminal_event(
                &event_sender,
                TerminalBackendEvent::Error(format!("启动本地终端失败：{error}")),
            );
            return;
        }
    };

    let command = default_local_shell_command();
    let mut child = match pair.slave.spawn_command(command) {
        Ok(child) => child,
        Err(error) => {
            send_terminal_event(
                &event_sender,
                TerminalBackendEvent::Error(format!("启动本地 shell 失败：{error}")),
            );
            return;
        }
    };
    drop(pair.slave);

    let mut reader = match pair.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(error) => {
            send_terminal_event(
                &event_sender,
                TerminalBackendEvent::Error(format!("读取本地终端输出失败：{error}")),
            );
            let _ = child.kill();
            return;
        }
    };
    let mut writer = match pair.master.take_writer() {
        Ok(writer) => writer,
        Err(error) => {
            send_terminal_event(
                &event_sender,
                TerminalBackendEvent::Error(format!("打开本地终端输入失败：{error}")),
            );
            let _ = child.kill();
            return;
        }
    };

    let reader_event_sender = event_sender.clone();
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => send_terminal_event(
                    &reader_event_sender,
                    TerminalBackendEvent::Output(buffer[..count].to_vec()),
                ),
                Err(error) => {
                    send_terminal_event(
                        &reader_event_sender,
                        TerminalBackendEvent::Error(format!("读取本地终端输出失败：{error}")),
                    );
                    break;
                }
            }
        }
    });

    send_terminal_event(&event_sender, TerminalBackendEvent::Connected);
    let mut shutdown_requested = false;
    loop {
        match command_receiver.recv_timeout(Duration::from_millis(TERMINAL_COMMAND_POLL_MILLIS)) {
            Ok(TerminalBackendCommand::Input(bytes)) => {
                if let Err(error) = writer.write_all(&bytes) {
                    send_terminal_event(
                        &event_sender,
                        TerminalBackendEvent::Error(format!("写入本地终端失败：{error}")),
                    );
                    break;
                }
                let _ = writer.flush();
            }
            Ok(TerminalBackendCommand::Resize(next_size)) => {
                if let Err(error) = pair.master.resize(next_size.to_portable_pty_size()) {
                    send_terminal_event(
                        &event_sender,
                        TerminalBackendEvent::Error(format!("调整本地终端尺寸失败：{error}")),
                    );
                }
            }
            Ok(TerminalBackendCommand::Shutdown) => {
                if !shutdown_requested {
                    shutdown_requested = true;
                    let _ = child.kill();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                break;
            }
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                send_terminal_event(
                    &event_sender,
                    TerminalBackendEvent::Exited(format!("本地终端已退出：{status}")),
                );
                break;
            }
            Ok(None) => {}
            Err(error) => {
                send_terminal_event(
                    &event_sender,
                    TerminalBackendEvent::Error(format!("检查本地终端状态失败：{error}")),
                );
                break;
            }
        }

        if shutdown_requested {
            send_terminal_event(
                &event_sender,
                TerminalBackendEvent::Exited("本地终端已关闭".to_string()),
            );
            break;
        }
    }
}

/// 运行 SSH 后端。
fn run_ssh_backend(
    backend: SshTerminalBackend,
    command_receiver: Receiver<TerminalBackendCommand>,
    event_sender: Sender<TerminalBackendEvent>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            send_terminal_event(
                &event_sender,
                TerminalBackendEvent::Error(format!("启动 SSH 运行时失败：{error}")),
            );
            return;
        }
    };

    runtime.block_on(async move {
        if let Err(error) =
            run_ssh_backend_async(backend, command_receiver, event_sender.clone()).await
        {
            send_terminal_event(&event_sender, TerminalBackendEvent::Error(error));
        }
    });
}

/// 异步执行 SSH 连接、认证、PTY 和事件转发。
async fn run_ssh_backend_async(
    mut backend: SshTerminalBackend,
    command_receiver: Receiver<TerminalBackendCommand>,
    event_sender: Sender<TerminalBackendEvent>,
) -> Result<(), String> {
    let host_key_blocked = Arc::new(AtomicBool::new(false));
    let handler = SshClientHandler {
        trusted_fingerprint: backend.trusted_fingerprint.clone(),
        event_sender: event_sender.clone(),
        host_key_blocked: Arc::clone(&host_key_blocked),
    };

    let mut config = client::Config::default();
    config.inactivity_timeout = Some(Duration::from_secs(60 * 60 * 24));
    config.keepalive_interval = Some(Duration::from_secs(30));
    config.keepalive_max = 3;

    let address = (backend.profile.host.as_str(), backend.profile.port);
    let mut session = match client::connect(Arc::new(config), address, handler).await {
        Ok(session) => session,
        Err(_error) if host_key_blocked.load(Ordering::SeqCst) => {
            return Ok(());
        }
        Err(error) => return Err(format!("连接 SSH 服务器失败：{error}")),
    };

    // 认证前先把密码从后端快照中移出，避免认证错误路径继续在 `backend` 中保留一份明文。
    // russh 当前 API 内部会接收普通 String；这里至少保证本模块持有的副本在认证结束后立即清零。
    let password = std::mem::replace(&mut backend.password, Zeroizing::new(String::new()));
    let authenticated = session
        .authenticate_password(backend.profile.username.clone(), password.as_str())
        .await
        .map_err(|error| format!("SSH 密码认证失败：{error}"))?;
    drop(password);
    if !authenticated.success() {
        return Err("SSH 密码认证失败：用户名或密码不正确".to_string());
    }

    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|error| format!("打开 SSH 会话失败：{error}"))?;
    channel
        .request_pty(
            false,
            "xterm-256color",
            u32::from(backend.size.cols),
            u32::from(backend.size.rows),
            u32::from(backend.size.pixel_width),
            u32::from(backend.size.pixel_height),
            &[],
        )
        .await
        .map_err(|error| format!("申请远程终端失败：{error}"))?;
    // 远端 shell 的中文输出取决于启动时的 locale。通过 SSH env request 主动声明 UTF-8，
    // 能覆盖不少默认 `C` locale 的服务器；请求不需要回复，避免服务端禁用 AcceptEnv 时阻塞连接流程。
    for (name, value) in ssh_utf8_environment_pairs() {
        channel
            .set_env(false, name, value)
            .await
            .map_err(|error| format!("设置远程终端字符集失败：{error}"))?;
    }
    channel
        .request_shell(true)
        .await
        .map_err(|error| format!("启动远程 shell 失败：{error}"))?;

    send_terminal_event(&event_sender, TerminalBackendEvent::Connected);
    loop {
        tokio::select! {
            channel_message = channel.wait() => {
                match channel_message {
                    Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        send_terminal_event(&event_sender, TerminalBackendEvent::Output(data.to_vec()));
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        send_terminal_event(
                            &event_sender,
                            TerminalBackendEvent::Exited(format!("远程终端已退出，状态码：{exit_status}")),
                        );
                        break;
                    }
                    Some(ChannelMsg::ExitSignal { signal_name, error_message, .. }) => {
                        let message = if error_message.is_empty() {
                            format!("远程终端被信号终止：{signal_name:?}")
                        } else {
                            format!("远程终端被信号终止：{signal_name:?}，{error_message}")
                        };
                        send_terminal_event(&event_sender, TerminalBackendEvent::Exited(message));
                        break;
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                        send_terminal_event(&event_sender, TerminalBackendEvent::Exited("远程终端已关闭".to_string()));
                        break;
                    }
                    Some(_) => {}
                }
            }
            _ = sleep(Duration::from_millis(TERMINAL_COMMAND_POLL_MILLIS)) => {
                if !drain_ssh_backend_commands(&channel, &command_receiver, &event_sender).await? {
                    break;
                }
            }
        }
    }

    Ok(())
}

/// 消费 UI 发来的 SSH 终端命令。
async fn drain_ssh_backend_commands(
    channel: &russh::Channel<client::Msg>,
    command_receiver: &Receiver<TerminalBackendCommand>,
    event_sender: &Sender<TerminalBackendEvent>,
) -> Result<bool, String> {
    loop {
        match command_receiver.try_recv() {
            Ok(TerminalBackendCommand::Input(bytes)) => {
                channel
                    .data_bytes(bytes)
                    .await
                    .map_err(|error| format!("写入远程终端失败：{error}"))?;
            }
            Ok(TerminalBackendCommand::Resize(size)) => {
                channel
                    .window_change(
                        u32::from(size.cols),
                        u32::from(size.rows),
                        u32::from(size.pixel_width),
                        u32::from(size.pixel_height),
                    )
                    .await
                    .map_err(|error| format!("调整远程终端尺寸失败：{error}"))?;
            }
            Ok(TerminalBackendCommand::Shutdown) => {
                let _ = channel.close().await;
                send_terminal_event(
                    event_sender,
                    TerminalBackendEvent::Exited("远程终端已关闭".to_string()),
                );
                return Ok(false);
            }
            Err(TryRecvError::Empty) => return Ok(true),
            Err(TryRecvError::Disconnected) => {
                let _ = channel.close().await;
                return Ok(false);
            }
        }
    }
}

/// SSH client handler，只负责主机指纹校验。
struct SshClientHandler {
    /// 本地保存的可信指纹。
    trusted_fingerprint: Option<String>,
    /// 向 UI 报告首次信任或不匹配事件的通道。
    event_sender: Sender<TerminalBackendEvent>,
    /// 标记连接失败是否由主机指纹阻断导致，用于抑制重复错误消息。
    host_key_blocked: Arc<AtomicBool>,
}

impl Handler for SshClientHandler {
    type Error = russh::Error;

    /// 校验服务器公钥指纹。
    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fingerprint = server_public_key_fingerprint(server_public_key);
        match verify_connection_host_key(self.trusted_fingerprint.as_deref(), &fingerprint) {
            HostKeyVerification::Trusted => Ok(true),
            HostKeyVerification::Unknown { actual } => {
                self.host_key_blocked.store(true, Ordering::SeqCst);
                send_terminal_event(
                    &self.event_sender,
                    TerminalBackendEvent::HostKeyUnknown {
                        fingerprint: actual,
                    },
                );
                Ok(false)
            }
            HostKeyVerification::Mismatch { expected, actual } => {
                self.host_key_blocked.store(true, Ordering::SeqCst);
                send_terminal_event(
                    &self.event_sender,
                    TerminalBackendEvent::HostKeyMismatch { expected, actual },
                );
                Ok(false)
            }
        }
    }
}

/// 计算 russh 公钥指纹。
fn server_public_key_fingerprint(public_key: &ssh_key::PublicKey) -> String {
    public_key.fingerprint(HashAlg::Sha256).to_string()
}

/// 构造当前平台默认本地 shell 命令。
fn default_local_shell_command() -> CommandBuilder {
    let mut command = {
        #[cfg(target_os = "windows")]
        {
            CommandBuilder::new("powershell.exe")
        }
        #[cfg(not(target_os = "windows"))]
        {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
            CommandBuilder::new(shell)
        }
    };
    configure_local_shell_environment(&mut command);
    command
}

/// 配置本地 shell 的终端环境。
///
/// 业务意图：
/// - macOS 从 GUI 启动时经常缺少 `LANG`/`LC_CTYPE`，`ls` 等命令会按 C locale 输出，中文文件名会在源头变成 `?`。
/// - 这里保证本地 PTY 至少拥有 UTF-8 locale 和 `xterm-256color`，让 shell 输出 UTF-8 字节，再交给 alacritty_terminal 解码。
///
/// 跨平台约束：
/// - Windows PowerShell 对 `LANG` 不敏感，但部分跨平台 CLI 会读取该变量；同时设置 Python UTF-8 变量，减少脚本输出乱码。
fn configure_local_shell_environment(command: &mut CommandBuilder) {
    command.env("TERM", "xterm-256color");

    let locale = preferred_utf8_locale();
    command.env("LANG", locale.as_str());
    command.env("LC_CTYPE", locale.as_str());
    if std::env::var("LC_ALL")
        .ok()
        .is_some_and(|value| !locale_is_utf8(&value))
    {
        command.env("LC_ALL", locale.as_str());
    }

    #[cfg(target_os = "windows")]
    {
        command.env("PYTHONUTF8", "1");
        command.env("PYTHONIOENCODING", "utf-8");
    }
}

/// 返回 SSH 远程 shell 启动前需要声明的 UTF-8 环境变量。
///
/// 业务意图：
/// - SSH 终端没有本地进程环境继承链，远端默认 locale 可能是 `C`，会导致 `ls`、`grep` 等命令直接输出问号。
/// - 这里只声明影响字符编码的最小变量，不覆盖 `LC_ALL`，避免过度改变远端日期、排序和数字格式。
fn ssh_utf8_environment_pairs() -> [(&'static str, String); 2] {
    let locale = preferred_remote_utf8_locale();
    [("LANG", locale.clone()), ("LC_CTYPE", locale)]
}

/// 返回 SSH 远端优先使用的 UTF-8 locale。
///
/// 边界条件：
/// - 如果本地进程已经有 UTF-8 locale，优先沿用，减少用户对远端日期、排序和本地化输出的意外变化。
/// - GUI 启动时常常没有 locale，此时 SSH 远端更可能是 Linux 服务器，`C.UTF-8` 比 `en_US.UTF-8` 更少依赖额外语言包。
fn preferred_remote_utf8_locale() -> String {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| locale_is_utf8(value))
        .unwrap_or_else(|| "C.UTF-8".to_string())
}

/// 返回优先使用的 UTF-8 locale。
///
/// 边界条件：
/// - 如果用户环境已经有 UTF-8 locale，保留原值，避免改变日期、数字和排序习惯。
/// - 如果没有可用值，回退到 `en_US.UTF-8`，该值在 macOS 上稳定可用，也能满足中文文件名的编码需求。
fn preferred_utf8_locale() -> String {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| locale_is_utf8(value))
        .unwrap_or_else(|| "en_US.UTF-8".to_string())
}

/// 判断 locale 文本是否声明 UTF-8 编码。
fn locale_is_utf8(locale: &str) -> bool {
    let upper = locale.to_ascii_uppercase();
    upper.contains("UTF-8") || upper.contains("UTF8")
}

/// 判断命令环境变量是否等于指定文本。
#[cfg(test)]
fn command_env_equals(command: &CommandBuilder, key: &str, value: &str) -> bool {
    command
        .get_env(key)
        .is_some_and(|actual| actual == std::ffi::OsStr::new(value))
}

/// 发送终端事件；如果 UI tab 已关闭，发送失败可以安全忽略。
fn send_terminal_event(sender: &Sender<TerminalBackendEvent>, event: TerminalBackendEvent) {
    let _ = sender.send(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证终端尺寸不会生成 0 行或 0 列。
    ///
    /// 业务风险：
    /// - UI 初次布局尚未完成时如果把 0 行列传给后端，远端 pty 或终端模拟器可能直接失败。
    #[test]
    fn 终端尺寸会夹到至少一行一列() {
        let size = TerminalSize::new(0, 0, 0, 0);
        assert_eq!(size.cols, 1);
        assert_eq!(size.rows, 1);
    }

    /// 验证主机指纹事件可以通过后端事件通道传递。
    #[test]
    fn 终端后端事件可以传递主机指纹状态() {
        let (sender, receiver) = mpsc::channel();
        send_terminal_event(
            &sender,
            TerminalBackendEvent::HostKeyUnknown {
                fingerprint: "SHA256:test".to_string(),
            },
        );
        assert_eq!(
            receiver.try_recv().unwrap(),
            TerminalBackendEvent::HostKeyUnknown {
                fingerprint: "SHA256:test".to_string(),
            }
        );
    }

    /// 验证本地 shell 默认使用 UTF-8 环境。
    ///
    /// 业务风险：
    /// - macOS GUI 启动缺少 locale 时，本地 `ls` 会把中文文件名直接替换成问号，渲染层无法再恢复。
    #[test]
    fn 本地_shell_命令会注入_utf8_locale() {
        let command = default_local_shell_command();

        assert!(command_env_equals(&command, "TERM", "xterm-256color"));
        assert!(
            command
                .get_env("LANG")
                .and_then(|value| value.to_str())
                .is_some_and(locale_is_utf8)
        );
        assert!(
            command
                .get_env("LC_CTYPE")
                .and_then(|value| value.to_str())
                .is_some_and(locale_is_utf8)
        );
    }

    /// 验证 SSH 会话在启动 shell 前声明 UTF-8 字符集。
    ///
    /// 业务风险：
    /// - 如果只修本地 PTY，不修 SSH env request，远端默认 `C` locale 时中文仍会从源头变成问号。
    #[test]
    fn ssh_终端会声明_utf8_locale() {
        let pairs = ssh_utf8_environment_pairs();

        assert_eq!(pairs[0].0, "LANG");
        assert_eq!(pairs[1].0, "LC_CTYPE");
        assert!(locale_is_utf8(&pairs[0].1));
        assert!(locale_is_utf8(&pairs[1].1));
    }

    /// 验证 UTF-8 locale 判断覆盖常见写法。
    #[test]
    fn locale_utf8_判断支持连字符和紧凑写法() {
        assert!(locale_is_utf8("zh_CN.UTF-8"));
        assert!(locale_is_utf8("en_US.utf8"));
        assert!(!locale_is_utf8("C"));
    }
}
