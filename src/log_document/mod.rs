//! 日志文档业务功能域入口。
//!
//! 业务意图：
//! - 该目录集中维护日志读取、编码识别、行索引、分页读取和大小文件统一打开模型。
//! - `app` 只消费统一的 `LogTabDocument` 和解码/分页接口，避免 UI 层继续感知具体文件拆分。
//!
//! 关键约束：
//! - 小文件、超大文件、压缩包成员和编码切换语义保持迁移前一致。
//! - 行索引和分页读取仍只面向 crate 内部，不形成公开 API。

mod content;
mod large;
mod line_index;
mod paged;

pub(crate) use content::*;
pub(crate) use large::*;
pub(crate) use line_index::*;
pub(crate) use paged::*;
