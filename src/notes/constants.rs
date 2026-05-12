// 笔记业务域常量。
//
// 业务意图：
// - 统一保存笔记 SQLite 文件名、schema 版本、内容上限等纯业务规则。
// - UI 布局尺寸不放在这里，避免顶层 notes 域反向依赖 GPUI。

use std::sync::atomic::AtomicU64;

/// 笔记数据库文件名。
///
/// 业务意图：
/// - 笔记和 AI 对话虽然都使用 SQLite，但两者生命周期、schema 迁移和数据恢复策略不同，因此使用独立数据库文件。
pub(crate) const NOTES_DATABASE_FILE_NAME: &str = "notes.db";

/// 笔记数据库首版 schema 版本。
///
/// 边界条件：
/// - 后续新增排序、移动、标签、附件等字段时必须提升版本并补充迁移逻辑。
pub(crate) const NOTES_DATABASE_SCHEMA_VERSION: i64 = 1;

/// 单篇笔记内容上限。
///
/// 业务意图：
/// - 第一版笔记编辑器按中小文本设计，会一次性加载正文并保存撤销快照；限制 1 MiB 可以避免极大内容拖慢 UI。
pub(crate) const NOTE_CONTENT_MAX_BYTES: usize = 1024 * 1024;

/// 笔记实体 ID 的进程内单调序号。
///
/// 业务意图：
/// - 目录和笔记 ID 只需要在本地 SQLite 中唯一；时间戳加原子序号可以规避低精度系统时钟导致的主键冲突。
pub(crate) static NOTE_ENTITY_ID_SEQUENCE: AtomicU64 = AtomicU64::new(1);
