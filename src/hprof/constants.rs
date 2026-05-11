//! HPROF 常量和低内存索引类型。
//!
//! 业务意图：
//! - 集中维护 HPROF 标签、字段类型、sidecar schema 和 dominator 低内存索引常量。
//! - parser、graph、dominator 和 sidecar 共享这些值，避免跨模块复制协议数字。
//!
//! 边界条件：
//! - 这些常量是解析协议和缓存兼容性边界，修改 sidecar 或 MAT 语义相关值必须同步更新测试。

/// HPROF 对象 ID。
///
/// 业务意图：
/// - JVM HPROF 支持 4 字节或 8 字节对象 ID；内部统一提升为 `u64`，避免 UI 和算法层反复分支。
pub(crate) type HprofObjectId = u64;
/// dominator 内部连续节点编号。
///
/// 业务意图：
/// - 5GB dump 中对象和边数量很大，LT 算法会为每个节点维护多组数组；用 `u32` 替代 `usize` 可以把这些热数组内存减半。
pub(crate) type HprofNodeId = u32;
/// `u32` 节点数组中的无效哨兵值。
pub(crate) const HPROF_INVALID_NODE: HprofNodeId = HprofNodeId::MAX;

/// 第一版 dominator 结果默认展示的 retained size Top N。
///
/// 业务意图：
/// - 完整 dominator tree 可能包含数百万对象，首屏只展示 Top retained 节点，再允许用户展开直接支配子节点。
pub(crate) const HPROF_TOP_DOMINATOR_LIMIT: usize = 50;

/// HPROF 文件头允许的最大字符串长度。
///
/// 边界条件：
/// - 标准 header 通常是 `JAVA PROFILE 1.0.2`；超过 1 KiB 基本可以视为损坏或非 HPROF 文件。
pub(crate) const HPROF_HEADER_MAX_BYTES: usize = 1024;

/// 解析进度节流用的对象/子记录批量大小。
///
/// 业务意图：
/// - 大 dump 中对象数可能非常多，如果每个对象都通知 UI，会让锁竞争和窗口刷新吞掉解析性能。
pub(crate) const HPROF_PROGRESS_RECORD_INTERVAL: u64 = 2048;
/// Dominator 和图构建阶段进度节流用的节点/边批量大小。
///
/// 业务意图：
/// - 计算阶段需要可观察，但不能让每条边都跨线程更新进度；约 6.5 万的批量能在大图上保持 UI 有反馈且开销可控。
pub(crate) const HPROF_PROGRESS_WORK_INTERVAL: usize = 65_536;
/// MAT 兼容 size model 中判断 compressed oops 的默认堆大小上限。
///
/// 实现原因：
/// - HPROF header 不可靠携带 HotSpot 压缩指针配置；MAT 常见场景会在 32GiB 以下使用 4 字节普通对象引用。
pub(crate) const HPROF_COMPRESSED_OOPS_HEAP_LIMIT: u64 = 32 * 1024 * 1024 * 1024;
/// MAT 兼容 size model 的默认对象对齐。
pub(crate) const HPROF_MAT_OBJECT_ALIGNMENT: u64 = 8;
/// MAT 兼容 size model 的默认普通对象头大小。
pub(crate) const HPROF_MAT_OBJECT_HEADER_SIZE: u64 = 12;
/// MAT 兼容 size model 的默认数组对象头大小。
pub(crate) const HPROF_MAT_ARRAY_HEADER_SIZE: u64 = 16;
/// 用于 `Thread.name` fallback 的最大字符串数组长度。
///
/// 业务意图：
/// - START_THREAD 通常已经包含线程名；这里仅为少数缺失记录提供兜底，限制长度可以避免把业务大 byte array 作为字符串缓存。
pub(crate) const HPROF_THREAD_STRING_FALLBACK_MAX_ELEMENTS: u32 = 4096;
/// HPROF sidecar 缓存 schema 版本。
///
/// 业务意图：
/// - sidecar 是跨进程、跨版本复用的磁盘格式；任何二进制布局、MAT 语义或结果字段变化都必须 bump 版本，避免误读旧索引。
pub(crate) const HPROF_SIDECAR_SCHEMA_VERSION: u32 = 3;
/// HPROF MAT 兼容语义版本。
///
/// 业务意图：
/// - 即使磁盘字段布局不变，只要 Reference 语义、ClassLoader 合成边或 shallow size model 发生变化，也必须让旧缓存失效。
pub(crate) const HPROF_MAT_SEMANTICS_VERSION: u32 = 1;
/// 源 dump 首尾快速校验块大小。
pub(crate) const HPROF_SIDECAR_CHECKSUM_BYTES: u64 = 1024 * 1024;
/// 大 dump 缓存目录不可写时触发硬错误的阈值。
pub(crate) const HPROF_SIDECAR_REQUIRED_BYTES: u64 = 1024 * 1024 * 1024;
/// sidecar v3 写入缓冲区大小。
///
/// 业务意图：
/// - 5GB dump 的结果索引可能包含数百万对象和子节点记录；大缓冲可以显著降低 macOS/Windows 上的系统调用次数，
///   避免最后“写入索引”阶段因为碎片化小写入拖慢。
pub(crate) const HPROF_SIDECAR_WRITER_BUFFER_BYTES: usize = 32 * 1024 * 1024;
/// sidecar v3 批量编码记录数。
///
/// 边界条件：
/// - chunk 太小会回到频繁写入，太大又会拉高写索引阶段的瞬时内存；65,536 条与已有进度节流保持一致。
pub(crate) const HPROF_SIDECAR_RECORD_CHUNK: usize = 65_536;

/// HPROF 顶层记录：UTF8 字符串。
pub(crate) const TAG_STRING_IN_UTF8: u8 = 0x01;
/// HPROF 顶层记录：类加载信息。
pub(crate) const TAG_LOAD_CLASS: u8 = 0x02;
/// HPROF 顶层记录：线程栈帧。
pub(crate) const TAG_STACK_FRAME: u8 = 0x04;
/// HPROF 顶层记录：线程栈轨迹。
pub(crate) const TAG_STACK_TRACE: u8 = 0x05;
/// HPROF 顶层记录：线程启动信息。
pub(crate) const TAG_START_THREAD: u8 = 0x0A;
/// HPROF 顶层记录：线程结束信息。
pub(crate) const TAG_END_THREAD: u8 = 0x0B;
/// HPROF 顶层记录：完整 heap dump。
pub(crate) const TAG_HEAP_DUMP: u8 = 0x0C;
/// HPROF 顶层记录：分段 heap dump。
pub(crate) const TAG_HEAP_DUMP_SEGMENT: u8 = 0x1C;
/// HPROF 顶层记录：heap dump 结束标记。
pub(crate) const TAG_HEAP_DUMP_END: u8 = 0x2C;

/// HPROF 子记录：未知 GC Root。
pub(crate) const SUB_ROOT_UNKNOWN: u8 = 0xFF;
/// HPROF 子记录：JNI Global Root。
pub(crate) const SUB_ROOT_JNI_GLOBAL: u8 = 0x01;
/// HPROF 子记录：JNI Local Root。
pub(crate) const SUB_ROOT_JNI_LOCAL: u8 = 0x02;
/// HPROF 子记录：Java Frame Root。
pub(crate) const SUB_ROOT_JAVA_FRAME: u8 = 0x03;
/// HPROF 子记录：Native Stack Root。
pub(crate) const SUB_ROOT_NATIVE_STACK: u8 = 0x04;
/// HPROF 子记录：System Class Root。
pub(crate) const SUB_ROOT_STICKY_CLASS: u8 = 0x05;
/// HPROF 子记录：Thread Block Root。
pub(crate) const SUB_ROOT_THREAD_BLOCK: u8 = 0x06;
/// HPROF 子记录：Monitor Used Root。
pub(crate) const SUB_ROOT_MONITOR_USED: u8 = 0x07;
/// HPROF 子记录：Thread Object Root。
pub(crate) const SUB_ROOT_THREAD_OBJECT: u8 = 0x08;
/// HPROF 子记录：Class Dump。
pub(crate) const SUB_CLASS_DUMP: u8 = 0x20;
/// HPROF 子记录：Instance Dump。
pub(crate) const SUB_INSTANCE_DUMP: u8 = 0x21;
/// HPROF 子记录：Object Array Dump。
pub(crate) const SUB_OBJECT_ARRAY_DUMP: u8 = 0x22;
/// HPROF 子记录：Primitive Array Dump。
pub(crate) const SUB_PRIMITIVE_ARRAY_DUMP: u8 = 0x23;
/// HPROF 子记录：Heap Dump Info。
pub(crate) const SUB_HEAP_DUMP_INFO: u8 = 0xFE;
/// HPROF 子记录：Primitive Array No Data。
pub(crate) const SUB_PRIMITIVE_ARRAY_NODATA: u8 = 0xC3;

/// HPROF 字段类型：对象引用。
pub(crate) const FIELD_TYPE_OBJECT: u8 = 2;
/// HPROF 字段类型：boolean。
pub(crate) const FIELD_TYPE_BOOLEAN: u8 = 4;
/// HPROF 字段类型：char。
pub(crate) const FIELD_TYPE_CHAR: u8 = 5;
/// HPROF 字段类型：float。
pub(crate) const FIELD_TYPE_FLOAT: u8 = 6;
/// HPROF 字段类型：double。
pub(crate) const FIELD_TYPE_DOUBLE: u8 = 7;
/// HPROF 字段类型：byte。
pub(crate) const FIELD_TYPE_BYTE: u8 = 8;
/// HPROF 字段类型：short。
pub(crate) const FIELD_TYPE_SHORT: u8 = 9;
/// HPROF 字段类型：int。
pub(crate) const FIELD_TYPE_INT: u8 = 10;
/// HPROF 字段类型：long。
pub(crate) const FIELD_TYPE_LONG: u8 = 11;
