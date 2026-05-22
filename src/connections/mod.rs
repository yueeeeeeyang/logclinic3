//! SSH 连接与终端业务功能域入口。
//!
//! 业务意图：
//! - 连接功能需要同时服务 SQLite 持久化、密码加密、SSH/SMB 后台会话和 GPUI 终端/文件展示，因此拆成独立业务域。
//! - 本模块不依赖 `app` 或 GPUI 类型，确保连接配置、加密和终端后端可以用纯单元测试覆盖。
//!
//! 跨平台约束：
//! - 目标平台仍是 macOS 和 Windows；密码主密钥通过系统安全存储保存，配置目录只保存 SQLite 密文。
//! - 本地 PTY 后端由独立“终端”主导航页使用，不写入连接数据库，避免本机 shell 生命周期和远程连接配置混淆。

mod constants;
mod crypto;
mod domain;
mod file_manager;
mod storage;
mod terminal;

pub(crate) use constants::*;
pub(crate) use crypto::*;
pub(crate) use domain::*;
pub(crate) use file_manager::*;
pub(crate) use storage::*;
pub(crate) use terminal::*;
