//! 压缩包扫描模型和树节点工具。
//!
//! 业务意图：
//! - 将扫描入口需要返回的纯数据模型从格式扫描流程中拆出，避免 `scan.rs` 同时承载类型定义和所有格式分发。
//! - 这些类型不依赖 UI、日志目录树或 GPUI 状态，调用方可以按自身展示模型做适配。
//!
//! 边界条件：
//! - 成员路径必须由扫描流程完成安全归一化后才能写入节点；本文件只负责树结构组织，不重新解析原始路径。
//! - 树节点辅助方法限制在 `archive::scan` 内部使用，避免其它模块绕过扫描入口构造不完整来源。

use std::{
    error::Error,
    fmt::{self, Display},
    path::PathBuf,
};

use crate::archive::ArchiveFormat;

/// 扫描嵌套压缩包目录时允许读入内存的最大压缩包成员大小。
///
/// 业务意图：
/// - 外层压缩包里的内层压缩包必须先拿到可 seek 的字节才能读取目录；限制大小可以避免加载树阶段因巨大内层压缩包占满内存。
/// - 超过该上限时仍保留内层压缩包本身作为普通文件节点，用户可以按既有分页/物化路径打开单文件压缩包。
pub(crate) const NESTED_ARCHIVE_SCAN_MAX_BYTES: u64 = 200 * 1024 * 1024;

/// 压缩包扫描阶段返回给日志加载器的完整结果。
///
/// 业务意图：
/// - `children` 表示压缩包根节点下的安全扫描树，不包含 UI 根节点，便于日志加载器按当前来源名称包装。
/// - `error_count` 只统计非致命条目错误，致命错误仍通过 `ArchiveScanError` 返回。
/// - `temporary_paths` 记录 7Z 或嵌套 RAR 扫描时创建的临时路径，由上层统一清理。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveScanResult {
    /// 压缩包根节点下的子节点。
    pub(crate) children: Vec<ArchiveScanNode>,
    /// 扫描过程中转成错误节点的非致命错误数。
    pub(crate) error_count: usize,
    /// 当前扫描创建的临时文件或目录。
    pub(crate) temporary_paths: Vec<PathBuf>,
}

/// 与 UI 无关的压缩包扫描树节点。
///
/// 业务意图：
/// - `archive` 功能域只表达压缩包内部结构和成员来源，不直接依赖日志目录树展示类型。
/// - 日志加载器可以把该节点适配为左侧目录树，也可以在测试中直接断言扫描语义。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveScanNode {
    /// 节点显示名称，已经经过安全路径拆分。
    pub(crate) label: String,
    /// 节点业务类型。
    pub(crate) kind: ArchiveScanEntryKind,
    /// 文件大小、GZIP 类型或错误摘要等补充信息。
    pub(crate) meta: Option<String>,
    /// 非致命错误节点的详细说明。
    pub(crate) error_message: Option<String>,
    /// 文件节点的压缩包来源描述，目录和错误节点为 `None`。
    pub(crate) source: Option<ArchiveMemberSource>,
    /// 子节点列表。
    pub(crate) children: Vec<ArchiveScanNode>,
}

/// 压缩包扫描节点类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArchiveScanEntryKind {
    /// 压缩包内部目录或被展开的内层压缩包。
    Directory,
    /// 可打开的压缩包成员文件。
    File,
    /// 加密条目、非法路径或局部读取失败。
    Error,
}

/// 与 UI 无关的压缩包成员来源描述。
///
/// 业务意图：
/// - 该类型只描述如何重新定位压缩包成员，日志加载器再转换为统一的 `LogFileSource`。
/// - 这样 `archive` 可以独立测试压缩包语义，而不依赖日志目录树或 tab 去重模型。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ArchiveMemberSource {
    /// 顶层压缩包内部成员。
    Direct {
        /// 压缩包文件路径。
        archive_path: PathBuf,
        /// 压缩包格式。
        archive_format: ArchiveFormat,
        /// 安全归一化后的成员路径。
        member_path: String,
    },
    /// 已物化到临时目录的顶层压缩包成员。
    Materialized {
        /// 原始压缩包文件路径。
        archive_path: PathBuf,
        /// 原始压缩包格式。
        archive_format: ArchiveFormat,
        /// 安全归一化后的成员路径。
        member_path: String,
        /// 物化后的本地临时文件。
        temp_path: PathBuf,
    },
    /// 外层压缩包内的内层压缩包成员。
    Nested {
        /// 外层压缩包路径。
        outer_archive_path: PathBuf,
        /// 外层压缩包格式。
        outer_archive_format: ArchiveFormat,
        /// 外层压缩包里的内层压缩包成员路径。
        archive_member_path: String,
        /// 内层压缩包格式。
        nested_archive_format: ArchiveFormat,
        /// 内层压缩包里的目标成员路径。
        nested_member_path: String,
    },
}

/// 压缩包扫描阶段的致命错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArchiveScanError {
    /// 中文错误说明。
    message: String,
}

impl ArchiveScanError {
    /// 创建新的压缩包扫描错误。
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for ArchiveScanError {
    /// 将压缩包扫描错误格式化为中文文本。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ArchiveScanError {}

impl ArchiveScanNode {
    /// 创建普通扫描节点。
    pub(super) fn new(label: impl Into<String>, kind: ArchiveScanEntryKind) -> Self {
        Self {
            label: label.into(),
            kind,
            meta: None,
            error_message: None,
            source: None,
            children: Vec::new(),
        }
    }

    /// 创建错误扫描节点。
    pub(super) fn error(
        label: impl Into<String>,
        meta: impl Into<String>,
        error_message: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            kind: ArchiveScanEntryKind::Error,
            meta: Some(meta.into()),
            error_message: Some(error_message.into()),
            source: None,
            children: Vec::new(),
        }
    }

    /// 添加普通错误子节点。
    pub(super) fn add_error_child(
        &mut self,
        label: impl Into<String>,
        meta: impl Into<String>,
        error_message: impl Into<String>,
    ) {
        self.children
            .push(Self::error(label.into(), meta.into(), error_message.into()));
    }

    /// 添加与压缩包内部路径相关的错误节点。
    pub(super) fn add_archive_error_entry(
        &mut self,
        raw_name: &str,
        meta: impl Into<String>,
        error_message: impl Into<String>,
    ) {
        let label = if raw_name.trim().is_empty() {
            "空压缩包路径".to_string()
        } else {
            format!("非法条目：{}", raw_name)
        };
        self.add_error_child(label, meta, error_message);
    }

    /// 根据安全路径片段添加叶子节点，并自动补齐中间目录。
    pub(super) fn add_leaf_path(
        &mut self,
        segments: &[String],
        kind: ArchiveScanEntryKind,
        meta: Option<String>,
        source: Option<ArchiveMemberSource>,
        error_message: Option<String>,
    ) {
        if segments.is_empty() {
            return;
        }

        let mut current = self;
        for segment in &segments[..segments.len() - 1] {
            current = current.get_or_insert_child(segment, ArchiveScanEntryKind::Directory);
        }

        let leaf_label = &segments[segments.len() - 1];
        let leaf = current.get_or_insert_child(leaf_label, kind);
        leaf.meta = meta;
        leaf.source = source;
        leaf.error_message = error_message;
    }

    /// 在指定路径挂载已经构建好的内层压缩包子树。
    pub(super) fn add_subtree_path(&mut self, segments: &[String], subtree: ArchiveScanNode) {
        if segments.is_empty() {
            return;
        }

        let mut current = self;
        for segment in &segments[..segments.len() - 1] {
            current = current.get_or_insert_child(segment, ArchiveScanEntryKind::Directory);
        }

        let leaf_label = &segments[segments.len() - 1];
        let leaf = current.get_or_insert_child(leaf_label, ArchiveScanEntryKind::Directory);
        leaf.meta = subtree.meta;
        leaf.source = None;
        leaf.error_message = subtree.error_message;
        leaf.children.extend(subtree.children);
    }

    /// 查找或插入一个同名同类型子节点。
    fn get_or_insert_child(
        &mut self,
        label: &str,
        kind: ArchiveScanEntryKind,
    ) -> &mut ArchiveScanNode {
        if let Some(index) = self
            .children
            .iter()
            .position(|child| child.label == label && child.kind == kind)
        {
            return &mut self.children[index];
        }

        self.children.push(Self::new(label.to_string(), kind));
        let index = self.children.len() - 1;
        &mut self.children[index]
    }

    /// 递归统计可打开文件节点数量。
    pub(super) fn descendant_file_count(&self) -> usize {
        match self.kind {
            ArchiveScanEntryKind::File => 1,
            ArchiveScanEntryKind::Directory => {
                self.children.iter().map(Self::descendant_file_count).sum()
            }
            ArchiveScanEntryKind::Error => 0,
        }
    }
}
