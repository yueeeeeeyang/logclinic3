//! HPROF 中文错误类型。
//!
//! 业务意图：
//! - UI 需要展示可理解的中文错误；解析、sidecar 和 dominator 模块统一返回该错误类型。
//! - 可恢复错误必须显式传播，避免后台任务 panic 或把底层英文 I/O 错误直接暴露给用户。
//!
//! 边界条件：
//! - `Canceled` 是用户主动取消，不应包装成普通失败；调用方可以据此保持窗口状态。

use std::{
    error::Error,
    fmt::{self, Display},
    io,
};

/// HPROF 解析错误。
///
/// 业务意图：
/// - UI 需要给用户中文可理解错误，不应直接暴露底层 `io::ErrorKind` 或二进制偏移的英文 panic。
#[derive(Debug)]
pub(crate) enum HprofError {
    /// 文件系统或读取错误。
    Io(String),
    /// 用户选择或输入不符合要求。
    InvalidInput(String),
    /// 文件内容不是合法 HPROF。
    InvalidFormat(String),
    /// 第一版尚未支持的 HPROF 记录。
    Unsupported(String),
    /// 用户主动取消。
    Canceled,
}

impl Display for HprofError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message)
            | Self::InvalidInput(message)
            | Self::InvalidFormat(message)
            | Self::Unsupported(message) => formatter.write_str(message),
            Self::Canceled => formatter.write_str("HPROF 解析已取消"),
        }
    }
}

impl Error for HprofError {}

impl From<io::Error> for HprofError {
    fn from(error: io::Error) -> Self {
        Self::Io(format!("读取 HPROF 文件失败：{error}"))
    }
}
