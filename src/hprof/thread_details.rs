//! HPROF 线程详情构造子模块。
//!
//! 业务意图：
//! - 该模块集中维护线程对象、START_THREAD、ROOT_THREAD_OBJECT、STACK_TRACE 和 STACK_FRAME
//!   之间的关联拼装逻辑，避免 dominator 结果构造代码继续承载 UI 线程详情规则。
//! - 线程详情仅服务于 HPROF 分析结果展示，不改变 retained size、sidecar schema 或外部分析入口。
//!
//! 边界条件：
//! - HPROF 可能缺少线程名、堆栈、类名或状态字段；所有缺失值继续保留原有 fallback 文案。
//! - 不同 JVM 对线程状态位的扩展不完全一致，未知位必须展示为十六进制，避免误导用户。

use super::*;

/// 根据解析阶段保留的线程记录构造线程详情索引。
///
/// 业务意图：
/// - 线程详情依赖对象字段、START_THREAD、ROOT_THREAD_OBJECT、STACK_TRACE、STACK_FRAME 和 dominator retained size；
///   在结果构造阶段统一拼装，可以避免 UI 层重复理解 HPROF 多张表之间的关联。
pub(super) fn build_thread_details(
    graph: &HprofObjectGraph,
    retained_sizes: &[u64],
    reachable: &[bool],
) -> FxHashMap<HprofObjectId, HprofThreadDetails> {
    let mut details = FxHashMap::default();
    for (object_id, instance) in &graph.thread_instances {
        let Some(object_index) = graph.object_index(*object_id) else {
            continue;
        };
        if !reachable.get(object_index).copied().unwrap_or(false) {
            continue;
        }
        let object = &graph.objects[object_index];
        let class_name = graph
            .classes
            .get(&instance.class_id)
            .and_then(|class_info| class_info.name.clone())
            .unwrap_or_else(|| format!("<unknown class 0x{:x}>", instance.class_id));
        let (thread_serial, stack_trace_serial) =
            thread_serial_and_stack_for_object(graph, *object_id);
        let thread_name = thread_name_for_instance(graph, instance, thread_serial);
        let retained_size = retained_sizes.get(object_index).copied().unwrap_or(0);
        let stack_frames = stack_frames_for_thread(graph, thread_serial, stack_trace_serial);
        let stack_message = if stack_frames.is_empty() {
            Some("该 HPROF 未包含此线程的堆栈信息".to_string())
        } else {
            None
        };
        let properties = thread_properties_for_instance(
            graph,
            instance,
            &class_name,
            &thread_name,
            object.shallow_size,
            retained_size,
        );
        details.insert(
            *object_id,
            HprofThreadDetails {
                object_id: *object_id,
                class_name,
                thread_name,
                properties,
                stack_frames,
                stack_message,
            },
        );
    }
    details
}

/// 返回线程对象对应的 thread serial 和 stack trace serial。
fn thread_serial_and_stack_for_object(
    graph: &HprofObjectGraph,
    object_id: HprofObjectId,
) -> (Option<u32>, Option<u32>) {
    let serial = graph.thread_serial_by_object_id.get(&object_id).copied();
    let start_stack = serial
        .and_then(|serial| graph.thread_starts.get(&serial))
        .map(|record| record.stack_trace_serial)
        .filter(|serial| *serial != 0);
    let root_stack = graph
        .thread_object_roots
        .get(&object_id)
        .map(|record| record.stack_trace_serial)
        .filter(|serial| *serial != 0);
    (serial, start_stack.or(root_stack))
}

/// 返回线程名。
fn thread_name_for_instance(
    graph: &HprofObjectGraph,
    instance: &HprofThreadInstanceInfo,
    thread_serial: Option<u32>,
) -> String {
    if let Some(name) = thread_serial
        .and_then(|serial| graph.thread_starts.get(&serial))
        .and_then(|record| graph.strings.get(&record.thread_name_id))
    {
        return name.clone();
    }
    if let Some(name) = instance
        .name_object_id
        .and_then(|object_id| graph.java_string_values.get(&object_id))
    {
        return name.clone();
    }
    "<unknown>".to_string()
}

/// 构造线程属性表。
fn thread_properties_for_instance(
    graph: &HprofObjectGraph,
    instance: &HprofThreadInstanceInfo,
    class_name: &str,
    thread_name: &str,
    shallow_size: u64,
    retained_size: u64,
) -> Vec<HprofThreadProperty> {
    let state_value = instance
        .thread_status
        .map(|status| format!("0x{status:x}"))
        .unwrap_or_else(|| "<unknown>".to_string());
    vec![
        HprofThreadProperty {
            name: "Object / Stack Frame".to_string(),
            value: format!("{class_name} @ 0x{:x}", instance.object_id),
        },
        HprofThreadProperty {
            name: "Name".to_string(),
            value: thread_name.to_string(),
        },
        HprofThreadProperty {
            name: "Shallow Heap".to_string(),
            value: format_hprof_bytes(shallow_size),
        },
        HprofThreadProperty {
            name: "Retained Heap".to_string(),
            value: format_hprof_bytes(retained_size),
        },
        HprofThreadProperty {
            name: "Max. Locals' Retained Heap".to_string(),
            value: "暂未计算".to_string(),
        },
        HprofThreadProperty {
            name: "Context Class Loader".to_string(),
            value: instance
                .context_class_loader_id
                .map(|object_id| object_label(graph, object_id))
                .unwrap_or_else(|| "<unknown>".to_string()),
        },
        HprofThreadProperty {
            name: "Is Daemon".to_string(),
            value: instance
                .daemon
                .map(|daemon| daemon.to_string())
                .unwrap_or_else(|| "<unknown>".to_string()),
        },
        HprofThreadProperty {
            name: "Priority".to_string(),
            value: instance
                .priority
                .map(|priority| priority.to_string())
                .unwrap_or_else(|| "<unknown>".to_string()),
        },
        HprofThreadProperty {
            name: "State".to_string(),
            value: instance
                .thread_status
                .map(describe_thread_status)
                .unwrap_or_else(|| "<unknown>".to_string()),
        },
        HprofThreadProperty {
            name: "State value".to_string(),
            value: state_value,
        },
    ]
}

/// 返回对象的 MAT 风格简短展示名。
fn object_label(graph: &HprofObjectGraph, object_id: HprofObjectId) -> String {
    let Some(object_index) = graph.object_index(object_id) else {
        return format!("0x{object_id:x}");
    };
    let object = &graph.objects[object_index];
    let class_name = match &object.kind {
        HprofObjectKind::Class => graph
            .classes
            .get(&object.id)
            .and_then(|class_info| class_info.name.as_deref())
            .map(|name| format!("Class {name}"))
            .unwrap_or_else(|| "Class".to_string()),
        HprofObjectKind::PrimitiveArray { element_type, .. } => {
            format!("{}[]", primitive_type_label(*element_type))
        }
        HprofObjectKind::ObjectArray { .. } | HprofObjectKind::Instance => graph
            .classes
            .get(&object.class_id)
            .and_then(|class_info| class_info.name.clone())
            .unwrap_or_else(|| format!("<unknown class 0x{:x}>", object.class_id)),
    };
    format!("{class_name} @ 0x{object_id:x}")
}

/// 根据 HotSpot/MAT 常见位标记解码线程状态。
///
/// 边界条件：
/// - 不同 JVM 版本可能保留额外状态位；未知位会保留十六进制，避免误导用户。
fn describe_thread_status(status: u32) -> String {
    let mut labels = Vec::new();
    let known_flags = [
        (0x0001, "alive"),
        (0x0002, "terminated"),
        (0x0004, "runnable"),
        (0x0010, "waiting indefinitely"),
        (0x0020, "waiting with timeout"),
        (0x0040, "sleeping"),
        (0x0080, "waiting"),
        (0x0100, "in object wait"),
        (0x0200, "parked"),
        (0x0400, "blocked on monitor enter"),
        (0x100000, "suspended"),
        (0x200000, "interrupted"),
        (0x400000, "in native"),
    ];
    let mut known_mask = 0u32;
    for (flag, label) in known_flags {
        known_mask |= flag;
        if status & flag != 0 {
            labels.push(label.to_string());
        }
    }
    let unknown = status & !known_mask;
    if unknown != 0 {
        labels.push(format!("unknown bits 0x{unknown:x}"));
    }
    if labels.is_empty() {
        "unknown".to_string()
    } else {
        labels.join(", ")
    }
}

/// 返回线程栈帧列表。
fn stack_frames_for_thread(
    graph: &HprofObjectGraph,
    thread_serial: Option<u32>,
    stack_trace_serial: Option<u32>,
) -> Vec<HprofThreadStackFrame> {
    let stack_trace = stack_trace_serial
        .and_then(|serial| graph.stack_traces.get(&serial))
        .or_else(|| {
            thread_serial.and_then(|thread_serial| {
                graph
                    .stack_traces
                    .values()
                    .find(|trace| trace.thread_serial == thread_serial)
            })
        });
    let Some(stack_trace) = stack_trace else {
        return Vec::new();
    };
    stack_trace
        .frame_ids
        .iter()
        .filter_map(|frame_id| graph.stack_frames.get(frame_id))
        .map(|frame| stack_frame_for_record(graph, frame))
        .collect()
}

/// 把原始 STACK_FRAME 记录转换成 UI 栈帧。
fn stack_frame_for_record(
    graph: &HprofObjectGraph,
    frame: &HprofStackFrameRecord,
) -> HprofThreadStackFrame {
    let class_name = graph
        .loaded_classes
        .get(&frame.class_serial)
        .and_then(|(class_id, _)| graph.classes.get(class_id))
        .and_then(|class_info| class_info.name.clone())
        .or_else(|| {
            graph
                .loaded_classes
                .get(&frame.class_serial)
                .and_then(|(_, name_id)| graph.strings.get(name_id))
                .map(|name| normalize_java_class_name(name))
        })
        .unwrap_or_else(|| format!("<unknown class serial {}>", frame.class_serial));
    let method_name = graph
        .strings
        .get(&frame.method_name_id)
        .cloned()
        .unwrap_or_else(|| "<unknown>".to_string());
    let method_signature = graph
        .strings
        .get(&frame.method_signature_id)
        .cloned()
        .unwrap_or_default();
    let source_file = graph
        .strings
        .get(&frame.source_file_id)
        .cloned()
        .unwrap_or_else(|| "Unknown Source".to_string());
    let source = stack_frame_source_label(&source_file, frame.line_number);
    let display = format!("at {class_name}.{method_name}{method_signature} ({source})");
    HprofThreadStackFrame {
        class_name,
        method_name,
        method_signature,
        source,
        line_number: frame.line_number,
        display,
    }
}

/// 返回栈帧源文件和行号展示文案。
fn stack_frame_source_label(source_file: &str, line_number: i32) -> String {
    match line_number {
        -3 => "Native Method".to_string(),
        -2 => "Compiled Method".to_string(),
        line if line > 0 => format!("{source_file}:{line}"),
        _ => "Unknown Source".to_string(),
    }
}
