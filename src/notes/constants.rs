// 笔记业务域常量。
//
// 业务意图：
// - 统一保存笔记物理目录名、旧 SQLite 文件名、内容上限等纯业务规则。
// - UI 布局尺寸不放在这里，避免顶层 notes 域反向依赖 GPUI。

/// 笔记物理文件根目录名。
///
/// 业务意图：
/// - 新版本把笔记存为真实目录和 Markdown 文件；根目录固定在应用配置目录下，方便用户备份和查看。
pub(crate) const NOTES_ROOT_DIR_NAME: &str = "notes";

/// Markdown 笔记默认扩展名。
///
/// 业务意图：
/// - 应用内新建笔记统一写 `.md`，扫描时仍兼容外部创建的 `.markdown` 文件。
pub(crate) const NOTE_MARKDOWN_EXTENSION: &str = "md";

/// 旧版本笔记数据库文件名。
///
/// 业务意图：
/// - 新版本不再运行时写入该文件，但首次启动会读取旧库迁移到物理目录，因此常量需要保留。
pub(crate) const NOTES_DATABASE_FILE_NAME: &str = "notes.db";

/// 旧版本笔记数据库首版 schema 版本。
///
/// 边界条件：
/// - 迁移逻辑只支持已知旧 schema；如果数据库版本来自未来版本，应返回中文错误并保留原文件。
pub(crate) const NOTES_DATABASE_SCHEMA_VERSION: i64 = 1;

/// 单篇笔记内容上限。
///
/// 业务意图：
/// - 第一版笔记编辑器按中小文本设计，会一次性加载正文并保存撤销快照；限制 1 MiB 可以避免极大内容拖慢 UI。
pub(crate) const NOTE_CONTENT_MAX_BYTES: usize = 1024 * 1024;
