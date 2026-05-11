//! HPROF 解析和 dominator tree 纯逻辑测试。
//!
//! 业务意图：
//! - 这些测试不依赖 GPUI 窗口系统，直接锁定文件校验、对象引用解析和 retained size 规则。
//! - 真实生产 dump 通常很大，单元测试使用最小合法 HPROF fixture 覆盖边界行为。

use super::*;
use std::{collections::HashSet, env, fs};

/// 构造唯一临时文件路径。
fn test_file_path(name: &str, extension: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "logclinic3-hprof-test-{}-{}.{}",
        std::process::id(),
        name,
        extension
    ))
}

/// 写入测试文件并返回路径。
fn write_test_file(name: &str, extension: &str, bytes: &[u8]) -> PathBuf {
    let path = test_file_path(name, extension);
    fs::write(&path, bytes).expect("测试 HPROF 文件应能写入临时目录");
    path
}

/// 返回测试文件对应的 sidecar 目录。
fn test_sidecar_dir(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("dump");
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{file_name}.logclinic-hprof"))
}

/// 清理测试文件及其 sidecar 目录。
fn cleanup_test_file(path: &Path) {
    let _ = fs::remove_file(path);
    let _ = fs::remove_dir_all(test_sidecar_dir(path));
}

/// 写入 4 字节 ID 的 HPROF header。
fn push_header(bytes: &mut Vec<u8>) {
    push_header_with_id_size(bytes, 4);
}

/// 写入指定 ID 宽度的 HPROF header。
fn push_header_with_id_size(bytes: &mut Vec<u8>, identifier_size: u32) {
    bytes.extend_from_slice(b"JAVA PROFILE 1.0.2\0");
    bytes.extend_from_slice(&identifier_size.to_be_bytes());
    bytes.extend_from_slice(&0u64.to_be_bytes());
}

/// 写入 HPROF 顶层记录。
fn push_record(bytes: &mut Vec<u8>, tag: u8, body: Vec<u8>) {
    bytes.push(tag);
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(&(body.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&body);
}

/// 写入 4 字节对象 ID。
fn push_id(bytes: &mut Vec<u8>, id: u32) {
    bytes.extend_from_slice(&id.to_be_bytes());
}

/// 构造 STRING_IN_UTF8 记录体。
fn string_body(id: u32, value: &str) -> Vec<u8> {
    let mut body = Vec::new();
    push_id(&mut body, id);
    body.extend_from_slice(value.as_bytes());
    body
}

/// 构造 LOAD_CLASS 记录体。
fn load_class_body(serial: u32, class_id: u32, name_id: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&serial.to_be_bytes());
    push_id(&mut body, class_id);
    body.extend_from_slice(&0u32.to_be_bytes());
    push_id(&mut body, name_id);
    body
}

/// 构造 START_THREAD 记录体。
fn start_thread_body(
    thread_serial: u32,
    object_id: u32,
    stack_trace_serial: u32,
    thread_name_id: u32,
) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&thread_serial.to_be_bytes());
    push_id(&mut body, object_id);
    body.extend_from_slice(&stack_trace_serial.to_be_bytes());
    push_id(&mut body, thread_name_id);
    push_id(&mut body, 0);
    push_id(&mut body, 0);
    body
}

/// 构造 STACK_FRAME 记录体。
fn stack_frame_body(
    frame_id: u32,
    method_name_id: u32,
    signature_id: u32,
    source_id: u32,
    class_serial: u32,
    line_number: i32,
) -> Vec<u8> {
    let mut body = Vec::new();
    push_id(&mut body, frame_id);
    push_id(&mut body, method_name_id);
    push_id(&mut body, signature_id);
    push_id(&mut body, source_id);
    body.extend_from_slice(&class_serial.to_be_bytes());
    body.extend_from_slice(&line_number.to_be_bytes());
    body
}

/// 构造 STACK_TRACE 记录体。
fn stack_trace_body(stack_trace_serial: u32, thread_serial: u32, frame_ids: &[u32]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&stack_trace_serial.to_be_bytes());
    body.extend_from_slice(&thread_serial.to_be_bytes());
    body.extend_from_slice(&(frame_ids.len() as u32).to_be_bytes());
    for frame_id in frame_ids {
        push_id(&mut body, *frame_id);
    }
    body
}

/// 构造单字段 CLASS_DUMP 子记录。
fn class_dump_subrecord(
    class_id: u32,
    super_id: u32,
    instance_size: u32,
    field_name_id: u32,
    field_type: u8,
) -> Vec<u8> {
    class_dump_subrecord_fields(
        class_id,
        super_id,
        instance_size,
        &[],
        &[(field_name_id, field_type)],
    )
}

/// 构造可指定静态字段和实例字段的 CLASS_DUMP 子记录。
fn class_dump_subrecord_fields(
    class_id: u32,
    super_id: u32,
    instance_size: u32,
    static_object_fields: &[(u32, u32)],
    instance_fields: &[(u32, u8)],
) -> Vec<u8> {
    class_dump_subrecord_fields_with_loader(
        class_id,
        super_id,
        0,
        instance_size,
        static_object_fields,
        instance_fields,
    )
}

/// 构造可指定类加载器、静态字段和实例字段的 CLASS_DUMP 子记录。
fn class_dump_subrecord_fields_with_loader(
    class_id: u32,
    super_id: u32,
    class_loader_id: u32,
    instance_size: u32,
    static_object_fields: &[(u32, u32)],
    instance_fields: &[(u32, u8)],
) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(SUB_CLASS_DUMP);
    push_id(&mut body, class_id);
    body.extend_from_slice(&0u32.to_be_bytes());
    push_id(&mut body, super_id);
    push_id(&mut body, class_loader_id);
    push_id(&mut body, 0);
    push_id(&mut body, 0);
    push_id(&mut body, 0);
    push_id(&mut body, 0);
    body.extend_from_slice(&instance_size.to_be_bytes());
    body.extend_from_slice(&0u16.to_be_bytes());
    body.extend_from_slice(&(static_object_fields.len() as u16).to_be_bytes());
    for (field_name_id, reference_id) in static_object_fields {
        push_id(&mut body, *field_name_id);
        body.push(FIELD_TYPE_OBJECT);
        push_id(&mut body, *reference_id);
    }
    body.extend_from_slice(&(instance_fields.len() as u16).to_be_bytes());
    for (field_name_id, field_type) in instance_fields {
        push_id(&mut body, *field_name_id);
        body.push(*field_type);
    }
    body
}

/// 构造单对象字段 INSTANCE_DUMP 子记录。
fn instance_dump_subrecord(object_id: u32, class_id: u32, field_reference: u32) -> Vec<u8> {
    instance_dump_subrecord_data(object_id, class_id, &field_reference.to_be_bytes())
}

/// 构造指定字段数据的 INSTANCE_DUMP 子记录。
fn instance_dump_subrecord_data(object_id: u32, class_id: u32, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(SUB_INSTANCE_DUMP);
    push_id(&mut body, object_id);
    body.extend_from_slice(&0u32.to_be_bytes());
    push_id(&mut body, class_id);
    body.extend_from_slice(&(data.len() as u32).to_be_bytes());
    body.extend_from_slice(data);
    body
}

/// 构造 OBJECT_ARRAY_DUMP 子记录。
fn object_array_subrecord(array_id: u32, class_id: u32, references: &[u32]) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(SUB_OBJECT_ARRAY_DUMP);
    push_id(&mut body, array_id);
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&(references.len() as u32).to_be_bytes());
    push_id(&mut body, class_id);
    for reference in references {
        push_id(&mut body, *reference);
    }
    body
}

/// 构造 PRIMITIVE_ARRAY_DUMP 子记录。
fn primitive_array_subrecord(array_id: u32, element_type: u8, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(SUB_PRIMITIVE_ARRAY_DUMP);
    push_id(&mut body, array_id);
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&(data.len() as u32).to_be_bytes());
    body.push(element_type);
    body.extend_from_slice(data);
    body
}

/// 构造 ROOT_UNKNOWN 子记录。
fn root_unknown_subrecord(object_id: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(SUB_ROOT_UNKNOWN);
    push_id(&mut body, object_id);
    body
}

/// 构造 ROOT_THREAD_OBJECT 子记录。
fn root_thread_object_subrecord(
    object_id: u32,
    thread_serial: u32,
    stack_trace_serial: u32,
) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(SUB_ROOT_THREAD_OBJECT);
    push_id(&mut body, object_id);
    body.extend_from_slice(&thread_serial.to_be_bytes());
    body.extend_from_slice(&stack_trace_serial.to_be_bytes());
    body
}

/// 构造最小 HPROF fixture。
fn minimal_hprof_fixture() -> Vec<u8> {
    let mut bytes = Vec::new();
    push_header(&mut bytes);
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(1, "com/example/Node"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "next"));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));

    let mut heap = Vec::new();
    heap.extend(class_dump_subrecord(100, 0, 4, 2, FIELD_TYPE_OBJECT));
    heap.extend(root_unknown_subrecord(1));
    heap.extend(instance_dump_subrecord(1, 100, 2));
    heap.extend(instance_dump_subrecord(2, 100, 0));
    heap.extend(object_array_subrecord(3, 100, &[1, 2]));
    heap.extend(primitive_array_subrecord(4, FIELD_TYPE_BYTE, &[1, 2, 3]));
    push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
    bytes
}

/// 构造大量实例组成的链式 HPROF fixture。
fn synthetic_chain_hprof_fixture(instance_count: u32) -> Vec<u8> {
    let class_id = 0x7fff_0000;
    let mut bytes = Vec::new();
    push_header(&mut bytes);
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(1, "com/example/Node"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "next"));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, class_id, 1));

    let mut heap = Vec::new();
    heap.extend(class_dump_subrecord(class_id, 0, 4, 2, FIELD_TYPE_OBJECT));
    heap.extend(root_unknown_subrecord(1));
    for object_id in 1..=instance_count {
        let next = if object_id == instance_count {
            0
        } else {
            object_id + 1
        };
        heap.extend(instance_dump_subrecord(object_id, class_id, next));
    }
    push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
    bytes
}

/// 构造包含 WeakReference.referent 的 HPROF fixture。
fn weak_reference_hprof_fixture() -> Vec<u8> {
    let mut bytes = Vec::new();
    push_header(&mut bytes);
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(1, "java/lang/ref/Reference"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "referent"));
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(3, "java/lang/ref/WeakReference"),
    );
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(4, "com/example/Target"),
    );
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 3));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(3, 300, 4));

    let mut heap = Vec::new();
    heap.extend(class_dump_subrecord(100, 0, 4, 2, FIELD_TYPE_OBJECT));
    heap.extend(class_dump_subrecord_fields(200, 100, 4, &[], &[]));
    heap.extend(class_dump_subrecord_fields(300, 0, 0, &[], &[]));
    heap.extend(root_unknown_subrecord(10));
    heap.extend(instance_dump_subrecord(10, 200, 20));
    heap.extend(instance_dump_subrecord_data(20, 300, &[]));
    push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
    bytes
}

/// 构造 ThreadLocalMap.Entry 继承 WeakReference 的 HPROF fixture。
fn thread_local_entry_hprof_fixture() -> Vec<u8> {
    let mut bytes = Vec::new();
    push_header(&mut bytes);
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(1, "java/lang/ref/Reference"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "referent"));
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(3, "java/lang/ref/WeakReference"),
    );
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(4, "java/lang/ThreadLocal$ThreadLocalMap$Entry"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(5, "value"));
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(6, "com/example/Target"),
    );
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 3));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(3, 300, 4));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(4, 400, 6));

    let mut heap = Vec::new();
    heap.extend(class_dump_subrecord(100, 0, 4, 2, FIELD_TYPE_OBJECT));
    heap.extend(class_dump_subrecord_fields(200, 100, 4, &[], &[]));
    heap.extend(class_dump_subrecord(300, 200, 8, 5, FIELD_TYPE_OBJECT));
    heap.extend(class_dump_subrecord_fields(400, 0, 0, &[], &[]));
    heap.extend(root_unknown_subrecord(10));
    let mut entry_data = Vec::new();
    push_id(&mut entry_data, 20);
    push_id(&mut entry_data, 30);
    heap.extend(instance_dump_subrecord_data(10, 300, &entry_data));
    heap.extend(instance_dump_subrecord_data(20, 400, &[]));
    heap.extend(instance_dump_subrecord_data(30, 400, &[]));
    push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
    bytes
}

/// 构造静态字段强引用 HPROF fixture。
fn static_field_hprof_fixture() -> Vec<u8> {
    let mut bytes = Vec::new();
    push_header(&mut bytes);
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(1, "com/example/Statics"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "cached"));
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(3, "com/example/Target"),
    );
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 3));

    let mut heap = Vec::new();
    heap.extend(class_dump_subrecord_fields(100, 0, 0, &[(2, 20)], &[]));
    heap.extend(class_dump_subrecord_fields(200, 0, 0, &[], &[]));
    heap.extend(root_unknown_subrecord(100));
    heap.extend(instance_dump_subrecord_data(20, 200, &[]));
    push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
    bytes
}

/// 构造对象数组强引用 HPROF fixture。
fn object_array_root_hprof_fixture() -> Vec<u8> {
    let mut bytes = Vec::new();
    push_header(&mut bytes);
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(1, "com/example/Target"),
    );
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));

    let mut heap = Vec::new();
    heap.extend(class_dump_subrecord_fields(100, 0, 0, &[], &[]));
    heap.extend(root_unknown_subrecord(50));
    heap.extend(object_array_subrecord(50, 100, &[20]));
    heap.extend(instance_dump_subrecord_data(20, 100, &[]));
    push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
    bytes
}

/// 构造 class loader 通过 loaded class 和 static 字段保活对象的 HPROF fixture。
fn class_loader_static_hprof_fixture() -> Vec<u8> {
    let mut bytes = Vec::new();
    push_header(&mut bytes);
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(1, "com/example/CacheStatics"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "cache"));
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(3, "com/example/Target"),
    );
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(4, "com/example/Loader"),
    );
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 3));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(3, 300, 4));

    let mut heap = Vec::new();
    heap.extend(class_dump_subrecord_fields_with_loader(
        100,
        0,
        10,
        0,
        &[(2, 20)],
        &[],
    ));
    heap.extend(class_dump_subrecord_fields(200, 0, 0, &[], &[]));
    heap.extend(class_dump_subrecord_fields(300, 0, 0, &[], &[]));
    heap.extend(root_unknown_subrecord(10));
    heap.extend(instance_dump_subrecord_data(10, 300, &[]));
    heap.extend(instance_dump_subrecord_data(20, 200, &[]));
    push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
    bytes
}

/// 构造 bootstrap/system class 通过 static 字段保活对象的 HPROF fixture。
fn bootstrap_static_hprof_fixture() -> Vec<u8> {
    let mut bytes = Vec::new();
    push_header(&mut bytes);
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(1, "com/example/BootstrapStatics"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "cache"));
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(3, "com/example/Target"),
    );
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 3));

    let mut heap = Vec::new();
    heap.extend(class_dump_subrecord_fields(100, 0, 0, &[(2, 20)], &[]));
    heap.extend(class_dump_subrecord_fields(200, 0, 0, &[], &[]));
    heap.extend(instance_dump_subrecord_data(20, 200, &[]));
    push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
    bytes
}

/// 构造子类 primitive 字段位于父类 object 字段之前的继承 fixture。
fn inherited_field_order_hprof_fixture() -> Vec<u8> {
    let mut bytes = Vec::new();
    push_header(&mut bytes);
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(1, "com/example/Parent"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "parentRef"));
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(3, "com/example/Child"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(4, "childInt"));
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(5, "com/example/Target"),
    );
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 3));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(3, 300, 5));

    let mut heap = Vec::new();
    heap.extend(class_dump_subrecord(100, 0, 4, 2, FIELD_TYPE_OBJECT));
    heap.extend(class_dump_subrecord(200, 100, 8, 4, FIELD_TYPE_INT));
    heap.extend(class_dump_subrecord_fields(300, 0, 0, &[], &[]));
    heap.extend(root_unknown_subrecord(10));
    let mut child_data = Vec::new();
    child_data.extend_from_slice(&1234u32.to_be_bytes());
    push_id(&mut child_data, 20);
    heap.extend(instance_dump_subrecord_data(10, 200, &child_data));
    heap.extend(instance_dump_subrecord_data(20, 300, &[]));
    push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
    bytes
}

/// 构造包含线程属性和堆栈的 HPROF fixture。
fn thread_details_hprof_fixture(include_start_thread: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    push_header(&mut bytes);
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(1, "java/lang/Thread"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "name"));
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(3, "priority"));
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(4, "daemon"));
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(5, "threadStatus"),
    );
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(6, "contextClassLoader"),
    );
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(7, "com/example/Loader"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(8, "worker-1"));
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(9, "java/util/concurrent/locks/LockSupport"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(10, "park"));
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(11, "(Ljava/lang/Object;)V"),
    );
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(12, "LockSupport.java"),
    );
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 200, 7));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(3, 300, 9));
    if include_start_thread {
        push_record(
            &mut bytes,
            TAG_START_THREAD,
            start_thread_body(7, 10, 70, 8),
        );
    }
    push_record(
        &mut bytes,
        TAG_STACK_FRAME,
        stack_frame_body(400, 10, 11, 12, 3, 175),
    );
    push_record(&mut bytes, TAG_STACK_TRACE, stack_trace_body(70, 7, &[400]));

    let mut heap = Vec::new();
    heap.extend(class_dump_subrecord_fields(
        100,
        0,
        17,
        &[],
        &[
            (2, FIELD_TYPE_OBJECT),
            (3, FIELD_TYPE_INT),
            (4, FIELD_TYPE_BOOLEAN),
            (5, FIELD_TYPE_INT),
            (6, FIELD_TYPE_OBJECT),
        ],
    ));
    heap.extend(class_dump_subrecord_fields(200, 0, 0, &[], &[]));
    heap.extend(root_thread_object_subrecord(10, 7, 70));
    let mut thread_data = Vec::new();
    push_id(&mut thread_data, 0);
    thread_data.extend_from_slice(&5u32.to_be_bytes());
    thread_data.push(1);
    thread_data.extend_from_slice(&0x291u32.to_be_bytes());
    push_id(&mut thread_data, 20);
    heap.extend(instance_dump_subrecord_data(10, 100, &thread_data));
    heap.extend(instance_dump_subrecord_data(20, 200, &[]));
    push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
    bytes
}

/// 构造 `java.lang.Thread` 子类的 HPROF fixture。
fn thread_subclass_hprof_fixture() -> Vec<u8> {
    let mut bytes = Vec::new();
    push_header(&mut bytes);
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(1, "java/lang/Thread"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(2, "priority"));
    push_record(
        &mut bytes,
        TAG_STRING_IN_UTF8,
        string_body(3, "com/example/WorkerThread"),
    );
    push_record(&mut bytes, TAG_STRING_IN_UTF8, string_body(4, "worker-sub"));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(1, 100, 1));
    push_record(&mut bytes, TAG_LOAD_CLASS, load_class_body(2, 110, 3));
    push_record(&mut bytes, TAG_START_THREAD, start_thread_body(8, 11, 0, 4));

    let mut heap = Vec::new();
    heap.extend(class_dump_subrecord(100, 0, 4, 2, FIELD_TYPE_INT));
    heap.extend(class_dump_subrecord_fields(110, 100, 4, &[], &[]));
    heap.extend(root_thread_object_subrecord(11, 8, 0));
    heap.extend(instance_dump_subrecord_data(11, 110, &5u32.to_be_bytes()));
    push_record(&mut bytes, TAG_HEAP_DUMP_SEGMENT, heap);
    bytes
}

/// 验证 `.hprof` 和 `.bin` 后缀都允许，其他后缀拒绝。
#[test]
fn 文件选择只接受_hprof_和_bin后缀() {
    assert!(hprof_path_has_supported_extension(Path::new("heap.hprof")));
    assert!(hprof_path_has_supported_extension(Path::new("heap.bin")));
    assert!(!hprof_path_has_supported_extension(Path::new("heap.txt")));
}

/// 验证 `.hprof` 文件同样需要通过真实 header 校验。
#[test]
fn hprof文件必须是合法_header() {
    let valid_path = write_test_file("valid-hprof", "hprof", &minimal_hprof_fixture());
    assert!(validate_hprof_file_selection(&valid_path).is_ok());

    let invalid_path = write_test_file("invalid-hprof", "hprof", b"not hprof");
    assert!(validate_hprof_file_selection(&invalid_path).is_err());
    let _ = fs::remove_file(valid_path);
    let _ = fs::remove_file(invalid_path);
}

/// 验证 `.bin` 改名文件仍必须通过 HPROF header 校验。
#[test]
fn bin文件必须是合法_hprof_header() {
    let valid_path = write_test_file("valid-bin", "bin", &minimal_hprof_fixture());
    assert!(validate_hprof_file_selection(&valid_path).is_ok());

    let invalid_path = write_test_file("invalid-bin", "bin", b"not hprof");
    assert!(validate_hprof_file_selection(&invalid_path).is_err());
    let _ = fs::remove_file(valid_path);
    let _ = fs::remove_file(invalid_path);
}

/// 验证非法后缀即使内容像 HPROF 也会在选择校验阶段被拒绝。
#[test]
fn 非法后缀会被拒绝() {
    let path = write_test_file("invalid-extension", "txt", &minimal_hprof_fixture());
    assert!(validate_hprof_file_selection(&path).is_err());
    let _ = fs::remove_file(path);
}

/// 验证截断 header 会返回格式错误。
#[test]
fn 截断header会被拒绝() {
    let path = write_test_file("truncated", "hprof", b"JAVA PROFILE 1.0.2");
    assert!(validate_hprof_file_selection(&path).is_err());
    let _ = fs::remove_file(path);
}

/// 验证最小 fixture 可以解析类名、实例引用、对象数组和基础数组。
#[test]
fn 最小fixture可以解析对象图() {
    let path = write_test_file("minimal", "hprof", &minimal_hprof_fixture());
    let mut progresses = Vec::new();
    let result = analyze_hprof_dominator_tree(
        path.clone(),
        |progress| progresses.push(progress),
        Arc::new(AtomicBool::new(false)),
    )
    .expect("最小 HPROF fixture 应解析成功");

    assert_eq!(result.header.label, "JAVA PROFILE 1.0.2");
    assert_eq!(result.total_classes, 1);
    assert_eq!(result.total_objects, 5);
    assert_eq!(result.gc_root_count, 2);
    assert_eq!(result.synthetic_bootstrap_class_root_count, 1);
    assert_eq!(
        result.node_for_object_id(1).unwrap().name,
        "com.example.Node"
    );
    assert!(
        progresses
            .iter()
            .any(|progress| progress.stage == HprofAnalysisStage::ReadingRecords)
    );
    let _ = fs::remove_file(path);
}

/// 验证优化后的紧凑结果仍保留第一版 fixture 的核心语义。
#[test]
fn 最小fixture紧凑结果保持等价() {
    let path = write_test_file("equivalence", "hprof", &minimal_hprof_fixture());
    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("最小 HPROF fixture 应解析成功");

    assert_eq!(result.edge_count, 3);
    assert_eq!(result.raw_edge_count, 3);
    assert_eq!(result.reference_edge_stats.total(), 0);
    assert_eq!(result.node_for_object_id(1).unwrap().retained_size, 32);
    assert_eq!(result.node_for_object_id(2).unwrap().retained_size, 16);
    assert_eq!(result.reachable_shallow_size, 32);
    assert_eq!(result.top_object_ids.first().copied(), Some(1));
    let _ = fs::remove_file(path);
}

/// 验证 WeakReference.referent 会参与 MAT dominator 对象图，同时单独统计为 weak-like referent。
#[test]
fn weak_reference_referent会参与mat对象图() {
    let path = write_test_file("weak-reference", "hprof", &weak_reference_hprof_fixture());
    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("WeakReference fixture 应解析成功");

    assert_eq!(result.reference_edge_stats.weak_like, 1);
    assert_eq!(result.reference_edge_stats.total(), 1);
    assert_eq!(result.raw_edge_count, result.edge_count);
    assert!(result.node_for_object_id(20).is_some());
    assert_eq!(result.node_for_object_id(10).unwrap().retained_size, 32);
    let _ = fs::remove_file(path);
}

/// 验证 ThreadLocalMap.Entry 继承 WeakReference 时 referent 和 value 都参与 MAT 对象图，referent 额外计入 weak-like 统计。
#[test]
fn threadlocal_entry会保留referent并统计弱引用边() {
    let path = write_test_file(
        "thread-local-entry",
        "hprof",
        &thread_local_entry_hprof_fixture(),
    );
    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("ThreadLocalMap.Entry fixture 应解析成功");

    assert_eq!(result.reference_edge_stats.weak_like, 1);
    assert_eq!(result.reference_edge_stats.total(), 1);
    assert!(result.node_for_object_id(20).is_some());
    assert!(result.node_for_object_id(30).is_some());
    assert_eq!(result.node_for_object_id(10).unwrap().retained_size, 56);
    let _ = fs::remove_file(path);
}

/// 验证静态字段仍按强引用处理，避免 MAT 兼容修正误伤 class object 到缓存对象的边。
#[test]
fn 静态对象字段仍然是强引用() {
    let path = write_test_file("static-field", "hprof", &static_field_hprof_fixture());
    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("静态字段 fixture 应解析成功");

    assert_eq!(result.reference_edge_stats.total(), 0);
    assert!(result.node_for_object_id(20).is_some());
    assert_eq!(result.node_for_object_id(100).unwrap().retained_size, 16);
    let _ = fs::remove_file(path);
}

/// 验证对象数组元素仍按强引用处理。
#[test]
fn 对象数组元素仍然是强引用() {
    let path = write_test_file(
        "object-array-root",
        "hprof",
        &object_array_root_hprof_fixture(),
    );
    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("对象数组 fixture 应解析成功");

    assert_eq!(result.reference_edge_stats.total(), 0);
    assert!(result.node_for_object_id(20).is_some());
    assert_eq!(result.node_for_object_id(50).unwrap().retained_size, 40);
    let _ = fs::remove_file(path);
}

/// 验证 MAT 兼容模式会合成 class loader 到 class object 的强引用边，使 static 字段对象进入 dominator tree。
#[test]
fn classloader会保活其加载类的static字段对象() {
    let path = write_test_file(
        "class-loader-static",
        "hprof",
        &class_loader_static_hprof_fixture(),
    );
    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("class loader static fixture 应解析成功");

    assert_eq!(result.synthetic_class_loader_edge_count, 1);
    assert_eq!(result.reference_edge_stats.total(), 0);
    assert!(result.node_for_object_id(100).is_some());
    assert!(result.node_for_object_id(20).is_some());
    assert_eq!(result.node_for_object_id(100).unwrap().retained_size, 16);
    let _ = fs::remove_file(path);
}

/// 验证 bootstrap/system class 会作为合成 root，使没有普通 class loader 对象的 static 字段仍可达。
#[test]
fn bootstrap_class会作为合成root保活static字段对象() {
    let path = write_test_file(
        "bootstrap-static",
        "hprof",
        &bootstrap_static_hprof_fixture(),
    );
    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("bootstrap static fixture 应解析成功");

    assert_eq!(result.synthetic_class_loader_edge_count, 0);
    assert_eq!(result.synthetic_bootstrap_class_root_count, 2);
    assert!(result.node_for_object_id(100).is_some());
    assert!(result.node_for_object_id(20).is_some());
    assert_eq!(result.node_for_object_id(100).unwrap().retained_size, 16);
    let _ = fs::remove_file(path);
}

/// 验证 MAT 兼容 shallow size 不把 HPROF ID 宽度误当作堆内引用宽度。
#[test]
fn mat_size_model会区分id宽度和堆引用宽度() {
    let header4 = HprofHeader {
        label: "JAVA PROFILE 1.0.2".to_string(),
        identifier_size: 4,
        timestamp_millis: 0,
    };
    let header8 = HprofHeader {
        label: "JAVA PROFILE 1.0.2".to_string(),
        identifier_size: 8,
        timestamp_millis: 0,
    };
    let model4 = HprofSizeModel::mat_compatible(&header4, 1024);
    let compressed_model8 = HprofSizeModel::mat_compatible(&header8, 1024);
    let wide_model8 = HprofSizeModel::mat_compatible(&header8, HPROF_COMPRESSED_OOPS_HEAP_LIMIT);

    assert_eq!(model4.reference_size, 4);
    assert_eq!(compressed_model8.reference_size, 4);
    assert!(compressed_model8.compressed_oops);
    assert_eq!(wide_model8.reference_size, 8);
    assert!(!wide_model8.compressed_oops);
    assert_eq!(instance_shallow_size(&compressed_model8, 17), 32);
    assert_eq!(object_array_shallow_size(&compressed_model8, 2), 24);
    assert_eq!(
        primitive_array_shallow_size(&compressed_model8, FIELD_TYPE_BYTE, 3).unwrap(),
        24
    );
    assert_eq!(object_array_shallow_size(&wide_model8, 2), 32);
}

/// 验证重复对象 ID 会被视为损坏 dump，避免 streaming/sidecar 路径出现覆盖语义。
#[test]
fn 重复对象id会返回格式错误() {
    let header = HprofHeader {
        label: "JAVA PROFILE 1.0.2".to_string(),
        identifier_size: 8,
        timestamp_millis: 0,
    };
    let size_model = HprofSizeModel::mat_compatible(&header, 1024);
    let mut graph = HprofObjectGraph::new(header, size_model);
    let object = HprofHeapObject {
        id: 1,
        class_id: 100,
        shallow_size: 8,
        kind: HprofObjectKind::Instance,
    };
    graph
        .insert_object(object.clone(), Vec::new())
        .expect("首次插入对象应成功");
    let error = graph
        .insert_object(object, Vec::new())
        .expect_err("重复对象 ID 应被拒绝");
    assert!(error.to_string().contains("重复定义"));
}

/// 验证低内存索引会拒绝超过 `u32` 哨兵上限的节点数。
#[test]
fn 低内存节点索引超过上限会返回中文错误() {
    let error = hprof_node_id(HPROF_INVALID_NODE as usize, "对象数量")
        .expect_err("u32 哨兵值必须保留给无效节点");
    assert!(error.to_string().contains("对象数量"));
    assert!(error.to_string().contains("低内存索引上限"));
}

/// 验证大于多个报告阈值的解析进度保持单调增长。
#[test]
fn 大量子记录进度单调增长() {
    let path = write_test_file("progress", "hprof", &synthetic_chain_hprof_fixture(5000));
    let mut progresses = Vec::new();
    let result = analyze_hprof_dominator_tree(
        path.clone(),
        |progress| progresses.push(progress),
        Arc::new(AtomicBool::new(false)),
    )
    .expect("合成链式 HPROF 应解析成功");

    let record_progresses = progresses
        .iter()
        .filter(|progress| progress.stage == HprofAnalysisStage::ReadingRecords)
        .collect::<Vec<_>>();
    assert!(record_progresses.len() >= 2);
    for window in record_progresses.windows(2) {
        assert!(window[1].bytes_read >= window[0].bytes_read);
        assert!(window[1].record_count >= window[0].record_count);
        assert!(window[1].object_count >= window[0].object_count);
        assert!(window[1].edge_count >= window[0].edge_count);
    }
    assert_eq!(result.total_objects, 5001);
    assert_eq!(result.edge_count, 4999);
    let _ = fs::remove_file(path);
}

/// 验证紧凑图生成和 LT 算法都会输出可展示的细分阶段进度。
#[test]
fn 计算阶段会报告紧凑图和lt细分进度() {
    let path = write_test_file(
        "phase-progress",
        "hprof",
        &synthetic_chain_hprof_fixture(32),
    );
    let mut progresses = Vec::new();
    analyze_hprof_dominator_tree(
        path.clone(),
        |progress| progresses.push(progress),
        Arc::new(AtomicBool::new(false)),
    )
    .expect("合成 HPROF 应解析成功");

    for expected in [
        "添加紧凑图节点",
        "添加 GC Root 边",
        "添加对象引用边",
        "DFS 编号",
        "构建前驱索引",
        "LT 主循环",
        "修正 immediate dominator",
        "后序聚合 retained size",
    ] {
        assert!(
            progresses
                .iter()
                .any(|progress| progress.sub_message == expected),
            "缺少阶段进度：{expected}"
        );
    }
    assert!(progresses.iter().any(|progress| {
        progress.sub_message == "添加对象引用边"
            && progress.phase_total > 0
            && progress.phase_done == progress.phase_total
            && progress.phase_unit == "边"
    }));
    assert!(progresses.iter().any(|progress| {
        progress.sub_message == "LT 主循环"
            && progress.phase_total > 0
            && progress.phase_done == progress.phase_total
            && progress.phase_unit == "节点"
    }));
    let _ = fs::remove_file(path);
}

/// 验证完成结果会写入 sidecar，并且第二次打开可以直接从缓存恢复。
#[test]
fn sidecar缓存命中会恢复相同dominator结果() {
    let path = write_test_file("sidecar-cache", "hprof", &minimal_hprof_fixture());
    cleanup_test_file(&path);
    fs::write(&path, minimal_hprof_fixture()).expect("测试 HPROF 文件应能重新写入");

    let first =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("首次 HPROF 解析应成功");
    let sidecar_dir = test_sidecar_dir(&path);
    assert!(sidecar_dir.join("manifest.json").is_file());
    assert!(sidecar_dir.join("objects.bin").is_file());
    assert!(sidecar_dir.join("dominator.bin").is_file());
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(sidecar_dir.join("manifest.json")).unwrap())
            .expect("sidecar manifest 应是合法 JSON");
    assert_eq!(
        manifest["schema_version"], HPROF_SIDECAR_SCHEMA_VERSION,
        "sidecar v3 应让旧缓存按 schema 自动失效"
    );

    let mut progresses = Vec::new();
    let second = analyze_hprof_dominator_tree(
        path.clone(),
        |progress| progresses.push(progress),
        Arc::new(AtomicBool::new(false)),
    )
    .expect("第二次 HPROF 解析应从 sidecar 恢复");

    assert_eq!(first.total_objects, second.total_objects);
    assert_eq!(first.reachable_shallow_size, second.reachable_shallow_size);
    assert_eq!(first.top_object_ids, second.top_object_ids);
    assert_eq!(second.cache_status.as_deref(), Some("sidecar index 命中"));
    assert!(
        progresses
            .iter()
            .any(|progress| progress.stage == HprofAnalysisStage::LoadingCache)
    );
    assert!(
        progresses.iter().any(|progress| {
            progress.stage == HprofAnalysisStage::LoadingCache
                && progress.sub_message.contains("对象摘要懒加载")
                && progress.phase_unit == "文件"
                && progress.phase_total == 1
        }),
        "缓存命中路径不应全量读取对象摘要，应只建立可见行懒加载读取器"
    );
    assert!(
        progresses.iter().any(|progress| {
            progress.stage == HprofAnalysisStage::LoadingCache
                && progress.object_count == second.total_objects
                && progress.class_count == second.total_classes
                && progress.gc_root_count == second.gc_root_count
                && progress.edge_count == second.edge_count
        }),
        "缓存命中路径应在读取期间恢复摘要计数，避免 UI 长时间显示 0"
    );
    assert!(
        progresses.iter().all(|progress| {
            progress.stage != HprofAnalysisStage::LoadingCache
                || !progress.sub_message.starts_with("写入 ")
        }),
        "缓存命中路径只能展示读取进度，不能复用写入 sidecar 的文案"
    );
    cleanup_test_file(&path);
}

/// 验证 sidecar 缺少主体文件时会自动忽略旧缓存并重新解析。
#[test]
fn sidecar缺少文件会重新解析() {
    let path = write_test_file("sidecar-missing-file", "hprof", &minimal_hprof_fixture());
    cleanup_test_file(&path);
    fs::write(&path, minimal_hprof_fixture()).expect("测试 HPROF 文件应能重新写入");

    analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
        .expect("首次 HPROF 解析应成功");
    fs::remove_file(test_sidecar_dir(&path).join("objects.bin"))
        .expect("测试应能删除 sidecar 主体文件");
    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("缺失 sidecar 文件时应重新解析成功");

    assert_ne!(result.cache_status.as_deref(), Some("sidecar index 命中"));
    cleanup_test_file(&path);
}

/// 验证损坏 sidecar 的长度字段不会触发异常大分配，而是忽略缓存并重新解析。
#[test]
fn sidecar损坏长度不会导致异常大分配() {
    let path = write_test_file("sidecar-corrupt-count", "hprof", &minimal_hprof_fixture());
    cleanup_test_file(&path);
    fs::write(&path, minimal_hprof_fixture()).expect("测试 HPROF 文件应能重新写入");

    analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
        .expect("首次 HPROF 解析应成功");
    let objects_path = test_sidecar_dir(&path).join("objects.bin");
    let mut bytes = fs::read(&objects_path).expect("测试应能读取 objects sidecar");
    let count_offset = 8 + b"LCHP-objects-v3".len();
    bytes[count_offset..count_offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    fs::write(&objects_path, bytes).expect("测试应能写入损坏 sidecar");

    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("损坏 sidecar 应被忽略并重新解析成功");

    assert_ne!(result.cache_status.as_deref(), Some("sidecar index 命中"));
    cleanup_test_file(&path);
}

/// 默认忽略的 100MB 合成性能验证入口。
///
/// 业务意图：
/// - 该测试会写入较大临时文件并执行完整 dominator 计算，只用于本地手动比较优化前后阶段耗时，不应进入常规测试耗时。
#[test]
#[ignore]
fn 合成100mb_hprof性能采样() {
    let target_bytes = 100 * 1024 * 1024usize;
    let instance_record_bytes = 1 + 4 + 4 + 4 + 4 + 4;
    let instance_count = (target_bytes / instance_record_bytes).min(u32::MAX as usize) as u32;
    let bytes = synthetic_chain_hprof_fixture(instance_count);
    let path = write_test_file("synthetic-100mb", "hprof", &bytes);
    let started_at = std::time::Instant::now();
    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("100MB 合成 HPROF 应解析成功");
    eprintln!(
        "objects={} edges={} elapsed_ms={}",
        result.total_objects,
        result.edge_count,
        started_at.elapsed().as_millis()
    );
    let _ = fs::remove_file(path);
}

/// 构造 dominator 测试对象图。
fn make_graph(objects: &[(u64, u64, u64, &[u64])], roots: &[u64]) -> HprofObjectGraph {
    let header = HprofHeader {
        label: "JAVA PROFILE 1.0.2".to_string(),
        identifier_size: 8,
        timestamp_millis: 0,
    };
    let size_model = HprofSizeModel::mat_compatible(&header, 1024 * 1024);
    let mut graph = HprofObjectGraph::new(header, size_model);
    graph.classes.insert(
        100,
        HprofClassInfo {
            instance_size: 1,
            name: Some("com.example.Node".to_string()),
            instance_fields: Vec::new(),
        },
    );
    for (id, class_id, shallow_size, references) in objects {
        graph
            .insert_object(
                HprofHeapObject {
                    id: *id,
                    class_id: *class_id,
                    shallow_size: *shallow_size,
                    kind: HprofObjectKind::Instance,
                },
                references.to_vec(),
            )
            .expect("测试对象图不应包含重复对象 ID");
    }
    for root in roots {
        graph.gc_roots.push(HprofGcRoot {
            object_id: *root,
            kind: HprofGcRootKind::Unknown,
        });
    }
    graph
}

/// 测试中忽略计算阶段进度回调。
fn ignore_compute_progress(
    _stage: HprofAnalysisStage,
    _message: &str,
    _sub_message: &str,
    _phase_done: u64,
    _phase_total: u64,
    _phase_unit: &'static str,
) {
}

/// 验证线性链 retained size 会沿 dominator tree 向上累计。
#[test]
fn 线性链会累计retained_size() {
    let graph = make_graph(
        &[(1, 100, 10, &[2]), (2, 100, 20, &[3]), (3, 100, 30, &[])],
        &[1],
    );
    let result = build_hprof_dominator_result(
        Path::new("linear.hprof"),
        graph,
        &AtomicBool::new(false),
        ignore_compute_progress,
    )
    .expect("线性 dominator 图应计算成功");

    assert_eq!(result.node_for_object_id(1).unwrap().retained_size, 60);
    assert_eq!(result.node_for_object_id(2).unwrap().retained_size, 50);
    assert_eq!(result.node_for_object_id(3).unwrap().retained_size, 30);
    assert_eq!(result.top_object_ids.first().copied(), Some(1));
}

/// 验证继承字段布局会按 HPROF INSTANCE_DUMP 的“子类字段在前、父类字段在后”顺序合并。
#[test]
fn 继承字段布局会合并父类字段() {
    let mut raw_classes = FxHashMap::default();
    raw_classes.insert(
        100,
        RawClassInfo {
            super_class_id: 0,
            class_loader_id: 0,
            static_fields: Vec::new(),
            instance_fields: vec![RawFieldDescriptor {
                name_id: 1,
                field_type: FIELD_TYPE_OBJECT,
            }],
        },
    );
    raw_classes.insert(
        200,
        RawClassInfo {
            super_class_id: 100,
            class_loader_id: 0,
            static_fields: Vec::new(),
            instance_fields: vec![RawFieldDescriptor {
                name_id: 2,
                field_type: FIELD_TYPE_INT,
            }],
        },
    );

    let layout = resolve_layout(&raw_classes, &mut FxHashMap::default(), 200)
        .expect("子类字段布局应能解析父类字段");

    assert_eq!(layout.len(), 2);
    assert_eq!(layout[0].field_type, FIELD_TYPE_INT);
    assert_eq!(layout[1].field_type, FIELD_TYPE_OBJECT);
}

/// 验证子类 primitive 字段位于父类 object 字段之前时，不会把 primitive 字节误读成对象 ID。
#[test]
fn 继承字段偏移按子类优先解析对象引用() {
    let path = write_test_file(
        "inherited-field-order",
        "hprof",
        &inherited_field_order_hprof_fixture(),
    );
    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("继承字段顺序 fixture 应解析成功");

    assert!(result.node_for_object_id(20).is_some());
    assert_eq!(result.node_for_object_id(10).unwrap().retained_size, 40);
    let _ = fs::remove_file(path);
}

/// 返回线程详情属性值，便于测试锁定 MAT 风格属性表。
fn thread_property_value<'a>(details: &'a HprofThreadDetails, name: &str) -> Option<&'a str> {
    details
        .properties
        .iter()
        .find(|property| property.name == name)
        .map(|property| property.value.as_str())
}

/// 验证 START_THREAD、STACK_TRACE 和 STACK_FRAME 可以生成线程详情。
#[test]
fn 线程详情会解析属性和堆栈() {
    let path = write_test_file(
        "thread-details",
        "hprof",
        &thread_details_hprof_fixture(true),
    );
    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("线程详情 fixture 应解析成功");

    assert!(result.is_thread_object(10));
    let details = result
        .thread_details_for_object_id(10)
        .expect("线程对象应有详情");
    assert_eq!(details.thread_name, "worker-1");
    assert_eq!(thread_property_value(details, "Name"), Some("worker-1"));
    assert_eq!(thread_property_value(details, "Is Daemon"), Some("true"));
    assert_eq!(thread_property_value(details, "Priority"), Some("5"));
    assert_eq!(thread_property_value(details, "State value"), Some("0x291"));
    assert!(
        thread_property_value(details, "State")
            .unwrap_or_default()
            .contains("parked")
    );
    assert!(
        thread_property_value(details, "Context Class Loader")
            .unwrap_or_default()
            .contains("com.example.Loader @ 0x14")
    );
    assert_eq!(
        result.node_for_object_id(10).unwrap().name,
        "java.lang.Thread worker-1"
    );
    assert_eq!(details.stack_frames.len(), 1);
    assert_eq!(
        details.stack_frames[0].display,
        "at java.util.concurrent.locks.LockSupport.park(Ljava/lang/Object;)V (LockSupport.java:175)"
    );
    let _ = fs::remove_file(path);
}

/// 验证缺少 START_THREAD 时仍可通过 ROOT_THREAD_OBJECT 找到线程堆栈。
#[test]
fn root_thread_object可作为线程堆栈fallback() {
    let path = write_test_file(
        "thread-root-fallback",
        "hprof",
        &thread_details_hprof_fixture(false),
    );
    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("ROOT_THREAD_OBJECT fallback fixture 应解析成功");

    let details = result
        .thread_details_for_object_id(10)
        .expect("线程对象应有详情");
    assert_eq!(details.thread_name, "<unknown>");
    assert_eq!(details.stack_frames.len(), 1);
    assert!(details.stack_message.is_none());
    let _ = fs::remove_file(path);
}

/// 验证 `java.lang.Thread` 子类同样会显示线程详情。
#[test]
fn thread子类也会生成线程详情() {
    let path = write_test_file("thread-subclass", "hprof", &thread_subclass_hprof_fixture());
    let result =
        analyze_hprof_dominator_tree(path.clone(), |_| {}, Arc::new(AtomicBool::new(false)))
            .expect("Thread 子类 fixture 应解析成功");

    assert!(result.is_thread_object(11));
    let details = result
        .thread_details_for_object_id(11)
        .expect("Thread 子类应有详情");
    assert_eq!(details.class_name, "com.example.WorkerThread");
    assert_eq!(details.thread_name, "worker-sub");
    assert_eq!(thread_property_value(details, "Priority"), Some("5"));
    assert_eq!(
        details.stack_message.as_deref(),
        Some("该 HPROF 未包含此线程的堆栈信息")
    );
    let _ = fs::remove_file(path);
}

/// 验证菱形引用中共享子节点不会被单侧 retained size 误吞。
#[test]
fn 菱形引用不会把共享对象算进单侧retained() {
    let graph = make_graph(
        &[
            (1, 100, 10, &[2, 3]),
            (2, 100, 20, &[4]),
            (3, 100, 30, &[4]),
            (4, 100, 40, &[]),
        ],
        &[1],
    );
    let result = build_hprof_dominator_result(
        Path::new("diamond.hprof"),
        graph,
        &AtomicBool::new(false),
        ignore_compute_progress,
    )
    .expect("菱形引用 dominator 图应计算成功");

    assert_eq!(result.node_for_object_id(1).unwrap().retained_size, 100);
    assert_eq!(result.node_for_object_id(2).unwrap().retained_size, 20);
    assert_eq!(result.node_for_object_id(3).unwrap().retained_size, 30);
    let mut expanded = HashSet::new();
    expanded.insert(1);
    assert!(
        result
            .visible_rows(&expanded)
            .iter()
            .any(|row| { row.depth == 1 && row.node.object_id == 4 })
    );
}

/// 验证多个 GC root 和不可达对象会被分别统计。
#[test]
fn 多root和不可达对象会被统计() {
    let graph = make_graph(
        &[
            (1, 100, 10, &[2]),
            (2, 100, 20, &[]),
            (3, 100, 30, &[]),
            (4, 100, 40, &[]),
        ],
        &[1, 3],
    );
    let result = build_hprof_dominator_result(
        Path::new("roots.hprof"),
        graph,
        &AtomicBool::new(false),
        ignore_compute_progress,
    )
    .expect("多 root dominator 图应计算成功");

    assert_eq!(result.reachable_object_count, 3);
    assert_eq!(result.unreachable_object_count, 1);
    assert_eq!(result.unreachable_shallow_size, 40);
}

/// 验证展开集合可以把 Top 节点的直接支配子节点加入可见行。
#[test]
fn 展开集合会生成可见子行() {
    let graph = make_graph(
        &[(1, 100, 10, &[2]), (2, 100, 20, &[3]), (3, 100, 30, &[])],
        &[1],
    );
    let result = build_hprof_dominator_result(
        Path::new("visible.hprof"),
        graph,
        &AtomicBool::new(false),
        ignore_compute_progress,
    )
    .expect("可见行测试 dominator 图应计算成功");

    let collapsed_rows = result.visible_rows(&HashSet::new());
    let mut expanded = HashSet::new();
    expanded.insert(1);
    let expanded_rows = result.visible_rows(&expanded);

    assert!(!collapsed_rows.is_empty());
    assert!(expanded_rows.len() > collapsed_rows.len());
    assert!(expanded_rows.iter().any(|row| row.depth == 1));
}

/// 验证顶层 dominator 行只包含虚拟 Root 的直接支配子节点，不把子孙节点重复提升为顶层 Top retained。
#[test]
fn 顶层行只显示虚拟root直接子节点() {
    let graph = make_graph(
        &[(1, 100, 10, &[2]), (2, 100, 20, &[3]), (3, 100, 30, &[])],
        &[1],
    );
    let result = build_hprof_dominator_result(
        Path::new("top-root-children.hprof"),
        graph,
        &AtomicBool::new(false),
        ignore_compute_progress,
    )
    .expect("顶层 dominator 图应计算成功");

    assert_eq!(result.top_object_ids, vec![1]);
    let collapsed_rows = result.visible_rows(&HashSet::new());
    assert_eq!(collapsed_rows.len(), 1);
    assert_eq!(collapsed_rows[0].node.object_id, 1);
}
