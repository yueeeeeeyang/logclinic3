//! HPROF 二进制解析子模块。
//!
//! 业务意图：
//! - 该模块集中处理 HPROF header、顶层记录、heap dump 子记录、类字段布局和实例引用物化。
//! - 父模块保留对象图和结果结构，解析器只负责把用户选择的 dump 转换为内部对象图。
//!
//! 边界条件：
//! - 大文件优先使用只读 mmap，失败时回退整文件读取，保持 macOS 和 Windows 的可恢复路径。
//! - 解析过程必须持续检查取消标记并上报进度，避免 UI 线程等待不可中断的长耗时任务。
//! - 损坏记录、重复对象 ID、缺失类布局和异常字符串都沿用原有中文错误或 fallback 行为。

use super::*;
use std::time::Instant;

/// 解析 HPROF 对象图。
pub(super) fn parse_hprof_object_graph<F>(
    path: &Path,
    progress: HprofProgress,
    progress_reporter: &mut F,
    cancel_flag: &AtomicBool,
) -> Result<HprofObjectGraph, HprofError>
where
    F: FnMut(HprofProgress),
{
    let bytes = HprofInputBytes::open(path)?;
    let progress_rate = HprofProgressRateSampler::new(&progress);
    let mut parser = HprofParser {
        bytes: bytes.as_slice(),
        position: 0,
        identifier_size: 0,
        progress,
        progress_reporter,
        cancel_flag,
        state: HprofParserState::default(),
        progress_rate,
    };
    parser.parse()
}

/// 原始类信息。
///
/// 业务意图：
/// - HPROF 中实例字段会继承父类字段；解析 instance dump 前需要保留 class dump 的原始字段布局。
#[derive(Clone, Debug)]
pub(super) struct RawClassInfo {
    /// 父类 ID。
    pub(super) super_class_id: HprofObjectId,
    /// 类加载器对象 ID。
    ///
    /// 业务意图：
    /// - MAT 兼容强引用图仍需要把 class object 到 class loader 的关系建成强边，避免静态字段和类加载器 retained 结构断开。
    pub(super) class_loader_id: HprofObjectId,
    /// 当前类声明的静态字段。
    pub(super) static_fields: Vec<RawStaticFieldDescriptor>,
    /// 当前类声明的实例字段。
    pub(super) instance_fields: Vec<RawFieldDescriptor>,
}

/// 原始字段描述。
#[derive(Clone, Debug)]
pub(super) struct RawFieldDescriptor {
    /// 字段名字符串 ID。
    ///
    /// 实现原因：
    /// - `java.lang.ref.Reference.referent` 必须根据字段名和声明类识别；解析阶段先保留 ID，待字符串表完整后再解析成名称。
    pub(super) name_id: HprofObjectId,
    /// HPROF 字段类型编码。
    pub(super) field_type: u8,
}

/// 原始静态字段描述。
#[derive(Clone, Debug)]
pub(super) struct RawStaticFieldDescriptor {
    /// 字段名字符串 ID。
    pub(super) _name_id: HprofObjectId,
    /// HPROF 字段类型编码。
    pub(super) field_type: u8,
    /// 对象静态字段的值；非对象字段为 `None`。
    pub(super) reference_id: Option<HprofObjectId>,
}

/// START_THREAD 顶层记录。
///
/// 业务意图：
/// - 线程对象、线程序号、栈轨迹和线程名在 HPROF 中被拆成多张表；保留原始 ID 便于完成阶段统一解析。
#[derive(Clone, Debug)]
pub(super) struct HprofThreadStartRecord {
    /// 线程启动记录关联的 stack trace serial。
    pub(super) stack_trace_serial: u32,
    /// 线程名字符串 ID。
    pub(super) thread_name_id: HprofObjectId,
}

/// ROOT_THREAD_OBJECT 子记录。
///
/// 业务意图：
/// - 部分 dump 可能没有完整 START_THREAD 记录，Thread Object root 仍可提供线程对象与栈轨迹的关联。
#[derive(Clone, Debug)]
pub(super) struct HprofThreadObjectRootRecord {
    /// Thread Object root 关联的 stack trace serial。
    pub(super) stack_trace_serial: u32,
}

/// STACK_TRACE 顶层记录。
#[derive(Clone, Debug)]
pub(super) struct HprofStackTraceRecord {
    /// 关联的 thread serial。
    pub(super) thread_serial: u32,
    /// 按调用顺序保存的 frame ID。
    pub(super) frame_ids: Vec<HprofObjectId>,
}

/// STACK_FRAME 顶层记录。
#[derive(Clone, Debug)]
pub(super) struct HprofStackFrameRecord {
    /// 方法名字符串 ID。
    pub(super) method_name_id: HprofObjectId,
    /// JVM 方法签名字符串 ID。
    pub(super) method_signature_id: HprofObjectId,
    /// 源文件名字符串 ID。
    pub(super) source_file_id: HprofObjectId,
    /// LOAD_CLASS serial。
    pub(super) class_serial: u32,
    /// HPROF 原始行号；负数有特殊含义。
    pub(super) line_number: i32,
}

/// 延迟解析出的 `java.lang.Thread` 实例字段。
///
/// 业务意图：
/// - Retained heap 要等 dominator 完成才能得到，但 name、daemon、priority、threadStatus 等来自实例字段，
///   因此在解析阶段先保存结构化原始值，完成阶段再拼成最终属性表。
#[derive(Clone, Debug, Default)]
pub(super) struct HprofThreadInstanceInfo {
    /// 线程对象 ID。
    pub(super) object_id: HprofObjectId,
    /// 线程对象类 ID。
    pub(super) class_id: HprofObjectId,
    /// `Thread.name` 字段引用的 `java.lang.String` 对象 ID。
    pub(super) name_object_id: Option<HprofObjectId>,
    /// `Thread.contextClassLoader` 字段引用的对象 ID。
    pub(super) context_class_loader_id: Option<HprofObjectId>,
    /// `Thread.daemon` 字段值。
    pub(super) daemon: Option<bool>,
    /// `Thread.priority` 字段值。
    pub(super) priority: Option<i32>,
    /// `Thread.threadStatus` 字段值。
    pub(super) thread_status: Option<u32>,
}

/// 可用于轻量字符串 fallback 的 primitive array 数据位置。
///
/// 边界条件：
/// - 只保存较短的 `byte[]` / `char[]` 偏移，避免为了少数线程名把大型业务 byte array 全部复制到内存。
#[derive(Clone, Copy, Debug)]
struct PendingPrimitiveArrayDump {
    /// HPROF 基础类型编码。
    element_type: u8,
    /// 数组元素数量。
    length: u32,
    /// 数组数据在输入切片中的起始偏移。
    data_start: usize,
    /// 数组数据字节数。
    data_len: usize,
}

/// 延迟物化的实例 dump。
///
/// 业务意图：
/// - 实例字段的 Reference 诊断分类依赖完整类名、字段名和继承链；解析二进制记录时只保存数据区偏移，避免提前丢失 referent 语义。
#[derive(Clone, Copy, Debug)]
struct PendingInstanceDump {
    /// 实例对象 ID。
    pub(super) object_id: HprofObjectId,
    /// 实例所属类 ID。
    pub(super) class_id: HprofObjectId,
    /// 实例字段数据在 mmap/输入切片中的起始偏移。
    data_start: usize,
    /// 实例字段数据长度。
    data_len: usize,
}

/// HPROF parser 的可变状态。
///
/// 业务意图：
/// - 解析过程需要同时累积对象图、类布局缓存和进度计数；集中在状态结构里可以让读取函数签名保持简单。
#[derive(Default)]
struct HprofParserState {
    /// 当前对象图，读取 header 后初始化。
    graph: Option<HprofObjectGraph>,
    /// 原始类布局。
    raw_classes: FxHashMap<HprofObjectId, RawClassInfo>,
    /// 需要在类名和字段名解析完成后再物化对象引用边的实例对象。
    pending_instances: Vec<PendingInstanceDump>,
    /// 可用于解析线程名 fallback 的短基础类型数组数据位置。
    pending_primitive_arrays: FxHashMap<HprofObjectId, PendingPrimitiveArrayDump>,
    /// 已展开继承后的引用字段偏移缓存。
    reference_layout_cache: FxHashMap<HprofObjectId, ResolvedReferenceLayout>,
}

/// 已解析的引用字段布局。
///
/// 业务意图：
/// - `INSTANCE_DUMP` 数量通常远大于类数量，缓存对象引用字段的字节偏移可以避免每个实例都遍历完整字段描述。
#[derive(Clone, Debug)]
pub(super) struct ResolvedReferenceLayout {
    /// 对象引用字段在实例数据区中的字节偏移及 MAT 引用强度。
    pub(super) reference_fields: Vec<ResolvedReferenceField>,
    /// 该类继承展开后的 HPROF 实例数据字节数，用于读取字段偏移。
    pub(super) hprof_byte_size: usize,
    /// 该类按 MAT size model 估算的字段区域字节数，用于 shallow size。
    pub(super) heap_field_byte_size: u64,
}

/// 已解析的对象引用字段。
#[derive(Clone, Debug)]
pub(super) struct ResolvedReferenceField {
    /// 该对象 ID 在实例数据区中的字节偏移。
    pub(super) offset: usize,
    /// 该引用在 MAT 兼容对象图中的强度。
    pub(super) strength: HprofReferenceStrength,
}

/// HPROF 二进制 parser。
///
/// 边界条件：
/// - 所有读取方法都会维护 `position`，用于进度展示和 heap segment 边界校验。
struct HprofParser<'a, F>
where
    F: FnMut(HprofProgress),
{
    /// HPROF 文件完整字节。
    bytes: &'a [u8],
    /// 已读取字节偏移。
    position: usize,
    /// header 中声明的对象 ID 字节数。
    identifier_size: u8,
    /// 当前进度。
    progress: HprofProgress,
    /// 进度回调。
    progress_reporter: &'a mut F,
    /// 取消标记。
    cancel_flag: &'a AtomicBool,
    /// 累积解析状态。
    state: HprofParserState,
    /// 读取阶段速率采样器。
    ///
    /// 业务意图：
    /// - 大 dump 的字节进度和真实工作量可能严重不一致；采样器在后台线程内计算 MiB/s、记录/s 和对象/s，
    ///   UI 只展示结果，避免主线程再做状态差分。
    progress_rate: HprofProgressRateSampler,
}

/// HPROF 读取进度速率采样器。
///
/// 业务意图：
/// - 解析线程是唯一了解连续进度快照的地方，在这里按时间差计算速率可以帮助用户判断当前是 I/O 慢、记录密集还是对象索引密集。
struct HprofProgressRateSampler {
    /// 上次采样时间。
    last_at: Instant,
    /// 上次采样的已读字节。
    last_bytes: u64,
    /// 上次采样的记录数。
    last_records: u64,
    /// 上次采样的对象数。
    last_objects: usize,
}

impl HprofProgressRateSampler {
    /// 创建采样器。
    fn new(progress: &HprofProgress) -> Self {
        Self {
            last_at: Instant::now(),
            last_bytes: progress.bytes_read,
            last_records: progress.record_count,
            last_objects: progress.object_count,
        }
    }

    /// 根据当前进度更新速率字段。
    ///
    /// 边界条件：
    /// - 小于 200ms 的间隔不更新，避免进度回调过密时产生剧烈抖动。
    fn update(&mut self, progress: &mut HprofProgress) {
        let elapsed = self.last_at.elapsed();
        let elapsed_seconds = elapsed.as_secs_f64();
        if elapsed_seconds < 0.2 {
            return;
        }
        progress.bytes_per_second =
            progress.bytes_read.saturating_sub(self.last_bytes) as f64 / elapsed_seconds;
        progress.records_per_second =
            progress.record_count.saturating_sub(self.last_records) as f64 / elapsed_seconds;
        progress.objects_per_second =
            progress.object_count.saturating_sub(self.last_objects) as f64 / elapsed_seconds;
        self.last_at = Instant::now();
        self.last_bytes = progress.bytes_read;
        self.last_records = progress.record_count;
        self.last_objects = progress.object_count;
    }
}

impl<'a, F> HprofParser<'a, F>
where
    F: FnMut(HprofProgress),
{
    /// 执行完整 HPROF 对象图解析。
    fn parse(&mut self) -> Result<HprofObjectGraph, HprofError> {
        self.report_stage(HprofAnalysisStage::ReadingHeader, "正在读取 HPROF header")?;
        let header = self.read_header()?;
        let size_model = HprofSizeModel::mat_compatible(&header, self.progress.total_bytes);
        self.state.graph = Some(HprofObjectGraph::new(header, size_model));
        self.report_stage(HprofAnalysisStage::ReadingRecords, "正在解析 HPROF 记录")?;

        loop {
            self.check_cancel()?;
            if self.is_eof() {
                break;
            }
            let tag = self.read_u8()?;
            let _time_delta = self.read_u32()?;
            let length = self.read_u32()?;
            self.progress.record_count += 1;

            match tag {
                TAG_STRING_IN_UTF8 => self.read_string_record(length)?,
                TAG_LOAD_CLASS => self.read_load_class_record(length)?,
                TAG_STACK_FRAME => self.read_stack_frame_record(length)?,
                TAG_STACK_TRACE => self.read_stack_trace_record(length)?,
                TAG_START_THREAD => self.read_start_thread_record(length)?,
                TAG_END_THREAD => self.skip_bytes(u64::from(length))?,
                TAG_HEAP_DUMP | TAG_HEAP_DUMP_SEGMENT => self.read_heap_dump_record(length)?,
                TAG_HEAP_DUMP_END => self.skip_bytes(u64::from(length))?,
                _ => self.skip_bytes(u64::from(length))?,
            }
            self.refresh_progress_counts();
            self.maybe_report_progress();
        }

        self.build_object_index_after_reading()?;
        self.report_stage(HprofAnalysisStage::ResolvingClasses, "正在解析类名和字段名")?;
        self.resolve_class_and_field_names()?;
        self.report_stage(
            HprofAnalysisStage::MaterializingReferences,
            "正在构建 MAT 兼容引用边",
        )?;
        self.materialize_mat_compatible_graph()?;
        self.refresh_progress_counts();
        self.report_progress();
        self.state
            .graph
            .take()
            .ok_or_else(|| HprofError::InvalidFormat("内部错误：HPROF 对象图未初始化".to_string()))
    }

    /// 读取 HPROF header。
    fn read_header(&mut self) -> Result<HprofHeader, HprofError> {
        let mut label_bytes = Vec::new();
        for _ in 0..HPROF_HEADER_MAX_BYTES {
            let byte = self.read_u8()?;
            if byte == 0 {
                let label = String::from_utf8(label_bytes).map_err(|_| {
                    HprofError::InvalidFormat("HPROF 文件头不是有效 UTF-8".to_string())
                })?;
                validate_hprof_label(&label)?;
                let identifier_size = self.read_u32()?;
                if !matches!(identifier_size, 4 | 8) {
                    return Err(HprofError::InvalidFormat(format!(
                        "不支持的 HPROF 对象 ID 字节数：{identifier_size}"
                    )));
                }
                self.identifier_size = identifier_size as u8;
                let timestamp_millis = self.read_u64()?;
                return Ok(HprofHeader {
                    label,
                    identifier_size: identifier_size as u8,
                    timestamp_millis,
                });
            }
            label_bytes.push(byte);
        }
        Err(HprofError::InvalidFormat(
            "HPROF 文件头过长，可能不是 JVM HPROF 文件".to_string(),
        ))
    }

    /// 读取 UTF8 字符串记录。
    fn read_string_record(&mut self, length: u32) -> Result<(), HprofError> {
        let id_size = self.id_size()?;
        if length < u32::from(id_size) {
            return Err(HprofError::InvalidFormat(
                "STRING_IN_UTF8 记录长度小于对象 ID 长度".to_string(),
            ));
        }
        let id = self.read_id()?;
        let string_len = length as usize - id_size as usize;
        let bytes = self.read_slice(string_len)?;
        let value = String::from_utf8_lossy(bytes).into_owned();
        self.graph_mut()?.strings.insert(id, value);
        Ok(())
    }

    /// 读取 STACK_FRAME 记录。
    ///
    /// 业务意图：
    /// - MAT 线程详情中的每一行堆栈都来自该记录；这里先保留字符串 ID，等 STRING 和 LOAD_CLASS 全部解析后再生成展示文案。
    fn read_stack_frame_record(&mut self, length: u32) -> Result<(), HprofError> {
        let id_size = self.id_size()?;
        let expected = u32::from(id_size) * 4 + 8;
        if length < expected {
            return Err(HprofError::InvalidFormat(
                "STACK_FRAME 记录长度不完整".to_string(),
            ));
        }
        let frame_id = self.read_id()?;
        let method_name_id = self.read_id()?;
        let method_signature_id = self.read_id()?;
        let source_file_id = self.read_id()?;
        let class_serial = self.read_u32()?;
        let line_number = self.read_i32()?;
        self.graph_mut()?.stack_frames.insert(
            frame_id,
            HprofStackFrameRecord {
                method_name_id,
                method_signature_id,
                source_file_id,
                class_serial,
                line_number,
            },
        );
        let extra = u64::from(length - expected);
        if extra > 0 {
            self.skip_bytes(extra)?;
        }
        Ok(())
    }

    /// 读取 STACK_TRACE 记录。
    fn read_stack_trace_record(&mut self, length: u32) -> Result<(), HprofError> {
        let id_size = self.id_size()?;
        if length < 12 {
            return Err(HprofError::InvalidFormat(
                "STACK_TRACE 记录长度不完整".to_string(),
            ));
        }
        let stack_trace_serial = self.read_u32()?;
        let thread_serial = self.read_u32()?;
        let frame_count = self.read_u32()?;
        let expected = 12u32
            .checked_add(frame_count.saturating_mul(u32::from(id_size)))
            .ok_or_else(|| HprofError::InvalidFormat("STACK_TRACE 长度溢出".to_string()))?;
        if length < expected {
            return Err(HprofError::InvalidFormat(
                "STACK_TRACE frame 列表长度不完整".to_string(),
            ));
        }
        let mut frame_ids = Vec::with_capacity(frame_count as usize);
        for _ in 0..frame_count {
            frame_ids.push(self.read_id()?);
        }
        self.graph_mut()?.stack_traces.insert(
            stack_trace_serial,
            HprofStackTraceRecord {
                thread_serial,
                frame_ids,
            },
        );
        let extra = u64::from(length - expected);
        if extra > 0 {
            self.skip_bytes(extra)?;
        }
        Ok(())
    }

    /// 读取 START_THREAD 记录。
    fn read_start_thread_record(&mut self, length: u32) -> Result<(), HprofError> {
        let id_size = self.id_size()?;
        let expected = 8 + u32::from(id_size) * 4;
        if length < expected {
            return Err(HprofError::InvalidFormat(
                "START_THREAD 记录长度不完整".to_string(),
            ));
        }
        let thread_serial = self.read_u32()?;
        let object_id = self.read_id()?;
        let stack_trace_serial = self.read_u32()?;
        let thread_name_id = self.read_id()?;
        let _thread_group_name_id = self.read_id()?;
        let _thread_parent_group_name_id = self.read_id()?;
        let graph = self.graph_mut()?;
        graph
            .thread_serial_by_object_id
            .insert(object_id, thread_serial);
        graph.thread_starts.insert(
            thread_serial,
            HprofThreadStartRecord {
                stack_trace_serial,
                thread_name_id,
            },
        );
        let extra = u64::from(length - expected);
        if extra > 0 {
            self.skip_bytes(extra)?;
        }
        Ok(())
    }

    /// 读取 LOAD_CLASS 记录。
    fn read_load_class_record(&mut self, length: u32) -> Result<(), HprofError> {
        let id_size = self.id_size()?;
        let expected = 4 + u32::from(id_size) + 4 + u32::from(id_size);
        if length < expected {
            return Err(HprofError::InvalidFormat(
                "LOAD_CLASS 记录长度不完整".to_string(),
            ));
        }
        let serial = self.read_u32()?;
        let class_object_id = self.read_id()?;
        let _stack_serial = self.read_u32()?;
        let name_string_id = self.read_id()?;
        self.graph_mut()?
            .loaded_classes
            .insert(serial, (class_object_id, name_string_id));
        let extra = u64::from(length - expected);
        if extra > 0 {
            self.skip_bytes(extra)?;
        }
        Ok(())
    }

    /// 读取 HEAP_DUMP 或 HEAP_DUMP_SEGMENT 记录。
    fn read_heap_dump_record(&mut self, length: u32) -> Result<(), HprofError> {
        let segment_end = self
            .position
            .checked_add(length as usize)
            .ok_or_else(|| HprofError::InvalidFormat("HPROF segment 长度溢出".to_string()))?;
        while self.position < segment_end {
            self.check_cancel()?;
            let sub_tag = self.read_u8()?;
            self.progress.record_count = self.progress.record_count.saturating_add(1);
            self.read_heap_sub_record(sub_tag)?;
            if self.position > segment_end {
                return Err(HprofError::InvalidFormat(format!(
                    "HEAP_DUMP 子记录越过 segment 边界：0x{sub_tag:02X}"
                )));
            }
            if self
                .progress
                .record_count
                .is_multiple_of(HPROF_PROGRESS_RECORD_INTERVAL)
            {
                self.refresh_progress_counts();
                self.report_progress();
            }
        }
        Ok(())
    }

    /// 读取单个 heap dump 子记录。
    fn read_heap_sub_record(&mut self, sub_tag: u8) -> Result<(), HprofError> {
        self.track_heap_sub_record(sub_tag);
        match sub_tag {
            SUB_ROOT_UNKNOWN => self.read_root_simple(HprofGcRootKind::Unknown),
            SUB_ROOT_JNI_GLOBAL => {
                let object_id = self.read_id()?;
                let _jni_global_ref = self.read_id()?;
                self.push_gc_root(object_id, HprofGcRootKind::JniGlobal)
            }
            SUB_ROOT_JNI_LOCAL => self.read_root_with_thread_and_frame(HprofGcRootKind::JniLocal),
            SUB_ROOT_JAVA_FRAME => self.read_root_with_thread_and_frame(HprofGcRootKind::JavaFrame),
            SUB_ROOT_NATIVE_STACK => self.read_root_with_thread(HprofGcRootKind::NativeStack),
            SUB_ROOT_STICKY_CLASS => self.read_root_simple(HprofGcRootKind::StickyClass),
            SUB_ROOT_THREAD_BLOCK => self.read_root_with_thread(HprofGcRootKind::ThreadBlock),
            SUB_ROOT_MONITOR_USED => self.read_root_simple(HprofGcRootKind::MonitorUsed),
            SUB_ROOT_THREAD_OBJECT => {
                let object_id = self.read_id()?;
                let thread_serial = self.read_u32()?;
                let stack_trace_serial = self.read_u32()?;
                {
                    let graph = self.graph_mut()?;
                    graph
                        .thread_serial_by_object_id
                        .entry(object_id)
                        .or_insert(thread_serial);
                    graph.thread_object_roots.insert(
                        object_id,
                        HprofThreadObjectRootRecord { stack_trace_serial },
                    );
                }
                self.push_gc_root(object_id, HprofGcRootKind::ThreadObject)
            }
            SUB_HEAP_DUMP_INFO => {
                let _heap_type = self.read_u32()?;
                let _heap_name_id = self.read_id()?;
                Ok(())
            }
            SUB_PRIMITIVE_ARRAY_NODATA => {
                let _array_id = self.read_id()?;
                let _stack_serial = self.read_u32()?;
                let _elements = self.read_u32()?;
                let _element_type = self.read_u8()?;
                Ok(())
            }
            SUB_CLASS_DUMP => self.read_class_dump(),
            SUB_INSTANCE_DUMP => self.read_instance_dump(),
            SUB_OBJECT_ARRAY_DUMP => self.read_object_array_dump(),
            SUB_PRIMITIVE_ARRAY_DUMP => self.read_primitive_array_dump(),
            _ => Err(HprofError::Unsupported(format!(
                "暂不支持的 HEAP_DUMP 子记录：0x{sub_tag:02X}，偏移 {}",
                self.position.saturating_sub(1)
            ))),
        }
    }

    /// 记录当前 heap 子记录类型。
    ///
    /// 业务意图：
    /// - 用户看到字节进度突然变慢时，需要知道当前区域是不是 INSTANCE_DUMP 高密度对象区。
    /// - 这里仅维护轻量计数，不解析额外内容，不改变 HPROF 语义。
    fn track_heap_sub_record(&mut self, sub_tag: u8) {
        match sub_tag {
            SUB_CLASS_DUMP => {
                self.progress.current_heap_record = "CLASS_DUMP";
                self.progress.heap_record_counts.class_dump = self
                    .progress
                    .heap_record_counts
                    .class_dump
                    .saturating_add(1);
            }
            SUB_INSTANCE_DUMP => {
                self.progress.current_heap_record = "INSTANCE_DUMP";
                self.progress.heap_record_counts.instance_dump = self
                    .progress
                    .heap_record_counts
                    .instance_dump
                    .saturating_add(1);
            }
            SUB_OBJECT_ARRAY_DUMP => {
                self.progress.current_heap_record = "OBJECT_ARRAY_DUMP";
                self.progress.heap_record_counts.object_array_dump = self
                    .progress
                    .heap_record_counts
                    .object_array_dump
                    .saturating_add(1);
            }
            SUB_PRIMITIVE_ARRAY_DUMP | SUB_PRIMITIVE_ARRAY_NODATA => {
                self.progress.current_heap_record = "PRIMITIVE_ARRAY_DUMP";
                self.progress.heap_record_counts.primitive_array_dump = self
                    .progress
                    .heap_record_counts
                    .primitive_array_dump
                    .saturating_add(1);
            }
            SUB_ROOT_UNKNOWN
            | SUB_ROOT_JNI_GLOBAL
            | SUB_ROOT_JNI_LOCAL
            | SUB_ROOT_JAVA_FRAME
            | SUB_ROOT_NATIVE_STACK
            | SUB_ROOT_STICKY_CLASS
            | SUB_ROOT_THREAD_BLOCK
            | SUB_ROOT_MONITOR_USED
            | SUB_ROOT_THREAD_OBJECT => {
                self.progress.current_heap_record = "GC_ROOT";
                self.progress.heap_record_counts.gc_root =
                    self.progress.heap_record_counts.gc_root.saturating_add(1);
            }
            _ => {
                self.progress.current_heap_record = "OTHER";
                self.progress.heap_record_counts.other =
                    self.progress.heap_record_counts.other.saturating_add(1);
            }
        }
    }

    /// 读取只包含对象 ID 的 GC root。
    fn read_root_simple(&mut self, kind: HprofGcRootKind) -> Result<(), HprofError> {
        let object_id = self.read_id()?;
        self.push_gc_root(object_id, kind)
    }

    /// 读取包含线程序号的 GC root。
    fn read_root_with_thread(&mut self, kind: HprofGcRootKind) -> Result<(), HprofError> {
        let object_id = self.read_id()?;
        let _thread_serial = self.read_u32()?;
        self.push_gc_root(object_id, kind)
    }

    /// 读取包含线程序号和 frame 序号的 GC root。
    fn read_root_with_thread_and_frame(&mut self, kind: HprofGcRootKind) -> Result<(), HprofError> {
        let object_id = self.read_id()?;
        let _thread_serial = self.read_u32()?;
        let _frame = self.read_u32()?;
        self.push_gc_root(object_id, kind)
    }

    /// 写入 GC root。
    fn push_gc_root(
        &mut self,
        object_id: HprofObjectId,
        kind: HprofGcRootKind,
    ) -> Result<(), HprofError> {
        self.graph_mut()?
            .gc_roots
            .push(HprofGcRoot { object_id, kind });
        Ok(())
    }

    /// 读取 CLASS_DUMP。
    fn read_class_dump(&mut self) -> Result<(), HprofError> {
        let class_object_id = self.read_id()?;
        let _stack_serial = self.read_u32()?;
        let super_class_id = self.read_id()?;
        let class_loader_id = self.read_id()?;
        let _signers = self.read_id()?;
        let _protection_domain = self.read_id()?;
        let _reserved_1 = self.read_id()?;
        let _reserved_2 = self.read_id()?;
        let instance_size = u64::from(self.read_u32()?);

        let constant_pool_count = self.read_u16()?;
        for _ in 0..constant_pool_count {
            let _index = self.read_u16()?;
            let field_type = self.read_u8()?;
            self.skip_field_value(field_type)?;
        }

        let static_field_count = self.read_u16()?;
        let mut static_fields = Vec::with_capacity(static_field_count as usize);
        for _ in 0..static_field_count {
            let name_id = self.read_id()?;
            let field_type = self.read_u8()?;
            let reference_id = if field_type == FIELD_TYPE_OBJECT {
                Some(self.read_id()?)
            } else {
                self.skip_field_value(field_type)?;
                None
            };
            static_fields.push(RawStaticFieldDescriptor {
                _name_id: name_id,
                field_type,
                reference_id,
            });
        }

        let instance_field_count = self.read_u16()?;
        let mut instance_fields = Vec::with_capacity(instance_field_count as usize);
        for _ in 0..instance_field_count {
            let name_id = self.read_id()?;
            let field_type = self.read_u8()?;
            instance_fields.push(RawFieldDescriptor {
                name_id,
                field_type,
            });
        }

        self.state.raw_classes.insert(
            class_object_id,
            RawClassInfo {
                super_class_id,
                class_loader_id,
                static_fields: static_fields.clone(),
                instance_fields: instance_fields.clone(),
            },
        );
        let field_descriptors = instance_fields
            .iter()
            .map(|field| HprofFieldDescriptor {
                name: None,
                field_type: field.field_type,
            })
            .collect();
        let graph = self.graph_mut()?;
        graph.classes.insert(
            class_object_id,
            HprofClassInfo {
                instance_size,
                name: None,
                instance_fields: field_descriptors,
            },
        );
        graph.insert_object_deferred_index(
            HprofHeapObject {
                id: class_object_id,
                class_id: class_object_id,
                shallow_size: 0,
                kind: HprofObjectKind::Class,
            },
            Vec::new(),
        );
        Ok(())
    }

    /// 读取 INSTANCE_DUMP。
    fn read_instance_dump(&mut self) -> Result<(), HprofError> {
        let object_id = self.read_id()?;
        let _stack_serial = self.read_u32()?;
        let class_id = self.read_id()?;
        let data_len = self.read_u32()? as usize;
        let data_start = self.position;
        self.skip_bytes(data_len as u64)?;
        self.state.pending_instances.push(PendingInstanceDump {
            object_id,
            class_id,
            data_start,
            data_len,
        });

        self.graph_mut()?.insert_object_deferred_index(
            HprofHeapObject {
                id: object_id,
                class_id,
                shallow_size: 0,
                kind: HprofObjectKind::Instance,
            },
            Vec::new(),
        );
        Ok(())
    }

    /// 读取 OBJECT_ARRAY_DUMP。
    fn read_object_array_dump(&mut self) -> Result<(), HprofError> {
        let array_id = self.read_id()?;
        let _stack_serial = self.read_u32()?;
        let length = self.read_u32()?;
        let array_class_id = self.read_id()?;
        let mut references = Vec::with_capacity(length as usize);
        for _ in 0..length {
            let reference_id = self.read_id()?;
            push_non_zero_reference(&mut references, reference_id);
        }
        let shallow_size = object_array_shallow_size(&self.graph_ref()?.size_model, length);
        self.graph_mut()?.insert_object_deferred_index(
            HprofHeapObject {
                id: array_id,
                class_id: array_class_id,
                shallow_size,
                kind: HprofObjectKind::ObjectArray { length },
            },
            references,
        );
        Ok(())
    }

    /// 读取 PRIMITIVE_ARRAY_DUMP。
    fn read_primitive_array_dump(&mut self) -> Result<(), HprofError> {
        let array_id = self.read_id()?;
        let _stack_serial = self.read_u32()?;
        let length = self.read_u32()?;
        let element_type = self.read_u8()?;
        let width = field_value_size(element_type, self.id_size()?).ok_or_else(|| {
            HprofError::InvalidFormat(format!("未知 primitive array 元素类型：{element_type}"))
        })?;
        let data_len = u64::from(length) * u64::from(width);
        let data_start = self.position;
        self.skip_bytes(data_len)?;
        if matches!(element_type, FIELD_TYPE_BYTE | FIELD_TYPE_CHAR)
            && length <= HPROF_THREAD_STRING_FALLBACK_MAX_ELEMENTS
        {
            let data_len = usize::try_from(data_len).map_err(|_| {
                HprofError::InvalidFormat("primitive array 数据长度超过当前平台限制".to_string())
            })?;
            self.state.pending_primitive_arrays.insert(
                array_id,
                PendingPrimitiveArrayDump {
                    element_type,
                    length,
                    data_start,
                    data_len,
                },
            );
        }
        let shallow_size = primitive_array_shallow_size(
            &self.graph_ref()?.size_model,
            element_type,
            u64::from(length),
        )?;
        self.graph_mut()?.insert_object_deferred_index(
            HprofHeapObject {
                id: array_id,
                class_id: 0,
                shallow_size,
                kind: HprofObjectKind::PrimitiveArray {
                    element_type,
                    length,
                },
            },
            Vec::new(),
        );
        Ok(())
    }

    /// 顺序扫描结束后一次性建立对象索引。
    ///
    /// 业务意图：
    /// - 读取 INSTANCE_DUMP 的热路径只追加连续对象数组，不再对千万级 `object_id -> index` HashMap 做随机写入。
    /// - 顺序扫描完成后统一预留 HashMap 容量并建立索引，把慢点变成独立可见阶段，便于用户判断“解析对象记录”是否已经真正结束。
    ///
    /// 边界条件：
    /// - 重复对象 ID 仍然是格式错误，只是从“读到第二个重复对象时失败”改为“建立索引阶段失败”，不改变最终校验语义。
    fn build_object_index_after_reading(&mut self) -> Result<(), HprofError> {
        self.check_cancel()?;
        let mut graph = self.state.graph.take().ok_or_else(|| {
            HprofError::InvalidFormat("内部错误：HPROF 对象图未初始化".to_string())
        })?;
        let total = graph.object_count();
        graph.object_indices = FxHashMap::default();
        graph.object_indices.reserve(total);

        self.progress.stage = HprofAnalysisStage::BuildingObjectIndex;
        self.progress.message = "正在建立 HPROF 对象索引".to_string();
        self.progress.sub_message = "准备建立对象 ID 索引".to_string();
        self.progress.phase_done = 0;
        self.progress.phase_total = total as u64;
        self.progress.phase_unit = "对象";
        self.progress.bytes_read = self.position as u64;
        self.progress.object_count = total;
        self.progress.class_count = graph.class_count();
        self.progress.gc_root_count = graph.gc_roots.len();
        self.progress.edge_count = graph.edge_count();
        self.report_progress();

        for index in 0..total {
            check_cancel(self.cancel_flag)?;
            let object_id = graph.objects[index].id;
            if graph.object_indices.insert(object_id, index).is_some() {
                return Err(HprofError::InvalidFormat(format!(
                    "HPROF 对象重复定义：0x{object_id:x}"
                )));
            }
            let done = index + 1;
            if should_report_work(done, total) {
                self.progress.sub_message = "建立对象 ID 索引".to_string();
                self.progress.phase_done = done as u64;
                self.report_progress();
            }
        }
        self.progress.phase_done = total as u64;
        self.progress.sub_message = "对象 ID 索引建立完成".to_string();
        self.report_progress();
        self.state.graph = Some(graph);
        Ok(())
    }

    /// 解析 LOAD_CLASS、STRING_IN_UTF8 得到类名和字段名。
    ///
    /// 业务意图：
    /// - MAT 兼容引用诊断依赖 `java.lang.ref.Reference.referent` 的字段名和声明类；必须在物化对象引用边前完成解析。
    fn resolve_class_and_field_names(&mut self) -> Result<(), HprofError> {
        let class_names = {
            let graph = self.graph_ref()?;
            graph
                .loaded_classes
                .values()
                .filter_map(|(class_object_id, name_string_id)| {
                    graph
                        .strings
                        .get(name_string_id)
                        .map(|name| (*class_object_id, normalize_java_class_name(name)))
                })
                .collect::<FxHashMap<_, _>>()
        };
        let class_updates = self
            .state
            .raw_classes
            .iter()
            .map(|(class_id, raw_class)| {
                let instance_fields = raw_class
                    .instance_fields
                    .iter()
                    .map(|field| HprofFieldDescriptor {
                        name: self
                            .state
                            .graph
                            .as_ref()
                            .and_then(|graph| graph.strings.get(&field.name_id))
                            .cloned(),
                        field_type: field.field_type,
                    })
                    .collect::<Vec<_>>();
                (*class_id, instance_fields)
            })
            .collect::<Vec<_>>();
        for (class_id, instance_fields) in class_updates {
            if let Some(class_info) = self.graph_mut()?.classes.get_mut(&class_id) {
                class_info.name = class_names.get(&class_id).cloned();
                class_info.instance_fields = instance_fields;
            }
        }
        Ok(())
    }

    /// 基于完整类元数据构建 MAT 兼容对象引用边和 shallow size。
    ///
    /// 实现原因：
    /// - 二进制扫描阶段无法可靠判断 `Reference.referent` 是否弱/软/虚引用，因为类名、字段名和继承链可能在后续记录中才完整。
    /// - 这里统一扫描延迟保存的实例数据，把所有对象引用写入 MAT 对象图，并同步统计 Reference referent 分类和 shallow size。
    fn materialize_mat_compatible_graph(&mut self) -> Result<(), HprofError> {
        let class_ids = self.state.raw_classes.keys().copied().collect::<Vec<_>>();
        // pending instance 表在引用物化后不再使用，直接移动可避免大 dump 中额外复制数百万条偏移记录。
        let pending_instances = std::mem::take(&mut self.state.pending_instances);
        let total_work = class_ids.len().saturating_add(pending_instances.len());
        let mut reference_edge_stats = HprofReferenceEdgeStats::default();
        let mut done = 0usize;

        for class_id in class_ids {
            self.check_cancel()?;
            let (shallow_size, references) = self.class_object_payload(class_id)?;
            self.graph_mut()?
                .update_object_payload(class_id, shallow_size, references);
            done = done.saturating_add(1);
            self.report_materialize_progress(done, total_work, "构建 class/static 引用边")?;
        }

        let id_size = self.id_size()? as usize;
        let mut layout_cache = std::mem::take(&mut self.state.reference_layout_cache);
        let (java_string_class_ids, thread_class_ids) = {
            let graph = self.graph_ref()?;
            (
                collect_subclass_ids(&self.state.raw_classes, &graph.classes, "java.lang.String"),
                collect_subclass_ids(&self.state.raw_classes, &graph.classes, "java.lang.Thread"),
            )
        };
        for pending in pending_instances {
            self.check_cancel()?;
            let (shallow_size, references, edge_stats) =
                self.instance_payload(pending, id_size, &mut layout_cache)?;
            // 线程名 fallback 只需要 String/Thread 两类对象；预先按 class_id 筛选，避免普通业务对象反复走继承链。
            let java_string_value = if java_string_class_ids.contains(&pending.class_id) {
                self.decode_java_string_instance(pending, id_size)?
            } else {
                None
            };
            let thread_instance = if thread_class_ids.contains(&pending.class_id) {
                self.decode_thread_instance(pending, id_size)?
            } else {
                None
            };
            reference_edge_stats.merge(edge_stats);
            let graph = self.graph_mut()?;
            graph.update_object_payload(pending.object_id, shallow_size, references);
            if let Some(value) = java_string_value {
                graph.java_string_values.insert(pending.object_id, value);
            }
            if let Some(thread_instance) = thread_instance {
                graph
                    .thread_instances
                    .insert(thread_instance.object_id, thread_instance);
            }
            done = done.saturating_add(1);
            self.report_materialize_progress(
                done,
                total_work,
                "构建实例引用边 / 计算 shallow size",
            )?;
        }
        self.state.reference_layout_cache = layout_cache;
        self.graph_mut()?.reference_edge_stats = reference_edge_stats;
        self.report_materialize_progress(done, total_work, "补充 ClassLoader 合成可达边")?;
        self.materialize_class_loader_edges()?;
        self.refresh_progress_counts();
        Ok(())
    }

    /// 补充 MAT 兼容的 class loader 可达关系。
    ///
    /// 业务意图：
    /// - HPROF `CLASS_DUMP` 只记录 class object 的 `class_loader_id`，这不是普通 Java 字段；
    ///   如果只建 `class object -> class loader`，从已可达的 class loader 无法走到它加载的 class object，class static 字段也会断开。
    /// - MAT 会把“类加载器定义的类”作为可达关系处理，因此这里合成 `class loader -> class object` 强边。
    /// - `class_loader_id == 0` 的 bootstrap/system class 没有普通对象可挂载，直接加入虚拟 root 入口，保证系统类 static 字段可达。
    fn materialize_class_loader_edges(&mut self) -> Result<(), HprofError> {
        let mut loader_to_classes: FxHashMap<HprofObjectId, Vec<HprofObjectId>> =
            FxHashMap::default();
        let mut bootstrap_class_ids = Vec::new();
        for (class_id, raw_class) in &self.state.raw_classes {
            if raw_class.class_loader_id == 0 {
                bootstrap_class_ids.push(*class_id);
            } else {
                loader_to_classes
                    .entry(raw_class.class_loader_id)
                    .or_default()
                    .push(*class_id);
            }
        }

        let mut synthetic_class_loader_edge_count = 0usize;
        for (loader_id, mut class_ids) in loader_to_classes {
            self.check_cancel()?;
            normalize_references(&mut class_ids);
            let Some(loader_index) = self.graph_ref()?.object_index(loader_id) else {
                continue;
            };
            let loader_object = &self.graph_ref()?.objects[loader_index];
            let shallow_size = loader_object.shallow_size;
            let mut references = self
                .graph_ref()?
                .references_for_index(loader_index)
                .to_vec();
            let previous_len = references.len();
            references.extend(class_ids);
            normalize_references(&mut references);
            synthetic_class_loader_edge_count = synthetic_class_loader_edge_count
                .saturating_add(references.len().saturating_sub(previous_len));
            self.graph_mut()?
                .update_object_payload(loader_id, shallow_size, references);
        }

        let mut synthetic_bootstrap_class_root_count = 0usize;
        normalize_references(&mut bootstrap_class_ids);
        for class_id in bootstrap_class_ids {
            self.check_cancel()?;
            if self.graph_ref()?.contains_object_id(class_id) {
                self.graph_mut()?.gc_roots.push(HprofGcRoot {
                    object_id: class_id,
                    kind: HprofGcRootKind::SyntheticBootstrapClass,
                });
                synthetic_bootstrap_class_root_count =
                    synthetic_bootstrap_class_root_count.saturating_add(1);
            }
        }

        let graph = self.graph_mut()?;
        graph.synthetic_class_loader_edge_count = synthetic_class_loader_edge_count;
        graph.synthetic_bootstrap_class_root_count = synthetic_bootstrap_class_root_count;
        Ok(())
    }

    /// 构建 class object 的对象引用列表。
    fn class_object_payload(
        &self,
        class_id: HprofObjectId,
    ) -> Result<(u64, Vec<HprofObjectId>), HprofError> {
        let raw_class = self.state.raw_classes.get(&class_id).ok_or_else(|| {
            HprofError::InvalidFormat(format!("内部错误：缺少 class 0x{class_id:x} 的原始信息"))
        })?;
        let mut references = Vec::new();
        for field in &raw_class.static_fields {
            if field.field_type == FIELD_TYPE_OBJECT
                && let Some(reference_id) = field.reference_id
            {
                push_non_zero_reference(&mut references, reference_id);
            }
        }
        push_non_zero_reference(&mut references, raw_class.super_class_id);
        push_non_zero_reference(&mut references, raw_class.class_loader_id);
        Ok((0, references))
    }

    /// 构建单个实例对象的 MAT 兼容 shallow size、对象引用列表和 Reference referent 统计。
    fn instance_payload(
        &self,
        pending: PendingInstanceDump,
        id_size: usize,
        layout_cache: &mut FxHashMap<HprofObjectId, ResolvedReferenceLayout>,
    ) -> Result<(u64, Vec<HprofObjectId>, HprofReferenceEdgeStats), HprofError> {
        let data_end = pending
            .data_start
            .checked_add(pending.data_len)
            .ok_or_else(|| HprofError::InvalidFormat("实例字段数据偏移溢出".to_string()))?;
        if data_end > self.bytes.len() {
            return Err(HprofError::InvalidFormat(format!(
                "实例 0x{:x} 字段数据越过文件边界",
                pending.object_id
            )));
        }
        let data = &self.bytes[pending.data_start..data_end];
        let layout = {
            let graph = self.graph_ref()?;
            resolve_reference_layout(
                &self.state.raw_classes,
                &graph.classes,
                &graph.strings,
                layout_cache,
                pending.class_id,
                self.identifier_size,
                &graph.size_model,
            )
        };

        let mut references = Vec::new();
        let mut reference_edge_stats = HprofReferenceEdgeStats::default();
        let shallow_size = if let Some(layout) = layout {
            for field in &layout.reference_fields {
                if field.offset + id_size <= data.len() {
                    let reference_id =
                        read_id_from_slice(&data[field.offset..field.offset + id_size]);
                    if reference_id == 0 {
                        continue;
                    }
                    references.push(reference_id);
                    reference_edge_stats.add_strength(field.strength);
                }
            }
            instance_shallow_size(&self.graph_ref()?.size_model, layout.heap_field_byte_size)
        } else {
            let fallback_size = self
                .graph_ref()?
                .classes
                .get(&pending.class_id)
                .map(|class| class.instance_size)
                .unwrap_or(pending.data_len as u64);
            instance_shallow_size(&self.graph_ref()?.size_model, fallback_size)
        };
        Ok((shallow_size, references, reference_edge_stats))
    }

    /// 尝试从 `java.lang.String` 实例解析字符串值。
    ///
    /// 业务意图：
    /// - START_THREAD 是线程名的首选来源；当 dump 缺失 START_THREAD 时，仍可通过 `Thread.name` 指向的 String 实例恢复部分线程名。
    /// - 这里只支持常见 HotSpot `char[] value` 和 compact-string `byte[] value + coder`，解析失败返回 `None` 而不影响主分析。
    /// - 调用方已用 class_id 缓存确认对象属于 `java.lang.String`，这里不再重复走继承链。
    fn decode_java_string_instance(
        &self,
        pending: PendingInstanceDump,
        id_size: usize,
    ) -> Result<Option<String>, HprofError> {
        let data = self.pending_instance_data(pending)?;
        let mut value_array_id = None;
        let mut coder = None;
        let mut offset = None;
        let mut count = None;
        self.for_each_instance_field(
            pending.class_id,
            data,
            id_size,
            |name, field_type, bytes| match (name, field_type) {
                ("value", FIELD_TYPE_OBJECT) => value_array_id = Some(read_id_from_slice(bytes)),
                ("coder", FIELD_TYPE_BYTE) => coder = bytes.first().copied(),
                ("offset", FIELD_TYPE_INT) => {
                    offset = Some(read_i32_from_slice(bytes).max(0) as usize)
                }
                ("count", FIELD_TYPE_INT) => {
                    count = Some(read_i32_from_slice(bytes).max(0) as usize)
                }
                _ => {}
            },
        )?;

        let Some(value_array_id) = value_array_id.filter(|id| *id != 0) else {
            return Ok(None);
        };
        Ok(self.decode_java_string_array(value_array_id, coder, offset, count))
    }

    /// 尝试从 `java.lang.Thread` 或其子类实例解析线程属性原始值。
    ///
    /// 业务意图：
    /// - 调用方已用 class_id 缓存确认对象属于线程类，这里只负责读取字段值，避免对每个普通实例重复做继承链判断。
    fn decode_thread_instance(
        &self,
        pending: PendingInstanceDump,
        id_size: usize,
    ) -> Result<Option<HprofThreadInstanceInfo>, HprofError> {
        let data = self.pending_instance_data(pending)?;
        let mut info = HprofThreadInstanceInfo {
            object_id: pending.object_id,
            class_id: pending.class_id,
            ..HprofThreadInstanceInfo::default()
        };
        self.for_each_instance_field(
            pending.class_id,
            data,
            id_size,
            |name, field_type, bytes| match (name, field_type) {
                ("name", FIELD_TYPE_OBJECT) => {
                    let value = read_id_from_slice(bytes);
                    if value != 0 {
                        info.name_object_id = Some(value);
                    }
                }
                ("contextClassLoader", FIELD_TYPE_OBJECT) => {
                    let value = read_id_from_slice(bytes);
                    if value != 0 {
                        info.context_class_loader_id = Some(value);
                    }
                }
                ("daemon", FIELD_TYPE_BOOLEAN) => {
                    info.daemon = bytes.first().map(|value| *value != 0);
                }
                ("priority", FIELD_TYPE_INT) => {
                    info.priority = Some(read_i32_from_slice(bytes));
                }
                ("threadStatus", FIELD_TYPE_INT) => {
                    info.thread_status = Some(read_u32_from_slice(bytes));
                }
                _ => {}
            },
        )?;

        Ok(Some(info))
    }

    /// 返回延迟实例字段数据切片。
    fn pending_instance_data(&self, pending: PendingInstanceDump) -> Result<&[u8], HprofError> {
        let data_end = pending
            .data_start
            .checked_add(pending.data_len)
            .ok_or_else(|| HprofError::InvalidFormat("实例字段数据偏移溢出".to_string()))?;
        if data_end > self.bytes.len() {
            return Err(HprofError::InvalidFormat(format!(
                "实例 0x{:x} 字段数据越过文件边界",
                pending.object_id
            )));
        }
        Ok(&self.bytes[pending.data_start..data_end])
    }

    /// 以 HPROF INSTANCE_DUMP 字节顺序遍历实例字段。
    ///
    /// 边界条件：
    /// - 字段布局缺失或字段数据截断时返回格式错误；调用方可以把错误转成用户可见中文说明，而不是产生错位解析。
    fn for_each_instance_field(
        &self,
        class_id: HprofObjectId,
        data: &[u8],
        id_size: usize,
        mut visitor: impl FnMut(&str, u8, &[u8]),
    ) -> Result<(), HprofError> {
        let mut fields = Vec::new();
        collect_declared_fields(&self.state.raw_classes, class_id, &mut fields).ok_or_else(
            || HprofError::InvalidFormat(format!("缺少类 0x{class_id:x} 的实例字段布局")),
        )?;
        let mut offset = 0usize;
        for (_declaring_class_id, field) in fields {
            let width =
                field_value_size(field.field_type, self.identifier_size).ok_or_else(|| {
                    HprofError::InvalidFormat(format!("未知 HPROF 字段类型：{}", field.field_type))
                })? as usize;
            let end = offset.saturating_add(width);
            if end > data.len() {
                return Err(HprofError::InvalidFormat(format!(
                    "类 0x{class_id:x} 的实例字段数据截断"
                )));
            }
            if let Some(name) = self.graph_ref()?.strings.get(&field.name_id) {
                visitor(name, field.field_type, &data[offset..end]);
            }
            offset = end;
        }
        let _ = id_size;
        Ok(())
    }

    /// 从短 primitive array 中解码 Java String 的字符内容。
    fn decode_java_string_array(
        &self,
        array_id: HprofObjectId,
        coder: Option<u8>,
        offset: Option<usize>,
        count: Option<usize>,
    ) -> Option<String> {
        let array = self.state.pending_primitive_arrays.get(&array_id)?;
        let data_end = array.data_start.checked_add(array.data_len)?;
        let data = self.bytes.get(array.data_start..data_end)?;
        match array.element_type {
            FIELD_TYPE_CHAR => decode_char_array_string(data, array.length, offset, count),
            FIELD_TYPE_BYTE => decode_byte_array_string(data, array.length, coder, offset, count),
            _ => None,
        }
    }

    /// 报告 MAT 对象引用物化阶段进度。
    fn report_materialize_progress(
        &mut self,
        done: usize,
        total: usize,
        sub_message: &str,
    ) -> Result<(), HprofError> {
        if should_report_work(done, total) {
            self.refresh_progress_counts();
            self.progress.stage = HprofAnalysisStage::MaterializingReferences;
            self.progress.message = "正在构建 MAT 兼容引用边".to_string();
            self.progress.sub_message = sub_message.to_string();
            self.progress.phase_done = done as u64;
            self.progress.phase_total = total as u64;
            self.progress.phase_unit = "对象";
            self.report_progress();
        }
        Ok(())
    }

    /// 更新进度中的计数字段。
    fn refresh_progress_counts(&mut self) {
        if let Some(graph) = self.state.graph.as_ref() {
            self.progress.bytes_read = self.position as u64;
            self.progress.object_count = graph.object_count();
            self.progress.class_count = graph.class_count();
            self.progress.gc_root_count = graph.gc_roots.len();
            self.progress.edge_count = graph.edge_count();
        }
    }

    /// 按解析阶段报告进度。
    fn report_stage(&mut self, stage: HprofAnalysisStage, message: &str) -> Result<(), HprofError> {
        self.check_cancel()?;
        self.progress.stage = stage;
        self.progress.message = message.to_string();
        self.refresh_progress_counts();
        self.report_progress();
        Ok(())
    }

    /// 按节流规则报告进度。
    fn maybe_report_progress(&mut self) {
        if self
            .progress
            .record_count
            .is_multiple_of(HPROF_PROGRESS_RECORD_INTERVAL)
        {
            self.report_progress();
        }
    }

    /// 立即报告当前进度。
    fn report_progress(&mut self) {
        self.progress_rate.update(&mut self.progress);
        (self.progress_reporter)(self.progress.clone());
    }

    /// 检查取消状态。
    fn check_cancel(&self) -> Result<(), HprofError> {
        check_cancel(self.cancel_flag)
    }

    /// 判断是否已经读取到文件末尾。
    fn is_eof(&self) -> bool {
        self.position >= self.bytes.len()
    }

    /// 读取固定长度切片并推进偏移。
    fn read_slice(&mut self, len: usize) -> Result<&'a [u8], HprofError> {
        let end = self
            .position
            .checked_add(len)
            .ok_or_else(|| HprofError::InvalidFormat("HPROF 偏移计算溢出".to_string()))?;
        if end > self.bytes.len() {
            return Err(HprofError::InvalidFormat(format!(
                "HPROF 在偏移 {} 截断，需要读取 {} 字节",
                self.position, len
            )));
        }
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }

    /// 读取 `u8`。
    fn read_u8(&mut self) -> Result<u8, HprofError> {
        Ok(self.read_slice(1)?[0])
    }

    /// 读取大端 `u16`。
    fn read_u16(&mut self) -> Result<u16, HprofError> {
        let bytes = self.read_slice(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    /// 读取大端 `u32`。
    fn read_u32(&mut self) -> Result<u32, HprofError> {
        let bytes = self.read_slice(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// 读取大端 `i32`。
    fn read_i32(&mut self) -> Result<i32, HprofError> {
        let bytes = self.read_slice(4)?;
        Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// 读取大端 `u64`。
    fn read_u64(&mut self) -> Result<u64, HprofError> {
        let bytes = self.read_slice(8)?;
        Ok(u64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// 按 header 中的 ID 字节数读取对象 ID。
    fn read_id(&mut self) -> Result<HprofObjectId, HprofError> {
        match self.identifier_size {
            4 => Ok(u64::from(self.read_u32()?)),
            8 => self.read_u64(),
            other => Err(HprofError::InvalidFormat(format!(
                "不支持的对象 ID 字节数：{other}"
            ))),
        }
    }

    /// 跳过指定字节数。
    fn skip_bytes(&mut self, len: u64) -> Result<(), HprofError> {
        self.check_cancel()?;
        let len = usize::try_from(len).map_err(|_| {
            HprofError::InvalidFormat("HPROF 跳过长度超过当前平台可寻址范围".to_string())
        })?;
        self.read_slice(len)?;
        Ok(())
    }

    /// 跳过指定字段类型对应的值。
    fn skip_field_value(&mut self, field_type: u8) -> Result<(), HprofError> {
        let width = field_value_size(field_type, self.id_size()?).ok_or_else(|| {
            HprofError::InvalidFormat(format!("未知 HPROF 字段类型：{field_type}"))
        })?;
        self.skip_bytes(u64::from(width))
    }

    /// 返回 HPROF 对象 ID 字节数。
    fn id_size(&self) -> Result<u8, HprofError> {
        if matches!(self.identifier_size, 4 | 8) {
            Ok(self.identifier_size)
        } else {
            Err(HprofError::InvalidFormat(
                "内部错误：HPROF header 尚未读取".to_string(),
            ))
        }
    }

    /// 返回对象图不可变引用。
    fn graph_ref(&self) -> Result<&HprofObjectGraph, HprofError> {
        self.state
            .graph
            .as_ref()
            .ok_or_else(|| HprofError::InvalidFormat("内部错误：HPROF header 尚未读取".to_string()))
    }

    /// 返回对象图可变引用。
    fn graph_mut(&mut self) -> Result<&mut HprofObjectGraph, HprofError> {
        self.state
            .graph
            .as_mut()
            .ok_or_else(|| HprofError::InvalidFormat("内部错误：HPROF header 尚未读取".to_string()))
    }
}
