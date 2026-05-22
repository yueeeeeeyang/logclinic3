// SSH 连接业务域常量。
//
// 业务意图：
// - 集中维护连接数据库、密码密文格式、系统凭据条目和终端默认尺寸，避免 UI 与后端各自硬编码。
// - 这些常量只描述连接功能内部协议，不形成公开 API；后续迁移通过 schema 版本和密文版本兼容。

use std::sync::atomic::AtomicU64;

/// 连接配置 SQLite 数据库文件名。
///
/// 业务意图：
/// - 连接列表包含可编辑配置和主机指纹，需要跨重启保存；使用独立数据库避免和 AI 对话历史互相影响迁移节奏。
pub(crate) const CONNECTIONS_DATABASE_FILE_NAME: &str = "connections.db";

/// 连接数据库 schema 版本。
///
/// 业务意图：
/// - SQLite `PRAGMA user_version` 用于检测未来版本和后续迁移；v2 在 SSH 连接表之外新增分类树。
pub(crate) const CONNECTIONS_DATABASE_SCHEMA_VERSION: i64 = 2;

/// SSH 密码密文格式版本前缀。
///
/// 业务意图：
/// - 密文写入 SQLite 后需要自描述版本，后续如果改用不同算法或系统保护策略，可以按前缀迁移。
pub(crate) const CONNECTION_PASSWORD_CIPHERTEXT_VERSION: &str = "v1";

/// AES-256-GCM 主密钥字节长度。
pub(crate) const CONNECTION_PASSWORD_MASTER_KEY_LEN: usize = 32;

/// AES-GCM nonce 字节长度。
pub(crate) const CONNECTION_PASSWORD_NONCE_LEN: usize = 12;

/// 系统安全存储中的 service 名称。
///
/// 跨平台约束：
/// - macOS Keychain 和 Windows 凭据管理器都需要 service/user 两段定位；service 使用产品和功能域组合，避免和模型配置等其它密钥冲突。
pub(crate) const CONNECTION_KEYRING_SERVICE: &str = "logclinic3.connections";

/// 系统安全存储中的主密钥 user 名称。
pub(crate) const CONNECTION_KEYRING_MASTER_KEY_USER: &str = "logclinic3-connections-master-key-v1";

/// 默认 SSH 端口。
pub(crate) const DEFAULT_SSH_PORT: u16 = 22;

/// 新终端默认列数。
///
/// 业务意图：
/// - 后台连接可能先于 GPUI 真实测量完成，使用 80x24 作为远程 pty 初始值，随后根据面板尺寸同步 resize。
pub(crate) const DEFAULT_TERMINAL_COLUMNS: u16 = 80;

/// 新终端默认行数。
pub(crate) const DEFAULT_TERMINAL_ROWS: u16 = 24;

/// 终端滚动回放行数。
///
/// 业务意图：
/// - 连接页以排障命令输出为主，保留 10000 行与 Alacritty 默认一致，避免短时间大量输出后无法复制上下文。
pub(crate) const TERMINAL_SCROLLBACK_LINES: usize = 10_000;

/// SSH 连接后台命令轮询间隔。
pub(crate) const TERMINAL_COMMAND_POLL_MILLIS: u64 = 12;

/// 连接和终端 tab 本地 ID 的进程内单调序号。
///
/// 业务意图：
/// - 连接 ID 需要在 SQLite 主键中稳定唯一；时间戳附加原子序号可以规避低精度系统时钟连续创建的冲突。
pub(crate) static CONNECTION_ENTITY_ID_SEQUENCE: AtomicU64 = AtomicU64::new(1);
