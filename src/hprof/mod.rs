//! HPROF 分析功能域装配模块。
//!
//! 业务意图：
//! - 对外保持 `crate::hprof::*` 的既有入口和类型路径不变，让 UI、测试和调用方不感知内部文件拆分。
//! - 历史大文件实现已拆入 `core`、parser、sidecar、graph、dominator 和 thread details 等内部子模块，
//!   这里仅保留稳定的 crate 内重导出边界。
//!
//! 边界条件：
//! - 本文件只做模块边界装配和受控重导出，不改变 HPROF 解析、sidecar 缓存、错误文案或进度阶段。

mod core;

pub(crate) use self::core::*;
