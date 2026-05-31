// 插件声明式结果窗口和插件命令调度。
//
// 业务意图：
// - 插件 v1 通过外部进程 stdin/stdout 交换 JSON，宿主只接受声明式页面、消息和错误，不让第三方代码直接操作 GPUI 状态。
// - 本文件集中维护插件命令后台执行、响应分发和声明式页面渲染，避免日志树、笔记树和设置页各自拼接插件窗口。
//
// 边界条件：
// - 插件进程可能返回非法 JSON、超时或退出失败；错误必须转成中文提示写入主视图插件状态，不让 UI 线程崩溃。
// - 大数据插件会先打开进度窗口，再通过 stdout JSON Lines 刷新处理进度，避免用户误以为没有响应。
// - 声明式表格可能很宽或为空，窗口需要允许横向和纵向滚动，并在空表格时展示可理解的空状态。

use super::*;
use crate::log_document::stream_log_source_bytes_to_writer;
use chrono::{Datelike, Timelike};
use gpui::Size;
use gpui::prelude::FluentBuilder;
use std::{
    cmp::Ordering,
    collections::BTreeMap,
    io::{self, Write},
    ops::Range,
    sync::{
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
        mpsc,
    },
    time::Duration,
};

/// 插件表格固定行高。
///
/// 业务意图：
/// - 插件结果可能超过一万行，必须使用 `uniform_list` 虚拟渲染；固定行高是虚拟列表稳定计算可见区间的前提。
const PLUGIN_TABLE_ROW_HEIGHT: f32 = 32.0;

/// 插件表格表头固定高度。
const PLUGIN_TABLE_HEADER_HEIGHT: f32 = 34.0;

/// 插件表格自绘滚动条边距。
///
/// 业务意图：
/// - 插件表格可能同时出现横向和纵向滚动；滚动条需要贴近内容但不能遮住边框。
/// - 使用与日志正文一致的窄滑块，避免在高密度插件表格中占用过多空间。
const PLUGIN_TABLE_SCROLLBAR_PADDING: f32 = 4.0;

/// 插件表格自绘滚动条宽度。
const PLUGIN_TABLE_SCROLLBAR_WIDTH: f32 = 6.0;

/// 插件表格自绘滚动条最小滑块长度。
///
/// 边界条件：
/// - 超过一万行的插件表格会让理论滑块极小；保留最小长度才能让用户可靠拖动。
const PLUGIN_TABLE_SCROLLBAR_MIN_THUMB_LENGTH: f32 = 36.0;

/// 插件表格兜底列宽。
///
/// 业务意图：
/// - 插件协议允许第三方返回任意表头；宿主不能理解业务列含义，只能用统一兜底宽度保证布局稳定。
const PLUGIN_TABLE_FALLBACK_COLUMN_WIDTH: f32 = 160.0;

/// 插件表格搜索栏高度。
///
/// 业务意图：
/// - 所有插件声明式表格都共用宿主渲染层，搜索栏放在表格上方即可覆盖内置和第三方插件。
/// - 高度保持紧凑，避免大数据插件页因过滤控件占用过多垂直空间。
const PLUGIN_TABLE_FILTER_HEIGHT: f32 = 34.0;

/// 插件表格内容估算时单字符平均宽度。
///
/// 业务意图：
/// - GPUI 排版必须在窗口绘制阶段才能拿到真实字体宽度；表格列宽需要在状态重建时确定，不能为每个单元格同步排版。
/// - 使用偏保守的平均宽度估算列内容，可以让任意长文本触发横向滚动，同时避免逐行测量造成大表格卡顿。
const PLUGIN_TABLE_APPROX_CHAR_WIDTH: f32 = 7.6;

/// 插件表格内容列最大估算宽度。
///
/// 边界条件：
/// - 插件单元格可能返回非常长的原文片段，列宽不做上限会生成几万像素宽的滚动内容，影响命中测试和滚动体验。
/// - 上限只限制单列首屏宽度；用户仍可复制可见文本范围，超长内容后续可通过插件详情页继续拆分展示。
const PLUGIN_TABLE_MAX_CONTENT_COLUMN_WIDTH: f32 = 1800.0;

/// 插件表格列宽估算最多采样的行数。
///
/// 业务意图：
/// - 插件表格虽然使用虚拟列表渲染可见行，但页面初始化仍需要估算列宽；对几十万行逐行扫描会直接卡住窗口创建。
/// - 采样前若干行可以覆盖大多数首屏字段长度，同时把初始化开销限制在固定上限内。
///
/// 边界条件：
/// - 极端长文本如果只出现在采样窗口之后，列宽可能低估；单元格仍会被裁剪且可通过横向滚动/详情页查看，不影响数据正确性。
const PLUGIN_TABLE_COLUMN_WIDTH_SAMPLE_ROWS: usize = 2048;

/// 插件窗口默认尺寸。
///
/// 业务意图：
/// - 插件结果窗口通常承载多列表格或步骤输出，需要比普通确认弹窗更宽，避免主要列被过早截断。
const PLUGIN_PAGE_WINDOW_SIZE: Size<Pixels> = size(px(1180.0), px(660.0));

/// 插件窗口最小尺寸。
///
/// 边界条件：
/// - 最小宽度需要覆盖常见详情表格的固定列宽，防止操作列被挤压到不可点击。
const PLUGIN_PAGE_WINDOW_MIN_SIZE: Size<Pixels> = size(px(900.0), px(460.0));

/// 插件新窗口错位步长。
///
/// 业务意图：
/// - 插件行内动作会频繁打开明细窗口；如果所有窗口完全居中重叠，用户很难判断新窗口是否已经打开。
const PLUGIN_WINDOW_OFFSET_STEP: f32 = 28.0;

/// 插件窗口错位循环数量。
///
/// 边界条件：
/// - 持续打开很多窗口时不能无限向右下漂移，循环错位可以兼顾可见性和屏幕边界风险。
const PLUGIN_WINDOW_OFFSET_CYCLE: usize = 8;

/// 插件步骤输出中日志截图片段的起始标记。
///
/// 业务意图：
/// - 插件可以把异常行上下文作为文本流返回；宿主消费该内部标记后渲染成带行号的日志块。
/// - 标记只属于宿主和插件之间的声明式协议，不应直接展示给用户。
const PLUGIN_LOG_SNIPPET_BEGIN: &str = "@@LC_LOG_SNIPPET_BEGIN";

/// 插件步骤输出中日志截图片段的结束标记。
const PLUGIN_LOG_SNIPPET_END: &str = "@@LC_LOG_SNIPPET_END";

/// 插件步骤输出中单行日志片段的标记前缀。
const PLUGIN_LOG_SNIPPET_LINE_PREFIX: &str = "@@LC_LOG_LINE\t";

/// 当前进程内插件窗口打开序号。
///
/// 业务意图：
/// - 独立窗口之间不共享状态实体，因此用进程级原子计数为每个新插件窗口计算稳定错位。
/// - 该值只影响当前进程的窗口视觉位置，不写入配置，不改变插件协议。
static PLUGIN_WINDOW_OPEN_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

/// 将任意字节流按行转换成插件内容事件的 writer。
///
/// 业务意图：
/// - 压缩包 reader 和普通文件 reader 只会输出连续字节；插件协议要求 stdin 后续内容是 JSON Lines。
/// - 该适配器把两者隔离开：上游只负责流式写字节，下游只看到一行一个 `log_content_line` 事件。
///
/// 边界条件：
/// - 日志可能不是 UTF-8；这里使用有损转换，保持与既有插件读取路径一致，避免因单行乱码中断整个插件解析。
/// - 超长单行仍会在 `line_buffer` 中暂存，这是按行协议不可避免的边界；普通多行大文件不会整体进入内存。
struct PluginContentLineWriter<'a> {
    /// 插件进程 stdin writer。
    writer: &'a mut dyn Write,
    /// 宿主侧进度通知通道；为空时表示测试或同步调用路径不需要刷新 UI。
    event_sender: Option<&'a mpsc::Sender<PluginCommandRuntimeEvent>>,
    /// 当前尚未遇到换行符的一行原始字节。
    line_buffer: Vec<u8>,
    /// 已发送给插件的日志行数。
    done: u64,
}

/// 插件步骤输出正文的结构化块。
///
/// 业务意图：
/// - 插件步骤仍通过单个字符串做流式追加，但其中可能混入需要截图式展示的上下文日志片段。
/// - 渲染前先解析为普通文本块和日志片段块，避免把内部协议标记暴露给用户。
#[derive(Clone, Debug, PartialEq, Eq)]
enum PluginOutputStepTextBlock {
    /// 普通诊断文本，按原换行顺序展示。
    Text(Vec<String>),
    /// 带行号和高亮的原始日志上下文片段。
    LogSnippet(Vec<PluginOutputLogSnippetLine>),
}

/// 插件步骤输出中的单行原始日志上下文。
///
/// 边界条件：
/// - `text` 保留插件传回的原始行内容，可能包含制表符和较长数值列，渲染时使用横向滚动而不是截断。
/// - `highlight_terms` 是插件解析出的关键数字 token，宿主只按字面匹配高亮，不重新解释日志业务含义。
#[derive(Clone, Debug, PartialEq, Eq)]
struct PluginOutputLogSnippetLine {
    /// 原始文件行号，从 1 开始。
    line_number: String,
    /// 原始日志行。
    text: String,
    /// 是否为当前异常行。
    is_issue: bool,
    /// 当前行需要高亮的文本片段。
    highlight_terms: Vec<String>,
}

/// 插件表格文本选择状态。
///
/// 业务意图：
/// - GPUI 当前没有内建的可虚拟化表格文本选择控件；插件大表格仍必须使用 `uniform_list`，因此宿主只保存当前单元格的文本范围。
/// - 用户拖动光标选择的是文本片段，复制快捷键只复制该片段，避免旧实现“一点单元格就复制整格”造成误操作。
///
/// 边界条件：
/// - 范围必须始终夹在 UTF-8 字符边界上，避免中文、emoji 或异常插件文本被切成非法字符串。
/// - 选区只在当前插件页面生命周期内有效；页面替换、排序或重新加载会清空，避免复制过期数据。
#[derive(Clone, Debug, PartialEq, Eq)]
struct PluginTableTextSelection {
    /// 原始表格行下标，而不是排序后的可见下标。
    row_index: usize,
    /// 表格列下标。
    column_index: usize,
    /// 当前单元格文本快照。
    text: String,
    /// 当前被选中的文本字节范围。
    range: Range<usize>,
    /// 鼠标拖动开始时的字节位置。
    drag_anchor: usize,
    /// 是否仍处于鼠标拖选生命周期中。
    dragging: bool,
}

/// 插件表格滚动条拖动状态。
///
/// 业务意图：
/// - 横向和纵向滑块都覆盖在表格内容上方；鼠标按下后需要记录方向和滑块内偏移，后续移动才能稳定换算为滚动位置。
/// - 该状态只保存在插件窗口内，不影响主窗口日志正文或搜索结果的滚动条状态。
#[derive(Clone, Copy)]
struct PluginTableScrollbarDrag {
    /// 当前拖动的滚动条方向。
    axis: LogScrollbarAxis,
    /// 鼠标按下点相对滑块起点的偏移。
    cursor_offset: Pixels,
}

/// 插件表格单元格文本元素的预绘制结果。
///
/// 业务意图：
/// - 单元格需要支持按字符范围选中，因此必须在 `prepaint` 阶段拿到真实字形布局，才能准确绘制选区矩形。
/// - 该结构只保存当前帧绘制所需数据，不持久化任何插件内容。
struct PluginTableSelectableTextPrepaint {
    /// 当前单元格文本的字形布局。
    line: ShapedLine,
    /// 当前选区对应的绘制矩形；没有选区时为空。
    selection: Option<PaintQuad>,
}

/// 插件表格过滤输入框的预绘制结果。
///
/// 业务意图：
/// - 表格过滤属于插件窗口内部的临时 UI 状态，不走主窗口搜索对话框，也不写入插件协议。
/// - 输入框使用同一套 GPUI 平台输入协议，保证中文 IME、复制、粘贴和鼠标定位行为与其它输入框一致。
struct PluginTableFilterInputPrepaint {
    /// 当前输入文本的字形布局。
    line: ShapedLine,
    /// 当前选区对应的高亮矩形。
    selection: Option<PaintQuad>,
    /// 当前光标矩形。
    cursor: Option<PaintQuad>,
    /// 当前帧文本水平滚动偏移。
    horizontal_scroll_px: f32,
}

/// 插件表格过滤输入框最近一次布局。
///
/// 业务意图：
/// - 平台 IME 候选框和鼠标点击定位都需要把窗口坐标转换为文本字节位置。
/// - 该布局只缓存当前输入框最近绘制结果，窗口重绘后会覆盖，不参与插件数据持久化。
#[derive(Clone)]
struct PluginTableFilterInputLayout {
    /// 当前输入文本的字形布局。
    line: ShapedLine,
    /// 输入框文本元素边界。
    bounds: Bounds<Pixels>,
    /// 文本水平滚动偏移。
    horizontal_scroll_px: f32,
}

/// 插件表格过滤栏中的单行输入框种类。
///
/// 业务意图：
/// - 普通关键字过滤和插件声明式命令过滤共享同一套自绘输入框、IME、选区和剪贴板处理。
/// - 命令过滤控件数量由插件声明，宿主只用索引路由输入状态，不理解字段业务含义。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PluginTableInputKind {
    /// 当前表格的本地关键字过滤。
    QuickFilter,
    /// 插件命令过滤器中的第 N 个控件。
    CommandFilter(usize),
}

/// 插件声明式命令过滤输入框状态。
///
/// 业务意图：
/// - 每个输入框的字段名、初始值和占位文案都来自插件协议；宿主只保存当前窗口内的编辑状态。
/// - 焦点、拖选和布局缓存按控件独立保存，避免多个输入框之间的 IME 候选框和鼠标选择互相串扰。
struct PluginTableCommandFilterInputState {
    /// 回写到插件命令 `TableAction.data` 的字段名。
    key: String,
    /// 当前输入状态。
    input: SingleLineTextInputState,
    /// 输入框焦点。
    focus: gpui::FocusHandle,
    /// 鼠标拖选锚点。
    selection_drag: Option<usize>,
    /// 最近一次文本布局。
    layout: Option<PluginTableFilterInputLayout>,
    /// 空输入时的占位文案。
    placeholder: String,
    /// 控件类型。
    ///
    /// 业务意图：
    /// - 插件协议只声明通用输入语义，宿主按类型选择文本框或日期时间选择辅助。
    /// - 字段的业务过滤规则仍由插件解析，本状态不根据 key 判断含义。
    control_kind: PluginTableCommandFilterControlKind,
    /// 可选通用图标名。
    icon: Option<String>,
    /// 输入框建议宽度。
    width: Pixels,
    /// 日期时间弹层是否打开。
    ///
    /// 边界条件：
    /// - 仅 `DateTime` 控件使用；普通文本控件保持 false，避免渲染无效浮层。
    date_time_picker_open: bool,
    /// 日期时间弹层当前展示年份。
    date_time_picker_view_year: i32,
    /// 日期时间弹层当前展示月份。
    date_time_picker_view_month: u32,
}

/// 插件表格过滤输入元素。
///
/// 业务意图：
/// - 所有插件表格都需要一个轻量本地过滤框，用户输入任意关键字后只过滤当前窗口内已有行，不重新执行插件。
/// - 自绘输入避免引入平台差异明显的原生控件，同时复用 `EntityInputHandler` 支持中文输入法。
struct PluginTableFilterInputElement {
    /// 插件窗口实体。
    view: Entity<PluginPageWindowView>,
    /// 输入框种类。
    input_kind: PluginTableInputKind,
    /// 输入框焦点。
    focus_handle: gpui::FocusHandle,
    /// 空输入时的占位文案。
    placeholder: String,
    /// 当前主题调色板。
    palette: AppThemePalette,
}

impl IntoElement for PluginTableFilterInputElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// 插件表格可选择文本元素。
///
/// 业务意图：
/// - 普通 `div().child(text)` 只能整体命中，无法提供“拖动选择文本片段”的交互。
/// - 自绘元素让插件表格在保持虚拟列表性能的同时支持可视文本范围选择和复制。
struct PluginTableSelectableTextElement {
    /// 插件窗口实体，用于读取当前选区并回写本帧单元格边界。
    view: Entity<PluginPageWindowView>,
    /// 原始表格行下标。
    row_index: usize,
    /// 表格列下标。
    column_index: usize,
    /// 单元格文本快照。
    text: String,
    /// 当前主题调色板。
    palette: AppThemePalette,
}

impl IntoElement for PluginTableSelectableTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PluginTableSelectableTextElement {
    type RequestLayoutState = ();
    type PrepaintState = PluginTableSelectableTextPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        context: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        // 单元格文本由父级行固定高度承载；这里使用当前文本行高，避免文字垂直位置随窗口尺寸抖动。
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], context), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        context: &mut App,
    ) -> Self::PrepaintState {
        let line = Self::shape_text(&self.text, self.palette, window);
        let selection_range = self
            .view
            .read(context)
            .plugin_table_text_selection_for_cell(self.row_index, self.column_index);
        let selection = selection_range.and_then(|range| {
            let range = MainView::clamp_search_text_range(&self.text, range);
            if range.start >= range.end {
                return None;
            }
            let mut selection_color = rgb(self.palette.accent);
            selection_color.a = 0.28;
            let left = f32::from(bounds.left() + line.x_for_index(range.start))
                .clamp(f32::from(bounds.left()), f32::from(bounds.right()));
            let right = f32::from(bounds.left() + line.x_for_index(range.end))
                .clamp(f32::from(bounds.left()), f32::from(bounds.right()));
            Some(fill(
                Bounds::from_corners(
                    point(px(left.min(right)), bounds.top()),
                    point(px(left.max(right)), bounds.bottom()),
                ),
                selection_color,
            ))
        });

        PluginTableSelectableTextPrepaint { line, selection }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        context: &mut App,
    ) {
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        prepaint
            .line
            .paint(
                bounds.origin,
                bounds.bottom() - bounds.top(),
                window,
                context,
            )
            .ok();

        // 鼠标命中需要真实布局边界；在绘制阶段回写，避免用估算列宽处理中文和不同平台字体差异。
        self.view.update(context, |view, _context| {
            view.store_plugin_table_cell_bounds(self.row_index, self.column_index, bounds);
        });
    }
}

impl PluginTableSelectableTextElement {
    /// 使用当前窗口文本样式排版单元格文本。
    ///
    /// 边界条件：
    /// - 插件文本按普通字符串绘制，不解释 Markdown、HTML 或 ANSI 序列，避免第三方内容影响宿主 UI。
    fn shape_text(text: &str, palette: AppThemePalette, window: &mut Window) -> ShapedLine {
        let style = window.text_style();
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color: rgb(palette.text).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        window.text_system().shape_line(
            SharedString::from(text.to_string()),
            font_size,
            &[run],
            None,
        )
    }
}

impl Element for PluginTableFilterInputElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<PluginTableFilterInputPrepaint>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        context: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        // 输入框文本元素需要明确行高，避免过滤栏在窗口缩放后出现光标高度为 0 的平台差异。
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], context), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        context: &mut App,
    ) -> Self::PrepaintState {
        let snapshot = self
            .view
            .read(context)
            .table_input_snapshot(self.input_kind);
        let style = window.text_style();
        let display_text = if snapshot.text.is_empty() {
            SharedString::from(self.placeholder.clone())
        } else {
            SharedString::from(snapshot.text.clone())
        };
        let text_color = if snapshot.text.is_empty() {
            rgb(self.palette.muted_text).into()
        } else {
            style.color
        };
        let base_run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if !snapshot.text.is_empty()
            && let Some(marked_range) = snapshot.marked_range.clone()
        {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..base_run.clone()
                },
                TextRun {
                    len: marked_range.end.saturating_sub(marked_range.start),
                    underline: Some(UnderlineStyle {
                        color: Some(base_run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..base_run.clone()
                },
                TextRun {
                    len: display_text.len().saturating_sub(marked_range.end),
                    ..base_run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect::<Vec<_>>()
        } else {
            vec![base_run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);
        let focused = self.focus_handle.is_focused(window);
        let selection_range =
            MainView::clamp_search_text_range(&snapshot.text, snapshot.selection_range);
        let has_selection =
            focused && !snapshot.text.is_empty() && selection_range.start < selection_range.end;
        let cursor_index = selection_range.end;
        let content_width = if snapshot.text.is_empty() {
            px(0.0)
        } else {
            line.x_for_index(snapshot.text.len())
        };
        let horizontal_scroll_px = MainView::single_line_horizontal_scroll_offset(
            snapshot.horizontal_scroll_px,
            line.x_for_index(cursor_index),
            content_width,
            bounds.size.width,
            focused,
        );
        let text_origin = point(bounds.left() - px(horizontal_scroll_px), bounds.top());
        let selection = has_selection.then(|| {
            let mut selection_color = rgb(self.palette.accent);
            selection_color.a = 0.32;
            let left = f32::from(text_origin.x + line.x_for_index(selection_range.start))
                .clamp(f32::from(bounds.left()), f32::from(bounds.right()));
            let right = f32::from(text_origin.x + line.x_for_index(selection_range.end))
                .clamp(f32::from(bounds.left()), f32::from(bounds.right()));
            fill(
                Bounds::from_corners(
                    point(px(left.min(right)), bounds.top()),
                    point(px(left.max(right)), bounds.bottom()),
                ),
                selection_color,
            )
        });
        let cursor = (focused && !has_selection).then(|| {
            let cursor_right_limit = (f32::from(bounds.right()) - SINGLE_LINE_INPUT_CARET_WIDTH)
                .max(f32::from(bounds.left()));
            let cursor_x = f32::from(text_origin.x + line.x_for_index(cursor_index))
                .clamp(f32::from(bounds.left()), cursor_right_limit);
            fill(
                Bounds::new(
                    point(px(cursor_x), bounds.top()),
                    size(
                        px(SINGLE_LINE_INPUT_CARET_WIDTH),
                        bounds.bottom() - bounds.top(),
                    ),
                ),
                rgb(self.palette.accent),
            )
        });

        Some(PluginTableFilterInputPrepaint {
            line,
            selection,
            cursor,
            horizontal_scroll_px,
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        context: &mut App,
    ) {
        window.handle_input(
            &self.focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            context,
        );
        let Some(prepaint) = prepaint.take() else {
            return;
        };
        if let Some(selection) = prepaint.selection {
            window.paint_quad(selection);
        }
        prepaint
            .line
            .paint(
                point(
                    bounds.left() - px(prepaint.horizontal_scroll_px),
                    bounds.top(),
                ),
                bounds.bottom() - bounds.top(),
                window,
                context,
            )
            .ok();
        if let Some(cursor) = prepaint.cursor {
            window.paint_quad(cursor);
        }
        self.view.update(context, |view, _context| {
            view.store_table_input_layout(
                self.input_kind,
                PluginTableFilterInputLayout {
                    line: prepaint.line,
                    bounds,
                    horizontal_scroll_px: prepaint.horizontal_scroll_px,
                },
            );
        });
        if self.focus_handle.is_focused(window) {
            window.request_animation_frame();
        }
    }
}

impl<'a> PluginContentLineWriter<'a> {
    /// 创建按行转换 writer。
    fn new(
        writer: &'a mut dyn Write,
        event_sender: Option<&'a mpsc::Sender<PluginCommandRuntimeEvent>>,
    ) -> Self {
        Self {
            writer,
            event_sender,
            line_buffer: Vec::new(),
            done: 0,
        }
    }

    /// 结束流式写入，并把没有换行结尾的最后一行发送给插件。
    fn finish(mut self) -> Result<u64, String> {
        if !self.line_buffer.is_empty() {
            self.emit_current_line()?;
        }
        self.writer
            .flush()
            .map_err(|error| format!("刷新插件日志正文流失败：{error}"))?;
        Ok(self.done)
    }

    /// 将当前行缓冲写成一个插件内容事件。
    fn emit_current_line(&mut self) -> Result<(), String> {
        let line = String::from_utf8_lossy(&self.line_buffer);
        let line = line.trim_end_matches(['\r', '\n']);
        write_plugin_log_content_line(self.writer, line)?;
        self.line_buffer.clear();
        self.done = self.done.saturating_add(1);
        PluginPageWindowView::report_content_stream_progress(self.done, self.event_sender);
        Ok(())
    }
}

impl Write for PluginContentLineWriter<'_> {
    /// 接收上游 reader 的一段字节，并在遇到换行符时立即发送协议事件。
    ///
    /// 边界条件：
    /// - `Write` 约定成功时返回已消费字节数；即使内部拆成多行事件，也必须返回输入切片长度。
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        for byte in buffer {
            self.line_buffer.push(*byte);
            if *byte == b'\n' {
                self.emit_current_line()
                    .map_err(|error| io::Error::new(io::ErrorKind::Other, error))?;
            }
        }
        Ok(buffer.len())
    }

    /// 刷新底层插件 stdin。
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// 插件表格排序状态。
///
/// 业务意图：
/// - 声明式表格默认按插件返回顺序展示，用户点击表头后只在宿主窗口内临时排序，不回写插件数据。
/// - 排序状态随当前插件页面生命周期存在；切换插件结果时重置，避免不同表格列语义混用。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PluginTableSortState {
    /// 当前排序列下标。
    column_index: usize,
    /// 是否升序排列。
    ascending: bool,
}

/// 插件声明式页面独立窗口。
///
/// 业务意图：
/// - 插件返回的 `PluginPage` 由宿主统一渲染，避免插件直接持有窗口句柄或自定义 GPUI 元素。
/// - 同一个窗口可被多个插件命令复用；再次打开时替换页面内容并激活窗口。
pub(in crate::app) struct PluginPageWindowView {
    /// 插件窗口根焦点。
    ///
    /// 业务意图：
    /// - 表格单元格点击后需要接收 `Cmd/Ctrl+C`，因此插件窗口自身必须有可聚焦根节点。
    /// - 焦点只服务插件窗口内的复制快捷键，不参与主窗口日志正文或笔记编辑器的焦点流转。
    focus_handle: gpui::FocusHandle,
    /// 当前窗口标题。
    pub(in crate::app) title: String,
    /// 当前声明式页面数据。
    pub(in crate::app) page: PluginPage,
    /// 当前窗口缓存主题色板。
    ///
    /// 业务意图：
    /// - 插件窗口可能在主窗口事件更新过程中被调度创建，渲染时不能再读取 `MainView`，否则 GPUI 会因实体重入直接 panic。
    /// - 色板在打开和替换页面时由主窗口传入；主题切换后重新触发插件或刷新窗口即可取得新色板。
    pub(in crate::app) palette: AppThemePalette,
    /// 当前页面来源插件。
    ///
    /// 业务意图：
    /// - 行内“延迟命令”按钮需要在点击后再次启动同一个插件进程，例如从汇总页打开明细页。
    /// - 该字段保存插件定义快照，只服务当前窗口生命周期；插件被禁用或卸载后，已有窗口按钮再次点击会按快照尝试执行并展示错误，不影响主程序稳定性。
    origin_plugin: Option<PluginDefinition>,
    /// 当前插件页面关联的日志来源快照。
    ///
    /// 业务意图：
    /// - 插件结果页中的延迟按钮只会回传 `source_key` 等 JSON 字段；宿主需要用这里保存的原始 `LogFileSource` 进行受控正文读取。
    /// - 该快照只来自用户触发插件时的选择，不跟随后续左侧树选择变化，避免按钮点击时搜索范围漂移。
    origin_log_sources: BTreeMap<String, LogFileSource>,
    /// 当前插件页面关联的可序列化日志文件快照。
    ///
    /// 业务意图：
    /// - 插件可以把重型详情页做成延迟命令，首个响应只携带行标识，不携带所有明细行。
    /// - 用户点击详情时，宿主用这里保存的原始候选文件补齐 `TableAction.files`，再启动插件生成明细页。
    ///
    /// 边界条件：
    /// - 快照只来自用户当次右键或工具栏授权范围；后续左侧树选择变化不会影响已打开插件窗口。
    /// - `host_source` 字段只在宿主进程内使用，最终发给插件前仍会按权限剥离 `read_path`。
    origin_log_files: Vec<PluginLogFile>,
    /// 当前插件窗口内后台命令代次。
    ///
    /// 业务意图：
    /// - 插件声明式命令过滤在当前窗口内直接替换页面，如果用户连续点击“应用”或“重置”，旧命令迟到时不能覆盖新条件结果。
    /// - 该状态只保护当前窗口内命令，不影响主窗口工具栏、右键菜单和独立行内命令窗口的既有代次机制。
    current_window_command_generations: PluginWindowCommandGenerationState,
    /// 插件表格当前排序状态。
    ///
    /// 边界条件：
    /// - `None` 表示按插件返回顺序显示；插件侧已经排序的结果不会被宿主自动改写。
    /// - 点击表头后才生成宿主侧排序，不要求插件重新执行，避免大数据结果重复解析。
    table_sort: Option<PluginTableSortState>,
    /// 插件表格过滤输入状态。
    ///
    /// 业务意图：
    /// - 过滤只作用于当前插件窗口内已有表格数据，不回传插件、不重新读取日志，也不写入任何配置。
    /// - 使用 `SingleLineTextInputState` 统一 UTF-8 选区、IME 组合文本和水平滚动处理，保证中文关键字输入稳定。
    table_filter_input: SingleLineTextInputState,
    /// 插件表格过滤输入框焦点。
    ///
    /// 边界条件：
    /// - 过滤框聚焦时键盘事件优先用于编辑过滤关键字，不能触发表格复制或行按钮快捷行为。
    table_filter_focus: gpui::FocusHandle,
    /// 插件表格过滤输入框鼠标拖选锚点。
    table_filter_selection_drag: Option<usize>,
    /// 插件表格过滤输入框最近一次文本布局。
    table_filter_layout: Option<PluginTableFilterInputLayout>,
    /// 插件声明式命令过滤输入状态。
    ///
    /// 业务意图：
    /// - 插件表格可以声明需要回调插件重新生成页面的过滤控件；宿主只保存控件编辑状态和通用输入法状态。
    /// - 字段名、占位文案、默认值、图标和宽度都来自插件，主程序不包含任何插件业务规则。
    command_filter_inputs: Vec<PluginTableCommandFilterInputState>,
    /// 插件表格当前可见行顺序。
    ///
    /// 业务意图：
    /// - 大结果集排序时只保存原始行下标，不复制整张表，降低窗口滚动和重排时的内存压力。
    /// - `uniform_list` 渲染时按该下标映射到原始行，仍只创建可见行元素。
    table_row_order: Vec<usize>,
    /// 当前表格的列宽缓存。
    ///
    /// 业务意图：
    /// - 列宽需要参考表格内容，但大表格滚动时不能每帧扫描所有行；页面替换时计算一次并缓存。
    /// - 过滤只改变可见行，不改变列宽，避免输入关键字时表格横向布局来回跳动。
    table_column_widths: Vec<f32>,
    /// 插件表格虚拟列表滚动句柄。
    ///
    /// 业务意图：
    /// - 大结果集不能把所有表格行一次性渲染出来；滚动句柄让 `uniform_list` 只创建可见行，同时保存当前滚动位置。
    /// - 每次替换为新的最终页面时重置句柄，避免旧插件结果的滚动位置影响新结果。
    pub(in crate::app) table_scroll_handle: UniformListScrollHandle,
    /// 插件表格横向滚动句柄。
    ///
    /// 业务意图：
    /// - 表格列宽超过可视区域时，横向滚动由外层 `ScrollHandle` 维护；自绘横向滚动条和原生滚轮共享同一份偏移。
    /// - 页面替换时重置，避免从旧宽表切换到新窄表时仍停在旧的横向位置。
    table_x_scroll_handle: ScrollHandle,
    /// 当前正在拖动的插件表格滚动条。
    ///
    /// 边界条件：
    /// - 只在鼠标左键拖动期间存在；释放鼠标、页面替换或排序都会清空。
    table_scrollbar_drag: Option<PluginTableScrollbarDrag>,
    /// 当前表格文本选区。
    ///
    /// 业务意图：
    /// - 用户需要选择单元格内一段文本复制，而不是复制整个单元格；这里保存当前文本范围。
    /// - 选区和排序后的可见下标无关，统一使用原始行下标，避免滚动虚拟列表时选区错位。
    table_text_selection: Option<PluginTableTextSelection>,
    /// 步骤式输出当前已经显示的字符数。
    ///
    /// 业务意图：
    /// - 步骤式正文由插件按文本块追加，宿主负责逐字展示，避免插件为了动画频繁写 stdout。
    /// - 这里按步骤 ID 保存已显字符数；最终页面覆盖时复用旧进度，防止已显示内容突然重置。
    output_step_visible_chars: BTreeMap<String, usize>,
    /// 当前帧已绘制单元格的文本边界。
    ///
    /// 业务意图：
    /// - 鼠标拖选需要把窗口坐标换算为单元格内字符下标；真实边界由自绘文本元素在 paint 阶段回写。
    /// - 只缓存可见单元格边界，页面替换和排序会清空，避免旧布局参与新页面命中。
    table_cell_bounds: BTreeMap<(usize, usize), Bounds<Pixels>>,
}

impl PluginPageWindowView {
    /// 创建插件结果窗口。
    pub(in crate::app) fn new(
        title: String,
        page: PluginPage,
        palette: AppThemePalette,
        origin_plugin: Option<PluginDefinition>,
        origin_log_sources: BTreeMap<String, LogFileSource>,
        origin_log_files: Vec<PluginLogFile>,
        _context: &mut Context<Self>,
    ) -> Self {
        let table_row_order = Self::initial_table_row_order(&page);
        let table_column_widths = Self::initial_table_column_widths(&page);
        let output_step_visible_chars = Self::initial_output_step_visible_chars(&page);
        let command_filter_inputs = Self::initial_command_filter_inputs(&page, _context);
        Self {
            focus_handle: _context.focus_handle(),
            table_filter_focus: _context.focus_handle(),
            title,
            page,
            palette,
            origin_plugin,
            origin_log_sources,
            origin_log_files,
            current_window_command_generations: PluginWindowCommandGenerationState::new(),
            table_sort: None,
            table_filter_input: SingleLineTextInputState::empty(),
            table_filter_selection_drag: None,
            table_filter_layout: None,
            command_filter_inputs,
            table_row_order,
            table_column_widths,
            table_scroll_handle: UniformListScrollHandle::new(),
            table_x_scroll_handle: ScrollHandle::new(),
            table_scrollbar_drag: None,
            table_text_selection: None,
            output_step_visible_chars,
            table_cell_bounds: BTreeMap::new(),
        }
    }

    /// 替换当前插件页面。
    ///
    /// 业务意图：
    /// - 用户连续执行不同插件命令时复用一个窗口，减少桌面窗口数量，同时确保新结果立刻可见。
    pub(in crate::app) fn set_page(
        &mut self,
        title: String,
        page: PluginPage,
        palette: AppThemePalette,
        origin_plugin: Option<PluginDefinition>,
        origin_log_sources: BTreeMap<String, LogFileSource>,
        origin_log_files: Vec<PluginLogFile>,
        context: &mut Context<Self>,
    ) {
        let previous_step_visible_chars = self.output_step_visible_chars.clone();
        self.title = title;
        self.page = page;
        self.palette = palette;
        self.origin_plugin = origin_plugin;
        self.origin_log_sources = origin_log_sources;
        self.origin_log_files = origin_log_files;
        self.current_window_command_generations.cancel();
        self.table_sort = None;
        self.table_filter_input = SingleLineTextInputState::empty();
        self.table_filter_selection_drag = None;
        self.table_filter_layout = None;
        self.command_filter_inputs = Self::initial_command_filter_inputs(&self.page, context);
        self.table_row_order = Self::initial_table_row_order(&self.page);
        self.table_column_widths = Self::initial_table_column_widths(&self.page);
        self.table_scroll_handle = UniformListScrollHandle::new();
        self.table_x_scroll_handle = ScrollHandle::new();
        self.table_scrollbar_drag = None;
        self.table_text_selection = None;
        self.reconcile_output_step_visible_chars(previous_step_visible_chars);
        self.table_cell_bounds.clear();
        context.notify();
    }

    /// 更新当前插件窗口的处理进度。
    ///
    /// 业务意图：
    /// - 插件最终页面返回前，窗口先展示实时进度；每次进度事件只更新 progress 字段，避免覆盖已有标题和其它页面结构。
    pub(in crate::app) fn set_progress(
        &mut self,
        progress: PluginCommandProgress,
        context: &mut Context<Self>,
    ) {
        self.page.progress = Some(progress);
        context.notify();
    }

    /// 追加一行插件瀑布流输出。
    ///
    /// 业务意图：
    /// - 流式插件窗口会在后台处理时不断产出诊断内容；追加输出不能替换当前页面，否则用户会丢失之前文件的分析结果。
    /// - 输出行保持插件返回顺序，宿主不排序、不过滤，保证“按顺序分析”的业务语义可见。
    pub(in crate::app) fn append_output_line(
        &mut self,
        line: PluginOutputLine,
        context: &mut Context<Self>,
    ) {
        self.page.output.push(line);
        context.notify();
    }

    /// 开始一个步骤式输出块。
    ///
    /// 业务意图：
    /// - 插件运行中可能比初始 manifest 更早或更完整地声明步骤；同 ID 步骤到达时更新标题和状态，不重复插入。
    /// - 新步骤从 0 个可见字符开始，由渲染层逐帧推进打字机效果。
    pub(in crate::app) fn start_output_step(
        &mut self,
        step: PluginOutputStep,
        context: &mut Context<Self>,
    ) {
        if let Some(existing) = self
            .page
            .output_steps
            .iter_mut()
            .find(|existing| existing.id == step.id)
        {
            *existing = step;
        } else {
            self.output_step_visible_chars
                .entry(step.id.clone())
                .or_insert(0);
            self.page.output_steps.push(step);
        }
        context.notify();
    }

    /// 向指定步骤追加正文文本块。
    ///
    /// 边界条件：
    /// - 如果事件先于初始步骤到达，宿主创建一个兜底步骤，避免第三方插件事件乱序导致内容丢失。
    /// - 追加时不直接增加可见字符数，确保新文本仍由 UI 打字机动画逐字显示。
    pub(in crate::app) fn append_output_step_text(
        &mut self,
        step_id: String,
        text: String,
        context: &mut Context<Self>,
    ) {
        if let Some(step) = self
            .page
            .output_steps
            .iter_mut()
            .find(|step| step.id == step_id)
        {
            step.content.push_str(&text);
        } else {
            self.page.output_steps.push(PluginOutputStep {
                id: step_id.clone(),
                loading_text: "正在处理插件步骤".to_string(),
                done_text: None,
                status: PluginOutputStepStatus::Running,
                content: text,
            });
        }
        self.output_step_visible_chars.entry(step_id).or_insert(0);
        context.notify();
    }

    /// 结束指定步骤式输出块。
    ///
    /// 业务意图：
    /// - 步骤标题在完成后替换为插件给出的中文结果文案；正文仍继续按已追加内容展示。
    /// - 读取失败不阻断窗口展示，状态标记为失败即可让用户看到完成但有问题。
    pub(in crate::app) fn finish_output_step(
        &mut self,
        step_id: String,
        done_text: String,
        status: PluginOutputStepStatus,
        context: &mut Context<Self>,
    ) {
        if let Some(step) = self
            .page
            .output_steps
            .iter_mut()
            .find(|step| step.id == step_id)
        {
            step.done_text = Some(done_text);
            step.status = status;
        } else {
            self.page.output_steps.push(PluginOutputStep {
                id: step_id.clone(),
                loading_text: "正在处理插件步骤".to_string(),
                done_text: Some(done_text),
                status,
                content: String::new(),
            });
            self.output_step_visible_chars.entry(step_id).or_insert(0);
        }
        context.notify();
    }

    /// 应用一个插件运行时事件到当前窗口。
    ///
    /// 业务意图：
    /// - 主窗口插件命令和行内延迟命令都复用同一套窗口事件处理，保证瀑布流输出和步骤式输出行为一致。
    /// - 进度事件在调用方可能会做状态栏文案合并，这里仍保留兜底处理，便于未来直接转发完整事件序列。
    pub(in crate::app) fn apply_plugin_runtime_event(
        &mut self,
        event: PluginCommandRuntimeEvent,
        context: &mut Context<Self>,
    ) {
        match event {
            PluginCommandRuntimeEvent::Progress(progress) => self.set_progress(progress, context),
            PluginCommandRuntimeEvent::OutputAppend(line) => self.append_output_line(line, context),
            PluginCommandRuntimeEvent::OutputStepStart(step) => {
                self.start_output_step(step, context)
            }
            PluginCommandRuntimeEvent::OutputStepAppend { step_id, text } => {
                self.append_output_step_text(step_id, text, context)
            }
            PluginCommandRuntimeEvent::OutputStepFinish {
                step_id,
                done_text,
                status,
            } => self.finish_output_step(step_id, done_text, status, context),
        }
    }

    /// 为新页面初始化步骤可见字符数。
    fn initial_output_step_visible_chars(page: &PluginPage) -> BTreeMap<String, usize> {
        page.output_steps
            .iter()
            .map(|step| (step.id.clone(), 0))
            .collect()
    }

    /// 页面替换时保留已经展示过的步骤字符数。
    ///
    /// 边界条件：
    /// - 最终响应可能携带更完整的正文快照；已显示字符数不能超过新正文长度。
    /// - 如果是全新页面或步骤 ID 不同，则从 0 开始显示，避免串用旧窗口动画状态。
    fn reconcile_output_step_visible_chars(&mut self, previous: BTreeMap<String, usize>) {
        self.output_step_visible_chars = self
            .page
            .output_steps
            .iter()
            .map(|step| {
                let max_chars = Self::output_step_content_char_count(step);
                let visible = previous.get(&step.id).copied().unwrap_or(0).min(max_chars);
                (step.id.clone(), visible)
            })
            .collect();
    }

    /// 返回步骤正文字符数，使用字符而不是字节以正确处理中文。
    fn output_step_content_char_count(step: &PluginOutputStep) -> usize {
        step.content.chars().count()
    }

    /// 初始化插件表格行顺序。
    ///
    /// 业务意图：
    /// - 插件返回的表格顺序本身可能已经有业务含义，宿主初始状态必须保持插件声明的顺序。
    /// - 宿主只保存索引映射，避免为了排序能力复制大批量单元格字符串。
    fn initial_table_row_order(page: &PluginPage) -> Vec<usize> {
        page.table
            .as_ref()
            .map(|table| (0..table.rows.len()).collect::<Vec<_>>())
            .unwrap_or_default()
    }

    /// 初始化插件表格列宽缓存。
    ///
    /// 业务意图：
    /// - 横向滚动是否出现取决于整张表的最小内容宽度；页面加载时一次性估算列宽，可以避免滚动虚拟列表时反复扫描所有行。
    /// - 估算保守覆盖任意长文本，保证内容放不下时生成横向滚动条，而不是只在固定小列内截断。
    fn initial_table_column_widths(page: &PluginPage) -> Vec<f32> {
        page.table
            .as_ref()
            .map(|table| Self::plugin_table_column_widths(table, table.headers.len().max(1)))
            .unwrap_or_default()
    }

    /// 初始化插件声明式命令过滤输入状态。
    ///
    /// 业务意图：
    /// - 插件重新生成页面后会把已生效的过滤条件写回控件声明，宿主用它恢复输入框显示。
    /// - 普通插件表格没有命令过滤器时保持空集合，避免上一页条件串到下一页。
    fn initial_command_filter_inputs(
        page: &PluginPage,
        context: &mut Context<Self>,
    ) -> Vec<PluginTableCommandFilterInputState> {
        page.table
            .as_ref()
            .and_then(|table| table.command_filter.as_ref())
            .map(|filter| {
                filter
                    .controls
                    .iter()
                    .map(|control| {
                        let view_month = date_time_picker_month_from_text(&control.value);
                        PluginTableCommandFilterInputState {
                            key: control.key.clone(),
                            input: SingleLineTextInputState::from_text(control.value.clone()),
                            focus: context.focus_handle(),
                            selection_drag: None,
                            layout: None,
                            placeholder: control.placeholder.clone(),
                            control_kind: control.kind,
                            icon: control.icon.clone(),
                            width: px(control.width.unwrap_or(180).max(96) as f32),
                            date_time_picker_open: false,
                            date_time_picker_view_year: view_month.year,
                            date_time_picker_view_month: view_month.month,
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    /// 读取插件表格过滤输入框绘制快照。
    fn table_input_snapshot(
        &self,
        input_kind: PluginTableInputKind,
    ) -> SingleLineTextInputSnapshot {
        self.table_input_state(input_kind)
            .map(|input| SingleLineTextInputSnapshot {
                text: input.text.clone(),
                selection_range: input.selection_range.clone(),
                marked_range: input.marked_range.clone(),
                horizontal_scroll_px: input.horizontal_scroll_px,
            })
            .unwrap_or_else(|| SingleLineTextInputSnapshot {
                text: String::new(),
                selection_range: 0..0,
                marked_range: None,
                horizontal_scroll_px: 0.0,
            })
    }

    /// 记录插件表格输入框当前布局。
    fn store_table_input_layout(
        &mut self,
        input_kind: PluginTableInputKind,
        layout: PluginTableFilterInputLayout,
    ) {
        if let Some(input) = self.table_input_state_mut(input_kind) {
            input.horizontal_scroll_px = layout.horizontal_scroll_px;
        }
        if let Some(layout_slot) = self.table_input_layout_mut(input_kind) {
            *layout_slot = Some(layout);
        }
    }

    /// 返回指定插件表格输入框的只读状态。
    fn table_input_state(
        &self,
        input_kind: PluginTableInputKind,
    ) -> Option<&SingleLineTextInputState> {
        match input_kind {
            PluginTableInputKind::QuickFilter => Some(&self.table_filter_input),
            PluginTableInputKind::CommandFilter(index) => self
                .command_filter_inputs
                .get(index)
                .map(|state| &state.input),
        }
    }

    /// 返回指定插件表格输入框的可变状态。
    fn table_input_state_mut(
        &mut self,
        input_kind: PluginTableInputKind,
    ) -> Option<&mut SingleLineTextInputState> {
        match input_kind {
            PluginTableInputKind::QuickFilter => Some(&mut self.table_filter_input),
            PluginTableInputKind::CommandFilter(index) => self
                .command_filter_inputs
                .get_mut(index)
                .map(|state| &mut state.input),
        }
    }

    /// 返回指定插件表格输入框的焦点句柄。
    fn table_input_focus(&self, input_kind: PluginTableInputKind) -> Option<&gpui::FocusHandle> {
        match input_kind {
            PluginTableInputKind::QuickFilter => Some(&self.table_filter_focus),
            PluginTableInputKind::CommandFilter(index) => self
                .command_filter_inputs
                .get(index)
                .map(|state| &state.focus),
        }
    }

    /// 返回指定插件表格输入框的鼠标拖选锚点。
    fn table_input_selection_drag_mut(
        &mut self,
        input_kind: PluginTableInputKind,
    ) -> Option<&mut Option<usize>> {
        match input_kind {
            PluginTableInputKind::QuickFilter => Some(&mut self.table_filter_selection_drag),
            PluginTableInputKind::CommandFilter(index) => self
                .command_filter_inputs
                .get_mut(index)
                .map(|state| &mut state.selection_drag),
        }
    }

    /// 返回指定插件表格输入框最近一次布局。
    fn table_input_layout(
        &self,
        input_kind: PluginTableInputKind,
    ) -> Option<&PluginTableFilterInputLayout> {
        match input_kind {
            PluginTableInputKind::QuickFilter => self.table_filter_layout.as_ref(),
            PluginTableInputKind::CommandFilter(index) => self
                .command_filter_inputs
                .get(index)
                .and_then(|state| state.layout.as_ref()),
        }
    }

    /// 返回指定插件表格输入框布局的可变槽位。
    fn table_input_layout_mut(
        &mut self,
        input_kind: PluginTableInputKind,
    ) -> Option<&mut Option<PluginTableFilterInputLayout>> {
        match input_kind {
            PluginTableInputKind::QuickFilter => Some(&mut self.table_filter_layout),
            PluginTableInputKind::CommandFilter(index) => self
                .command_filter_inputs
                .get_mut(index)
                .map(|state| &mut state.layout),
        }
    }

    /// 返回当前获得平台输入焦点的插件表格输入框。
    fn focused_table_input_kind(&self, window: &Window) -> Option<PluginTableInputKind> {
        std::iter::once(PluginTableInputKind::QuickFilter)
            .chain((0..self.command_filter_inputs.len()).map(PluginTableInputKind::CommandFilter))
            .find(|input_kind| {
                self.table_input_focus(*input_kind)
                    .is_some_and(|focus| focus.is_focused(window))
            })
    }

    /// 返回当前正在鼠标拖选的插件表格输入框。
    fn dragging_table_input_kind(&self) -> Option<PluginTableInputKind> {
        if self.table_filter_selection_drag.is_some() {
            return Some(PluginTableInputKind::QuickFilter);
        }
        self.command_filter_inputs
            .iter()
            .enumerate()
            .find_map(|(index, state)| {
                state
                    .selection_drag
                    .is_some()
                    .then_some(PluginTableInputKind::CommandFilter(index))
            })
    }

    /// 结束所有插件表格输入框拖选。
    fn finish_all_table_input_mouse_selection(&mut self, context: &mut Context<Self>) {
        for input_kind in std::iter::once(PluginTableInputKind::QuickFilter)
            .chain((0..self.command_filter_inputs.len()).map(PluginTableInputKind::CommandFilter))
        {
            self.finish_table_input_mouse_selection(input_kind, context);
        }
    }

    /// 过滤关键字发生变化后刷新可见行。
    ///
    /// 业务意图：
    /// - 插件表格过滤只改变本地行索引顺序，不修改插件返回数据和行内动作，确保行内按钮仍指向原始行。
    /// - 过滤后重置滚动和文本选区，避免旧可见行的滚动位置或选中文本落到新结果上造成错位。
    fn rebuild_table_after_filter_change(&mut self, context: &mut Context<Self>) {
        self.rebuild_table_row_order();
        self.table_scroll_handle = UniformListScrollHandle::new();
        self.table_scrollbar_drag = None;
        self.table_text_selection = None;
        self.table_cell_bounds.clear();
        context.notify();
    }

    /// 点击表头后更新表格排序。
    ///
    /// 边界条件：
    /// - 同一列连续点击时在升序和降序之间切换。
    /// - 切换排序后重置虚拟列表滚动位置，让用户从排序后的顶部开始查看。
    fn sort_table_by_column(&mut self, column_index: usize, context: &mut Context<Self>) {
        let Some(table) = self.page.table.as_ref() else {
            return;
        };
        if column_index >= table.headers.len() {
            return;
        }
        if Self::plugin_table_action_column_index(table) == Some(column_index) {
            return;
        }
        let ascending = self
            .table_sort
            .filter(|sort| sort.column_index == column_index)
            .map(|sort| !sort.ascending)
            .unwrap_or(true);
        self.table_sort = Some(PluginTableSortState {
            column_index,
            ascending,
        });
        self.rebuild_table_row_order();
        self.table_scroll_handle = UniformListScrollHandle::new();
        self.table_scrollbar_drag = None;
        self.table_text_selection = None;
        self.table_cell_bounds.clear();
        context.notify();
    }

    /// 处理插件表格输入框键盘事件。
    ///
    /// 业务意图：
    /// - 普通字符交给 `EntityInputHandler` 和平台 IME；删除、方向键、复制粘贴等编辑键在这里统一维护状态。
    /// - 本地关键字过滤每次文本变化只重建可见行索引；插件命令过滤只更新输入文本，按“应用”或 Enter 后才重新执行插件命令。
    fn handle_table_input_key_down(
        &mut self,
        input_kind: PluginTableInputKind,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if MainView::is_paste_keystroke(&event.keystroke) {
            let clipboard_text = context
                .read_from_clipboard()
                .and_then(|item| item.text())
                .map(|text| MainView::sanitize_search_input_text(&text));
            if let Some(text) = clipboard_text
                && !text.is_empty()
            {
                self.replace_table_input_selection(input_kind, &text);
                self.after_table_input_text_changed(input_kind, context);
            }
            context.stop_propagation();
            return;
        }

        if MainView::is_copy_keystroke(&event.keystroke) {
            let Some(input) = self.table_input_state(input_kind) else {
                return;
            };
            let range =
                MainView::clamp_search_text_range(&input.text, input.selection_range.clone());
            if range.start < range.end {
                context
                    .write_to_clipboard(ClipboardItem::new_string(input.text[range].to_string()));
            }
            context.stop_propagation();
            return;
        }

        if MainView::is_select_all_keystroke(&event.keystroke) {
            let Some(input) = self.table_input_state_mut(input_kind) else {
                return;
            };
            input.marked_range = None;
            input.selection_range = 0..input.text.len();
            context.stop_propagation();
            context.notify();
            return;
        }

        match event.keystroke.key.as_str() {
            "left" => {
                {
                    let Some(input) = self.table_input_state_mut(input_kind) else {
                        return;
                    };
                    input.marked_range = None;
                    if event.keystroke.modifiers.shift {
                        input.selection_range.end = MainView::previous_search_text_boundary(
                            &input.text,
                            input.selection_range.end,
                        );
                        input.selection_range = MainView::clamp_search_text_range(
                            &input.text,
                            input.selection_range.clone(),
                        );
                    } else if input.selection_range.start != input.selection_range.end {
                        let cursor = input.selection_range.start;
                        input.selection_range = cursor..cursor;
                    } else {
                        let cursor = MainView::previous_search_text_boundary(
                            &input.text,
                            input.selection_range.end,
                        );
                        input.selection_range = cursor..cursor;
                    }
                }
                context.stop_propagation();
                context.notify();
            }
            "right" => {
                {
                    let Some(input) = self.table_input_state_mut(input_kind) else {
                        return;
                    };
                    input.marked_range = None;
                    if event.keystroke.modifiers.shift {
                        input.selection_range.end = MainView::next_search_text_boundary(
                            &input.text,
                            input.selection_range.end,
                        );
                        input.selection_range = MainView::clamp_search_text_range(
                            &input.text,
                            input.selection_range.clone(),
                        );
                    } else if input.selection_range.start != input.selection_range.end {
                        let cursor = input.selection_range.end;
                        input.selection_range = cursor..cursor;
                    } else {
                        let cursor = MainView::next_search_text_boundary(
                            &input.text,
                            input.selection_range.end,
                        );
                        input.selection_range = cursor..cursor;
                    }
                }
                context.stop_propagation();
                context.notify();
            }
            "up" => {
                let Some(input) = self.table_input_state_mut(input_kind) else {
                    return;
                };
                input.marked_range = None;
                input.selection_range = 0..0;
                context.stop_propagation();
                context.notify();
            }
            "down" => {
                let Some(input) = self.table_input_state_mut(input_kind) else {
                    return;
                };
                input.marked_range = None;
                let cursor = input.text.len();
                input.selection_range = cursor..cursor;
                context.stop_propagation();
                context.notify();
            }
            "backspace" => {
                {
                    let Some(input) = self.table_input_state_mut(input_kind) else {
                        return;
                    };
                    if let Some(range) = input.marked_range.take().or_else(|| {
                        (input.selection_range.start != input.selection_range.end)
                            .then(|| input.selection_range.clone())
                    }) {
                        input.text.replace_range(range.clone(), "");
                        input.selection_range = range.start..range.start;
                    } else if let Some((previous_index, _)) = input.text
                        [..input.selection_range.end]
                        .char_indices()
                        .next_back()
                    {
                        input
                            .text
                            .replace_range(previous_index..input.selection_range.end, "");
                        input.selection_range = previous_index..previous_index;
                    }
                }
                self.after_table_input_text_changed(input_kind, context);
                context.stop_propagation();
            }
            "delete" => {
                {
                    let Some(input) = self.table_input_state_mut(input_kind) else {
                        return;
                    };
                    if let Some(range) = input.marked_range.take().or_else(|| {
                        (input.selection_range.start != input.selection_range.end)
                            .then(|| input.selection_range.clone())
                    }) {
                        input.text.replace_range(range.clone(), "");
                        input.selection_range = range.start..range.start;
                    } else if let Some((next_index, next_character)) = input.text
                        [input.selection_range.end..]
                        .char_indices()
                        .next()
                    {
                        let start = input.selection_range.end + next_index;
                        let end = start + next_character.len_utf8();
                        input.text.replace_range(start..end, "");
                        input.selection_range = start..start;
                    }
                }
                self.after_table_input_text_changed(input_kind, context);
                context.stop_propagation();
            }
            "escape" => {
                if let PluginTableInputKind::CommandFilter(index) = input_kind
                    && self
                        .command_filter_inputs
                        .get(index)
                        .is_some_and(|state| state.date_time_picker_open)
                {
                    self.close_command_filter_date_time_picker(index);
                    context.notify();
                    context.stop_propagation();
                    return;
                }
                let had_text = self
                    .table_input_state(input_kind)
                    .is_some_and(|input| !input.text.is_empty());
                if had_text {
                    if let Some(input) = self.table_input_state_mut(input_kind) {
                        input.set_text(String::new());
                    }
                    self.after_table_input_text_changed(input_kind, context);
                }
                context.stop_propagation();
            }
            "enter" => {
                if input_kind != PluginTableInputKind::QuickFilter
                    && let Some(command) = self.current_command_filter_command()
                {
                    self.apply_plugin_table_command_filter(command, context);
                }
                context.stop_propagation();
            }
            _ => {}
        }
    }

    /// 表格输入文本变化后的业务副作用。
    fn after_table_input_text_changed(
        &mut self,
        input_kind: PluginTableInputKind,
        context: &mut Context<Self>,
    ) {
        if input_kind == PluginTableInputKind::QuickFilter {
            self.rebuild_table_after_filter_change(context);
        } else {
            context.notify();
        }
    }

    /// 替换插件表格输入框当前选区。
    fn replace_table_input_selection(
        &mut self,
        input_kind: PluginTableInputKind,
        replacement: &str,
    ) {
        let replacement = MainView::sanitize_search_input_text(replacement);
        let Some(input) = self.table_input_state_mut(input_kind) else {
            return;
        };
        let range = input
            .marked_range
            .take()
            .unwrap_or_else(|| input.selection_range.clone());
        let range = MainView::clamp_search_text_range(&input.text, range);
        input.text.replace_range(range.clone(), &replacement);
        let cursor = range.start + replacement.len();
        input.selection_range = cursor..cursor;
    }

    /// 处理插件表格输入框鼠标按下。
    fn start_table_input_mouse_selection(
        &mut self,
        input_kind: PluginTableInputKind,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        self.close_command_filter_date_time_pickers_for_input_focus(input_kind);
        let index = self.table_input_index_at_position(input_kind, event.position);
        let Some(text) = self
            .table_input_state(input_kind)
            .map(|input| input.text.clone())
        else {
            return;
        };
        let range = match event.click_count {
            0 | 1 => index..index,
            2 => MainView::search_text_word_range_for_index(&text, index),
            _ => 0..text.len(),
        };
        {
            let Some(input) = self.table_input_state_mut(input_kind) else {
                return;
            };
            input.selection_range = MainView::clamp_search_text_range(&input.text, range);
            input.marked_range = None;
        }
        if let Some(selection_drag) = self.table_input_selection_drag_mut(input_kind) {
            *selection_drag = (event.click_count <= 1).then_some(index);
        }
        if let Some(focus) = self.table_input_focus(input_kind) {
            window.focus(focus);
        }
        context.notify();
    }

    /// 根据鼠标位置更新插件表格输入框拖选范围。
    fn update_table_input_mouse_selection(
        &mut self,
        input_kind: PluginTableInputKind,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) {
        let Some(anchor) = self
            .table_input_selection_drag_mut(input_kind)
            .and_then(|drag| *drag)
        else {
            return;
        };
        let index = self.table_input_index_at_position(input_kind, position);
        let Some(input) = self.table_input_state_mut(input_kind) else {
            return;
        };
        input.selection_range = MainView::clamp_search_text_range(&input.text, anchor..index);
        context.notify();
    }

    /// 结束插件表格输入框拖选。
    fn finish_table_input_mouse_selection(
        &mut self,
        input_kind: PluginTableInputKind,
        context: &mut Context<Self>,
    ) {
        if self
            .table_input_selection_drag_mut(input_kind)
            .is_some_and(|drag| drag.take().is_some())
        {
            context.notify();
        }
    }

    /// 将窗口坐标转换为插件表格输入框 UTF-8 字节下标。
    fn table_input_index_at_position(
        &self,
        input_kind: PluginTableInputKind,
        position: Point<Pixels>,
    ) -> usize {
        let Some(input) = self.table_input_state(input_kind) else {
            return 0;
        };
        let Some(layout) = self.table_input_layout(input_kind) else {
            return input.text.len();
        };
        let relative_x =
            position.x - layout.bounds.left() + px(layout.horizontal_scroll_px).max(px(0.0));
        let index = layout.line.closest_index_for_x(relative_x.max(px(0.0)));
        MainView::clamp_search_text_range(&input.text, index..index).start
    }

    /// 开始选择插件表格单元格文本。
    ///
    /// 业务意图：
    /// - 单击只定位光标，不复制整格；拖动会扩展选区，双击选择连续非空白片段，三击选择整段单元格文本。
    /// - 选中时聚焦插件窗口根节点，保证随后 `Cmd/Ctrl+C` 或 `Ctrl+C` 能复制当前文本范围。
    fn start_table_cell_text_selection(
        &mut self,
        row_index: usize,
        column_index: usize,
        text: String,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let index = self.table_cell_text_index_at_position(
            row_index,
            column_index,
            &text,
            event.position,
            window,
        );
        let range = match event.click_count {
            0 | 1 => index..index,
            2 => MainView::search_text_word_range_for_index(&text, index),
            _ => 0..text.len(),
        };
        let range = Self::normalize_plugin_table_text_range(&text, range);
        self.table_text_selection = Some(PluginTableTextSelection {
            row_index,
            column_index,
            text,
            drag_anchor: range.start,
            dragging: event.click_count <= 1,
            range,
        });
        window.focus(&self.focus_handle);
        context.notify();
    }

    /// 返回指定单元格当前文本选择范围。
    fn plugin_table_text_selection_for_cell(
        &self,
        row_index: usize,
        column_index: usize,
    ) -> Option<Range<usize>> {
        self.table_text_selection
            .as_ref()
            .filter(|selection| {
                selection.row_index == row_index && selection.column_index == column_index
            })
            .map(|selection| selection.range.clone())
    }

    /// 记录当前帧单元格文本边界。
    ///
    /// 业务意图：
    /// - 自绘文本元素拥有真实布局边界，窗口层拖选逻辑只保存边界缓存，不重复推导 GPUI flex 布局。
    fn store_plugin_table_cell_bounds(
        &mut self,
        row_index: usize,
        column_index: usize,
        bounds: Bounds<Pixels>,
    ) {
        self.table_cell_bounds
            .insert((row_index, column_index), bounds);
    }

    /// 将鼠标位置换算为单元格文本字节下标。
    ///
    /// 边界条件：
    /// - 如果单元格尚未绘制或已经滚出可视区，则回退到文本末尾，避免拖选过程中出现崩溃。
    /// - `ShapedLine` 返回的下标仍需要夹到 UTF-8 边界，保证中文和其它多字节文本可安全切片。
    fn table_cell_text_index_at_position(
        &self,
        row_index: usize,
        column_index: usize,
        text: &str,
        position: Point<Pixels>,
        window: &mut Window,
    ) -> usize {
        let Some(bounds) = self.table_cell_bounds.get(&(row_index, column_index)) else {
            return text.len();
        };
        let line = PluginTableSelectableTextElement::shape_text(text, self.palette, window);
        // `bounds` 是 GPUI 在横向滚动偏移后写回的窗口坐标；这里不能再叠加横向滚动量，
        // 否则表格横向滚动后点击/拖选会命中到更靠后的字符，表现为选区错位。
        let relative_x = (position.x - bounds.left()).max(px(0.0));
        let index = line.closest_index_for_x(relative_x);
        MainView::clamp_search_text_range(text, index..index).start
    }

    /// 更新正在拖动的插件表格文本选区。
    fn update_table_cell_text_selection(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(selection_snapshot) = self.table_text_selection.clone() else {
            return;
        };
        if !selection_snapshot.dragging {
            return;
        }
        let index = self.table_cell_text_index_at_position(
            selection_snapshot.row_index,
            selection_snapshot.column_index,
            &selection_snapshot.text,
            position,
            window,
        );
        let range = Self::normalize_plugin_table_text_range(
            &selection_snapshot.text,
            selection_snapshot.drag_anchor..index,
        );
        if let Some(selection) = self.table_text_selection.as_mut() {
            selection.range = range;
        }
        context.notify();
    }

    /// 结束表格文本拖选。
    fn finish_table_cell_text_selection(&mut self, context: &mut Context<Self>) {
        if let Some(selection) = self.table_text_selection.as_mut()
            && selection.dragging
        {
            selection.dragging = false;
            context.notify();
        }
    }

    /// 将插件单元格文本范围夹到安全字符边界。
    fn normalize_plugin_table_text_range(text: &str, range: Range<usize>) -> Range<usize> {
        MainView::clamp_search_text_range(text, range)
    }

    /// 处理插件窗口根节点键盘事件。
    ///
    /// 业务意图：
    /// - 用户拖选表格单元格文本后，使用系统复制快捷键把当前文本范围写入剪贴板。
    /// - 空选区不消费复制快捷键，避免单击单元格后误复制整格，也为未来插件窗口输入控件保留默认复制行为。
    fn handle_plugin_window_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if MainView::is_copy_keystroke(&event.keystroke)
            && let Some(selection) = self.table_text_selection.as_ref()
        {
            let range =
                Self::normalize_plugin_table_text_range(&selection.text, selection.range.clone());
            if range.start < range.end {
                context.write_to_clipboard(ClipboardItem::new_string(
                    selection.text[range].to_string(),
                ));
                context.stop_propagation();
            }
        }
    }

    /// 按当前排序状态重建表格行顺序。
    ///
    /// 业务意图：
    /// - 只移动行下标，不移动原始 `PluginPageTable`，让窗口状态和插件响应保持可追踪。
    /// - 数字列按数值排序，时间和文本列按大小写不敏感文本排序，满足通用插件表格的基础交互。
    fn rebuild_table_row_order(&mut self) {
        let Some(table) = self.page.table.as_ref() else {
            self.table_row_order.clear();
            return;
        };
        self.table_row_order = (0..table.rows.len())
            .filter(|row_index| {
                table.rows.get(*row_index).is_some_and(|row| {
                    Self::plugin_table_row_matches_filter(row, &self.table_filter_input.text)
                })
            })
            .collect();
        let Some(sort) = self.table_sort else {
            return;
        };
        self.table_row_order.sort_by(|left_index, right_index| {
            let left_value = table
                .rows
                .get(*left_index)
                .and_then(|row| row.get(sort.column_index))
                .map(String::as_str)
                .unwrap_or_default();
            let right_value = table
                .rows
                .get(*right_index)
                .and_then(|row| row.get(sort.column_index))
                .map(String::as_str)
                .unwrap_or_default();
            let value_order = compare_plugin_table_cells(left_value, right_value);
            let ordered_value = if sort.ascending {
                value_order
            } else {
                value_order.reverse()
            };
            ordered_value.then_with(|| left_index.cmp(right_index))
        });
    }

    /// 判断表格行是否命中过滤关键字。
    ///
    /// 业务意图：
    /// - “模糊搜索”面向插件表格的快速定位，不改变插件业务搜索语义；用户输入的每个空白分隔关键字都在整行任意列中做忽略大小写包含匹配。
    /// - 多关键字采用全部命中规则，方便用户在任意插件列之间逐步收窄结果。
    fn plugin_table_row_matches_filter(row: &[String], query: &str) -> bool {
        let tokens = Self::plugin_table_filter_tokens(query);
        if tokens.is_empty() {
            return true;
        }
        let row_text = row.join("\u{1f}").to_lowercase();
        tokens.iter().all(|token| row_text.contains(token))
    }

    /// 规范化插件表格过滤关键字。
    fn plugin_table_filter_tokens(query: &str) -> Vec<String> {
        query
            .split_whitespace()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_lowercase)
            .collect()
    }

    /// 计算插件表格列宽。
    ///
    /// 业务意图：
    /// - 宿主不理解插件业务列含义，因此只按表头文本长度估算最小宽度，避免把某个插件的字段名固化到主程序。
    /// - 操作列由 `row_actions` 协议承载，固定宽度可以保证按钮区域稳定，其它列会继续参考内容采样扩宽。
    fn plugin_table_column_width(header: &str) -> f32 {
        Self::plugin_table_text_estimated_width(header)
            .clamp(96.0, PLUGIN_TABLE_FALLBACK_COLUMN_WIDTH)
    }

    /// 计算插件表格最小宽度。
    ///
    /// 业务意图：
    /// - 表格应优先撑满容器并让主文本列动态吸收剩余宽度；只有所有列的最小宽度确实超过视口时才出现横向滚动。
    /// - 宿主只基于通用列宽估算和插件声明的动作列计算最小宽度，不依赖任何插件业务表头。
    #[cfg(test)]
    fn plugin_table_min_width(table: &PluginPageTable, column_count: usize) -> f32 {
        Self::plugin_table_min_width_from_widths(&Self::plugin_table_column_widths(
            table,
            column_count,
        ))
    }

    /// 根据已缓存列宽计算表格最小宽度。
    fn plugin_table_min_width_from_widths(column_widths: &[f32]) -> f32 {
        column_widths.iter().sum::<f32>().max(720.0)
    }

    /// 返回当前渲染应使用的列宽。
    ///
    /// 边界条件：
    /// - 旧插件或测试构造的窗口可能没有缓存列宽；此时回退到即时计算，保证兼容性。
    fn table_column_widths_for_render(
        &self,
        table: &PluginPageTable,
        column_count: usize,
    ) -> Vec<f32> {
        if self.table_column_widths.len() == column_count {
            return self.table_column_widths.clone();
        }
        Self::plugin_table_column_widths(table, column_count)
    }

    /// 计算当前表格所有列宽。
    fn plugin_table_column_widths(table: &PluginPageTable, column_count: usize) -> Vec<f32> {
        (0..column_count)
            .map(|column_index| {
                let base_width = table
                    .headers
                    .get(column_index)
                    .map(|header| Self::plugin_table_column_width(header))
                    .unwrap_or(PLUGIN_TABLE_FALLBACK_COLUMN_WIDTH);
                Self::plugin_table_content_column_width(table, column_index, base_width)
            })
            .collect()
    }

    /// 按表格内容估算指定列宽。
    ///
    /// 业务意图：
    /// - 插件的任意文本列都可能远长于表头固定宽度；如果不参考内容，横向滚动永远不会出现，用户只能看到截断文本。
    /// - 估算只在页面切换或测试中调用，不在虚拟列表每行渲染时调用，避免大表格滚动卡顿。
    fn plugin_table_content_column_width(
        table: &PluginPageTable,
        column_index: usize,
        base_width: f32,
    ) -> f32 {
        if Self::plugin_table_action_column_index(table) == Some(column_index) {
            return base_width;
        }
        let preferred = table
            .rows
            .iter()
            .take(PLUGIN_TABLE_COLUMN_WIDTH_SAMPLE_ROWS)
            .filter_map(|row| row.get(column_index))
            .map(|text| Self::plugin_table_text_estimated_width(text))
            .fold(base_width, f32::max);
        preferred.clamp(base_width, PLUGIN_TABLE_MAX_CONTENT_COLUMN_WIDTH)
    }

    /// 估算单元格文本宽度。
    ///
    /// 边界条件：
    /// - 中文、emoji 和宽字符在不同平台字体下宽度不同；这里按字符数做近似，只用于决定是否需要横向滚动，不用于精确光标命中。
    fn plugin_table_text_estimated_width(text: &str) -> f32 {
        let content_chars = text.chars().count() as f32;
        (content_chars * PLUGIN_TABLE_APPROX_CHAR_WIDTH + 32.0)
            .min(PLUGIN_TABLE_MAX_CONTENT_COLUMN_WIDTH)
    }

    /// 返回插件表格中的操作列下标。
    ///
    /// 业务意图：
    /// - 行动作通过 `row_actions` 承载；当插件声明动作时，宿主把“操作”列渲染成按钮区，而不是普通文本。
    /// - 操作列不参与排序，避免点击按钮列时把用户带到没有业务意义的顺序。
    fn plugin_table_action_column_index(table: &PluginPageTable) -> Option<usize> {
        if !table.row_actions.iter().any(|actions| !actions.is_empty()) {
            return None;
        }
        table
            .headers
            .iter()
            .position(|header| header == "操作")
            .or_else(|| table.headers.len().checked_sub(1))
    }

    /// 返回插件表格中负责吸收剩余宽度的列。
    ///
    /// 业务意图：
    /// - 插件表格需要在窗口宽度充足时撑满容器，不能只按最小列宽渲染后在右侧留下空白。
    /// - 默认让第一个非操作列伸展；插件如果需要重点展示某列，应把它放在动作列之外的靠前位置。
    ///
    /// 边界条件：
    /// - 操作列承载按钮，保持固定宽度更稳定，不能被拉宽后造成按钮远离数据主体。
    fn plugin_table_stretch_column_index(table: &PluginPageTable) -> Option<usize> {
        let column_count = table.headers.len();
        if column_count == 0 {
            return None;
        }
        let action_column_index = Self::plugin_table_action_column_index(table);
        (0..column_count)
            .find(|column_index| Some(*column_index) != action_column_index)
            .or(Some(0))
    }

    /// 返回带错位的新插件窗口边界。
    ///
    /// 业务意图：
    /// - 连续打开插件明细窗口时，新窗口不能完全覆盖旧窗口，否则用户会误以为点击没有响应。
    /// - 使用小步长右下偏移既能看出新窗口已打开，又不会明显偏离屏幕中心。
    fn next_plugin_window_bounds(app: &App) -> WindowBounds {
        let sequence = PLUGIN_WINDOW_OPEN_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed)
            % PLUGIN_WINDOW_OFFSET_CYCLE;
        let offset = px(sequence as f32 * PLUGIN_WINDOW_OFFSET_STEP);
        let mut bounds = WindowBounds::centered(PLUGIN_PAGE_WINDOW_SIZE, app).get_bounds();
        bounds.origin.x += offset;
        bounds.origin.y += offset;
        WindowBounds::Windowed(bounds)
    }

    /// 渲染插件进度卡片。
    fn render_progress_card(
        &self,
        progress: &PluginCommandProgress,
        palette: AppThemePalette,
    ) -> gpui::Div {
        let total = progress.total.unwrap_or_default();
        let ratio = if total > 0 {
            (progress.done as f32 / total as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let unit = progress.unit.as_deref().unwrap_or("项");
        let progress_label = if total > 0 {
            format!("{} / {} {}", progress.done.min(total), total, unit)
        } else {
            format!("已处理 {} {}", progress.done, unit)
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .p_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(palette.text))
                                    .child(progress.message.clone()),
                            )
                            .when_some(progress.detail.clone(), |body, detail| {
                                body.child(
                                    div()
                                        .text_xs()
                                        .text_color(rgb(palette.muted_text))
                                        .child(detail),
                                )
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child(progress_label),
                    ),
            )
            .child(
                div()
                    .h(px(6.0))
                    .w_full()
                    .rounded_full()
                    .bg(rgb(palette.panel))
                    .overflow_hidden()
                    .child(
                        div()
                            .h_full()
                            .w(relative(if total > 0 { ratio } else { 0.18 }))
                            .rounded_full()
                            .bg(rgb(palette.accent)),
                    ),
            )
    }

    /// 渲染插件统计摘要。
    fn render_stat_grid(&self, palette: AppThemePalette) -> gpui::Div {
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(self.page.stats.iter().map(|stat| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(28.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .px_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child(stat.label.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(stat.value.clone()),
                    )
            }))
    }

    /// 渲染插件瀑布流输出。
    ///
    /// 业务意图：
    /// - 流式插件可按文件顺序输出“开始、异常、完成”，需要保留连续阅读语义；这里使用单个滚动流而不是表格或卡片。
    /// - 输出文本来自插件协议，只按纯文本展示，不解释 Markdown、HTML 或 ANSI 控制序列，避免第三方插件影响宿主界面。
    ///
    /// 边界条件：
    /// - 单行可能包含很长路径或日志摘要，因此允许自动换行，并给容器 `min_h_0 + overflow_y_scroll`，防止挤压窗口其它区域。
    fn render_output_flow(&self, palette: AppThemePalette) -> gpui::Stateful<gpui::Div> {
        div()
            .id("plugin-page-output-flow")
            .flex()
            .flex_col()
            .gap_1()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .border_t_1()
            .border_color(rgb(palette.border))
            .pt_3()
            .children(self.page.output.iter().map(|line| {
                let (icon, color) = Self::plugin_output_line_visual(line.level, palette);
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .w_full()
                    .py_1()
                    .text_xs()
                    .line_height(px(18.0))
                    .text_color(rgb(color))
                    .child(MainView::render_lucide_icon(Some(icon), 16.0, 13.0, color))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal()
                            .child(line.text.clone()),
                    )
            }))
    }

    /// 渲染步骤式插件输出。
    ///
    /// 业务意图：
    /// - 步骤式插件按“步骤标题 -> 输出正文 -> 步骤完成”的方式呈现，用户关注的是各诊断步骤的顺序过程。
    /// - 每个步骤只用一行状态标题和一个纯文本正文区域，不使用卡片嵌套，避免大量日志诊断时视觉负担过重。
    fn render_output_steps(
        &mut self,
        palette: AppThemePalette,
        window: &mut Window,
    ) -> gpui::Stateful<gpui::Div> {
        let should_keep_animating = self.advance_output_step_typewriter();
        if should_keep_animating
            || self
                .page
                .output_steps
                .iter()
                .any(|step| step.status == PluginOutputStepStatus::Running)
        {
            window.request_animation_frame();
        }

        let spinner = Self::output_step_spinner_frame();
        let rendered_steps = self
            .page
            .output_steps
            .iter()
            .map(|step| {
                let visible = self
                    .output_step_visible_chars
                    .get(&step.id)
                    .copied()
                    .unwrap_or_default();
                let text = Self::visible_output_step_text(&step.content, visible);
                let (status_glyph, use_lucide_icon, status_color, status_text) =
                    Self::output_step_status_visual(step, spinner, palette);
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .pb_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .text_color(rgb(status_color))
                            .child(
                                div()
                                    .w(px(16.0))
                                    .h(px(16.0))
                                    .flex_none()
                                    .text_center()
                                    .line_height(px(16.0))
                                    .text_size(px(13.0))
                                    .text_color(rgb(status_color))
                                    .font_family(if use_lucide_icon {
                                        LUCIDE_FONT_FAMILY
                                    } else {
                                        LOG_VIEWER_FONT_FAMILY
                                    })
                                    .child(status_glyph),
                            )
                            .child(status_text),
                    )
                    .when(!text.is_empty(), |body| {
                        body.child(Self::render_output_step_text(text, palette))
                    })
            })
            .collect::<Vec<_>>();

        div()
            .id("plugin-page-output-steps")
            .flex()
            .flex_col()
            .gap_4()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .children(rendered_steps)
    }

    /// 按固定字符步长推进步骤正文打字机动画。
    ///
    /// 边界条件：
    /// - 使用字符数而不是字节数，保证中文路径和中文诊断不会被截成非法 UTF-8。
    /// - 每帧最多推进少量字符，既能看出流式效果，又不会因为长日志摘要导致窗口动画过慢。
    fn advance_output_step_typewriter(&mut self) -> bool {
        const TYPEWRITER_CHARS_PER_FRAME: usize = 4;
        let mut advanced = false;
        for step in &self.page.output_steps {
            let total = Self::output_step_content_char_count(step);
            let visible = self
                .output_step_visible_chars
                .entry(step.id.clone())
                .or_insert(0);
            if *visible < total {
                *visible = (*visible + TYPEWRITER_CHARS_PER_FRAME).min(total);
                advanced = true;
            }
        }
        advanced
    }

    /// 返回当前帧可见的步骤正文。
    fn visible_output_step_text(text: &str, visible_chars: usize) -> String {
        text.chars().take(visible_chars).collect()
    }

    /// 渲染步骤正文。
    ///
    /// 业务意图：
    /// - 普通诊断仍按文本流逐行展示，带内部标记的异常上下文则渲染成截图式日志块，便于用户直接定位原始日志行。
    /// - 内部标记必须在这里被消费，不能泄露到窗口正文。
    fn render_output_step_text(text: String, palette: AppThemePalette) -> gpui::Div {
        let blocks = Self::parse_output_step_text_blocks(&text);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .pl_5()
            .font_family(LOG_VIEWER_FONT_FAMILY)
            .text_xs()
            .line_height(px(19.0))
            .text_color(rgb(palette.text))
            .children(
                blocks
                    .into_iter()
                    .enumerate()
                    .map(|(block_index, block)| match block {
                        PluginOutputStepTextBlock::Text(lines) => {
                            Self::render_output_step_plain_text(lines, palette)
                        }
                        PluginOutputStepTextBlock::LogSnippet(lines) => {
                            Self::render_output_log_snippet(block_index, lines, palette)
                        }
                    }),
            )
    }

    /// 解析步骤正文中的普通文本和日志截图片段。
    ///
    /// 边界条件：
    /// - 打字机动画可能只显示了半行内部标记；遇到不完整标记时直接隐藏，下一帧完整后再渲染。
    /// - 插件输出异常或第三方插件误用了同名前缀时，宿主只忽略无法解析的标记行，不影响其它普通文本展示。
    fn parse_output_step_text_blocks(text: &str) -> Vec<PluginOutputStepTextBlock> {
        let mut blocks = Vec::new();
        let mut plain_lines = Vec::<String>::new();
        let mut snippet_lines: Option<Vec<PluginOutputLogSnippetLine>> = None;

        for raw_line in text.split('\n') {
            if raw_line == PLUGIN_LOG_SNIPPET_BEGIN {
                Self::push_output_step_plain_text_block(&mut blocks, &mut plain_lines);
                snippet_lines = Some(Vec::new());
                continue;
            }
            if raw_line == PLUGIN_LOG_SNIPPET_END {
                if let Some(lines) = snippet_lines.take() {
                    if !lines.is_empty() {
                        blocks.push(PluginOutputStepTextBlock::LogSnippet(lines));
                    }
                }
                continue;
            }

            if let Some(lines) = snippet_lines.as_mut() {
                if let Some(line) = Self::parse_output_log_snippet_line(raw_line) {
                    lines.push(line);
                }
                continue;
            }

            if raw_line.starts_with("@@LC_") {
                continue;
            }
            plain_lines.push(if raw_line.is_empty() {
                " ".to_string()
            } else {
                raw_line.to_string()
            });
        }

        if let Some(lines) = snippet_lines.take() {
            if !lines.is_empty() {
                blocks.push(PluginOutputStepTextBlock::LogSnippet(lines));
            }
        }
        Self::push_output_step_plain_text_block(&mut blocks, &mut plain_lines);
        blocks
    }

    /// 将累计的普通文本行压入步骤正文块列表。
    fn push_output_step_plain_text_block(
        blocks: &mut Vec<PluginOutputStepTextBlock>,
        plain_lines: &mut Vec<String>,
    ) {
        if !plain_lines.is_empty() {
            blocks.push(PluginOutputStepTextBlock::Text(std::mem::take(plain_lines)));
        }
    }

    /// 解析一行日志截图内部协议。
    fn parse_output_log_snippet_line(raw_line: &str) -> Option<PluginOutputLogSnippetLine> {
        let payload = raw_line.strip_prefix(PLUGIN_LOG_SNIPPET_LINE_PREFIX)?;
        let mut parts = payload.splitn(4, '\t');
        let line_number = parts.next()?.to_string();
        let level = parts.next()?;
        let highlights = parts.next().unwrap_or_default();
        let text = Self::expand_plugin_log_tabs_for_display(parts.next().unwrap_or_default());
        Some(PluginOutputLogSnippetLine {
            line_number,
            text,
            is_issue: level == "issue",
            highlight_terms: highlights
                .split(',')
                .filter(|term| !term.is_empty())
                .map(str::to_string)
                .collect(),
        })
    }

    /// 将插件日志片段中的制表符展开为 4 个空格。
    ///
    /// 业务意图：
    /// - 第三方插件可能直接把 tab 分隔的原始日志放入截图式片段；宿主兜底展开，避免列在 GPUI 中黏连。
    /// - 插件侧也可能自行展开制表符；这里保留宿主防线，兼容未来插件和半截流式输出。
    fn expand_plugin_log_tabs_for_display(text: &str) -> String {
        text.replace('\t', "    ")
    }

    /// 渲染普通步骤文本。
    fn render_output_step_plain_text(lines: Vec<String>, palette: AppThemePalette) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(lines.into_iter().map(|line| {
                let is_log_path = Self::output_step_plain_text_line_is_log_path(&line);
                div()
                    .min_w_0()
                    .whitespace_normal()
                    .text_color(rgb(if is_log_path {
                        palette.muted_text
                    } else {
                        palette.text
                    }))
                    .child(line)
            }))
    }

    /// 判断普通步骤文本行是否是日志路径展示行。
    ///
    /// 业务意图：
    /// - 诊断正文里的路径通常只是来源提示，弱化为灰色可以让异常原因和截图式上下文成为视觉重点。
    /// - 插件可按 `- <path>` 输出扫描到的候选日志列表，因此优先识别这类路径行，避免把普通诊断文本误染成灰色。
    fn output_step_plain_text_line_is_log_path(line: &str) -> bool {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("- ") {
            return false;
        }
        let candidate = trimmed.trim_start_matches("- ").trim();
        let lower = candidate.to_ascii_lowercase();
        candidate.contains('/')
            || candidate.contains('\\')
            || candidate.contains('!')
            || matches!(
                lower.rsplit('.').next(),
                Some("log" | "txt" | "out" | "err" | "zip" | "gz")
            )
    }

    /// 渲染截图式原始日志上下文。
    ///
    /// 业务意图：
    /// - 异常行前后各 2 行需要像日志截图一样展示，行号、等宽字体和异常高亮是定位问题的关键。
    /// - 原始 jstat 列可能很宽，因此整体允许横向滚动，不做截断，避免丢失用于后续分析的指标列。
    fn render_output_log_snippet(
        block_index: usize,
        lines: Vec<PluginOutputLogSnippetLine>,
        palette: AppThemePalette,
    ) -> gpui::Div {
        div()
            .w_full()
            .min_w_0()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.input))
            .overflow_hidden()
            .child(
                div()
                    .id(SharedString::from(format!(
                        "plugin-output-log-snippet-scroll-{block_index}"
                    )))
                    .w_full()
                    .min_w_0()
                    .overflow_x_scroll()
                    .py_1()
                    .children(
                        lines
                            .into_iter()
                            .map(|line| Self::render_output_log_snippet_line(line, palette)),
                    ),
            )
    }

    /// 渲染日志截图中的单行。
    fn render_output_log_snippet_line(
        line: PluginOutputLogSnippetLine,
        palette: AppThemePalette,
    ) -> gpui::Div {
        let highlights =
            Self::output_log_snippet_highlights(&line.text, &line.highlight_terms, palette);
        div()
            .flex()
            .items_start()
            .min_w(px(760.0))
            .when(line.is_issue, |row| row.bg(rgb(palette.search_highlight)))
            .child(
                div()
                    .flex_none()
                    .w(px(64.0))
                    .px_2()
                    .text_right()
                    .text_color(rgb(if line.is_issue {
                        palette.error
                    } else {
                        palette.muted_text
                    }))
                    .child(line.line_number),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .whitespace_nowrap()
                    .text_color(rgb(palette.text))
                    .child(StyledText::new(line.text).with_highlights(highlights)),
            )
    }

    /// 计算日志截图行内高亮范围。
    ///
    /// 边界条件：
    /// - 高亮词由插件解析出的 ASCII 数字组成，`find` 返回的字节位置天然位于 UTF-8 边界。
    /// - 同一个数字可能在行内出现多次，全部高亮；重叠范围会被跳过，避免 GPUI 高亮列表互相覆盖。
    fn output_log_snippet_highlights(
        text: &str,
        highlight_terms: &[String],
        palette: AppThemePalette,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
        let style = HighlightStyle {
            color: Some(rgb(palette.error).into()),
            background_color: Some(rgb(palette.search_highlight).into()),
            font_weight: Some(FontWeight::SEMIBOLD),
            ..Default::default()
        };
        let mut terms = highlight_terms
            .iter()
            .filter(|term| !term.is_empty())
            .collect::<Vec<_>>();
        terms.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
        terms.dedup();
        for term in terms {
            let mut search_start = 0usize;
            while search_start < text.len() {
                let Some(offset) = text[search_start..].find(term.as_str()) else {
                    break;
                };
                let start = search_start + offset;
                let end = start + term.len();
                if !highlights
                    .iter()
                    .any(|(range, _)| start < range.end && end > range.start)
                {
                    highlights.push((start..end, style.clone()));
                }
                search_start = end;
            }
        }
        highlights.sort_by(|left, right| left.0.start.cmp(&right.0.start));
        highlights
    }

    /// 返回步骤状态对应的图标、颜色和标题。
    fn output_step_status_visual(
        step: &PluginOutputStep,
        spinner: &'static str,
        palette: AppThemePalette,
    ) -> (String, bool, u32, String) {
        match step.status {
            PluginOutputStepStatus::Running => (
                spinner.to_string(),
                false,
                palette.accent,
                step.loading_text.clone(),
            ),
            PluginOutputStepStatus::Completed => (
                char::from(Icon::Check).to_string(),
                true,
                palette.accent,
                step.done_text
                    .clone()
                    .unwrap_or_else(|| step.loading_text.clone()),
            ),
            PluginOutputStepStatus::Failed => (
                char::from(Icon::AlertCircle).to_string(),
                true,
                palette.error,
                step.done_text
                    .clone()
                    .unwrap_or_else(|| step.loading_text.clone()),
            ),
        }
    }

    /// 返回加载动画的字符帧。
    fn output_step_spinner_frame() -> &'static str {
        const FRAMES: [&str; 4] = ["|", "/", "-", "\\"];
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        FRAMES[((millis / 120) % FRAMES.len() as u128) as usize]
    }

    /// 返回瀑布流输出等级对应的图标和颜色。
    fn plugin_output_line_visual(
        level: PluginOutputLevel,
        palette: AppThemePalette,
    ) -> (Icon, u32) {
        match level {
            PluginOutputLevel::Info => (Icon::Info, palette.text),
            PluginOutputLevel::Success => (Icon::ChartNoAxesCombined, palette.accent),
            PluginOutputLevel::Warning => (Icon::AlertCircle, 0xd97706),
            PluginOutputLevel::Error => (Icon::AlertCircle, palette.error),
            PluginOutputLevel::Muted => (Icon::Info, palette.muted_text),
        }
    }

    /// 渲染插件表格过滤栏。
    ///
    /// 业务意图：
    /// - 插件协议只提供最终表格数据；宿主过滤栏在本地对任意插件表格做快速收窄，避免用户为了找一条记录重新执行插件。
    /// - 过滤栏必须消费鼠标和键盘事件，防止输入、拖选或清空按钮点击穿透到表格行按钮。
    fn render_table_filter_bar(
        &self,
        table: &PluginPageTable,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let total_count = table.rows.len();
        let filtered_count = self.table_row_order.len().min(total_count);
        let has_filter = !self.table_filter_input.text.trim().is_empty();
        div()
            .id("plugin-table-filter-bar")
            .flex()
            .flex_col()
            .gap_2()
            .flex_none()
            .when_some(table.command_filter.as_ref(), |bar, filter| {
                bar.child(self.render_command_filter_row(filter, palette, context))
            })
            .child(
                div()
                    .id("plugin-table-quick-filter-row")
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(PLUGIN_TABLE_FILTER_HEIGHT))
                    .child(self.render_table_input_box(
                        SharedString::from("plugin-table-filter-input"),
                        PluginTableInputKind::QuickFilter,
                        "过滤当前表格任意关键字".to_string(),
                        PluginTableCommandFilterControlKind::Text,
                        Some(Icon::Search),
                        true,
                        px(0.0),
                        palette,
                        context,
                    ))
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .child(if has_filter {
                                format!("{filtered_count} / {total_count} 行")
                            } else {
                                format!("{total_count} 行")
                            }),
                    ),
            )
    }

    /// 渲染插件声明式命令过滤行。
    ///
    /// 业务意图：
    /// - 插件声明的过滤控件会回调插件重新生成表格，不能混在通用本地关键字过滤里。
    /// - 输入控件保持紧凑并允许换行，避免较窄窗口下时间输入和按钮互相挤压遮挡。
    fn render_command_filter_row(
        &self,
        filter: &PluginTableCommandFilter,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let apply_command = filter.command.clone();
        let reset_command = filter.command.clone();
        div()
            .id("plugin-command-filter-row")
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .min_h(px(PLUGIN_TABLE_FILTER_HEIGHT))
            .children(
                self.command_filter_inputs
                    .iter()
                    .enumerate()
                    .map(|(index, input_state)| {
                        self.render_table_input_box(
                            SharedString::from(format!("plugin-command-filter-input-{index}")),
                            PluginTableInputKind::CommandFilter(index),
                            input_state.placeholder.clone(),
                            input_state.control_kind,
                            Self::plugin_command_filter_icon(input_state.icon.as_deref()),
                            false,
                            input_state.width,
                            palette,
                            context,
                        )
                    }),
            )
            .child(
                div()
                    .id("plugin-command-filter-apply")
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(PLUGIN_TABLE_FILTER_HEIGHT))
                    .px_3()
                    .rounded(px(7.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.panel))
                    .text_sm()
                    .text_color(rgb(palette.text))
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
                    .child(MainView::render_lucide_icon(
                        Some(Icon::ListFilter),
                        14.0,
                        14.0,
                        palette.text,
                    ))
                    .child("应用")
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                            view.apply_plugin_table_command_filter(apply_command.clone(), context);
                            context.stop_propagation();
                        }),
                    ),
            )
            .child(
                div()
                    .id("plugin-command-filter-reset")
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(PLUGIN_TABLE_FILTER_HEIGHT))
                    .px_3()
                    .rounded(px(7.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.background))
                    .text_sm()
                    .text_color(rgb(palette.muted_text))
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
                    .child(MainView::render_lucide_icon(
                        Some(Icon::RefreshCw),
                        14.0,
                        14.0,
                        palette.muted_text,
                    ))
                    .child("重置")
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                            view.clear_command_filter_inputs();
                            view.apply_plugin_table_command_filter(reset_command.clone(), context);
                            context.stop_propagation();
                        }),
                    ),
            )
    }

    /// 将插件声明的通用图标名映射到宿主图标。
    ///
    /// 业务意图：
    /// - 插件只能传递字符串协议，宿主统一提供有限的通用图标集合；未知图标名忽略，不影响控件可用性。
    fn plugin_command_filter_icon(icon: Option<&str>) -> Option<Icon> {
        match icon
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "user" => Some(Icon::User),
            "search" => Some(Icon::Search),
            "filter" => Some(Icon::ListFilter),
            "calendar" | "time" => Some(Icon::Calendar),
            _ => None,
        }
    }

    /// 渲染插件表格过滤栏中的单行输入框。
    fn render_table_input_box(
        &self,
        element_id: SharedString,
        input_kind: PluginTableInputKind,
        placeholder: String,
        control_kind: PluginTableCommandFilterControlKind,
        icon: Option<Icon>,
        fill_available_width: bool,
        fixed_width: Pixels,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let has_text = self
            .table_input_state(input_kind)
            .is_some_and(|input| !input.text.trim().is_empty());
        let focus_handle = self
            .table_input_focus(input_kind)
            .cloned()
            .unwrap_or_else(|| context.focus_handle());
        let date_time_control_index = match (control_kind, input_kind) {
            (
                PluginTableCommandFilterControlKind::DateTime,
                PluginTableInputKind::CommandFilter(index),
            ) => Some(index),
            _ => None,
        };
        let date_time_picker_open = date_time_control_index.is_some_and(|index| {
            self.command_filter_inputs
                .get(index)
                .is_some_and(|state| state.date_time_picker_open)
        });
        let (render_input_icon, date_time_toggle_icon) = match (date_time_control_index, icon) {
            (Some(_), Some(icon)) => (None, icon),
            (Some(_), None) => (None, Icon::Calendar),
            (None, icon) => (icon, Icon::Calendar),
        };
        div()
            .id(element_id.clone())
            .relative()
            .flex()
            .items_center()
            .gap_2()
            .h_full()
            .min_w_0()
            .px_2()
            .rounded(px(7.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.input))
            .track_focus(&focus_handle)
            .key_context("plugin-table-filter-input")
            .when(fill_available_width, |input| input.flex_1())
            .when(!fill_available_width, |input| {
                input.flex_none().w(fixed_width)
            })
            .on_key_down(context.listener(move |view, event, window, context| {
                view.handle_table_input_key_down(input_kind, event, window, context);
            }))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, event: &MouseDownEvent, window, context| {
                    view.start_table_input_mouse_selection(input_kind, event, window, context);
                    context.stop_propagation();
                }),
            )
            .on_mouse_move(context.listener(
                move |view, event: &MouseMoveEvent, _window, context| {
                    if event.dragging() {
                        view.update_table_input_mouse_selection(
                            input_kind,
                            event.position,
                            context,
                        );
                        context.stop_propagation();
                    }
                },
            ))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseUpEvent, _window, context| {
                    view.finish_table_input_mouse_selection(input_kind, context);
                    context.stop_propagation();
                }),
            )
            .when_some(render_input_icon, |input, icon| {
                input.child(MainView::render_lucide_icon(
                    Some(icon),
                    15.0,
                    15.0,
                    palette.muted_text,
                ))
            })
            .when_some(date_time_control_index, |input, index| {
                input.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(22.0))
                        .h(px(22.0))
                        .rounded(px(5.0))
                        .text_color(rgb(palette.muted_text))
                        .cursor_pointer()
                        .hover(move |button| button.bg(rgb(palette.hover)))
                        .child(MainView::render_lucide_icon(
                            Some(date_time_toggle_icon),
                            14.0,
                            14.0,
                            if date_time_picker_open {
                                palette.accent
                            } else {
                                palette.muted_text
                            },
                        ))
                        .on_mouse_down(
                            MouseButton::Left,
                            context.listener(
                                move |view, _event: &MouseDownEvent, _window, context| {
                                    view.toggle_command_filter_date_time_picker(index, context);
                                    context.stop_propagation();
                                },
                            ),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .line_height(px(20.0))
                    .text_size(px(13.0))
                    .text_color(rgb(palette.text))
                    .child(PluginTableFilterInputElement {
                        view: context.entity(),
                        input_kind,
                        focus_handle,
                        placeholder,
                        palette,
                    }),
            )
            .when(has_text, |input| {
                input.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(22.0))
                        .h(px(22.0))
                        .rounded(px(5.0))
                        .text_color(rgb(palette.muted_text))
                        .cursor_pointer()
                        .hover(move |button| button.bg(rgb(palette.hover)))
                        .child(MainView::render_lucide_icon(
                            Some(Icon::X),
                            14.0,
                            14.0,
                            palette.muted_text,
                        ))
                        .on_mouse_down(
                            MouseButton::Left,
                            context.listener(
                                move |view, _event: &MouseDownEvent, _window, context| {
                                    if let Some(input) = view.table_input_state_mut(input_kind) {
                                        input.set_text(String::new());
                                    }
                                    if let PluginTableInputKind::CommandFilter(index) = input_kind {
                                        view.close_command_filter_date_time_picker(index);
                                    }
                                    view.after_table_input_text_changed(input_kind, context);
                                    context.stop_propagation();
                                },
                            ),
                        ),
                )
            })
            .when(date_time_picker_open, |input| {
                let Some(index) = date_time_control_index else {
                    return input;
                };
                input.child({
                    gpui::deferred(
                        self.render_command_filter_date_time_picker(index, palette, context),
                    )
                    .with_priority(48)
                })
            })
    }

    /// 切换插件命令过滤器中的日期时间弹层。
    ///
    /// 业务意图：
    /// - 日期时间控件由插件声明，宿主只维护弹层打开状态和当前可见月份。
    /// - 打开新弹层时关闭其它命令过滤控件的弹层，避免多个浮层互相遮挡和鼠标命中不确定。
    fn toggle_command_filter_date_time_picker(
        &mut self,
        index: usize,
        context: &mut Context<Self>,
    ) {
        let Some(current) = self.command_filter_inputs.get(index) else {
            return;
        };
        if current.control_kind != PluginTableCommandFilterControlKind::DateTime {
            return;
        }

        let will_open = !current.date_time_picker_open;
        for input_state in &mut self.command_filter_inputs {
            input_state.date_time_picker_open = false;
        }
        if let Some(input_state) = self.command_filter_inputs.get_mut(index) {
            input_state.date_time_picker_open = will_open;
            if will_open {
                let view_month = date_time_picker_month_from_text(&input_state.input.text);
                input_state.date_time_picker_view_year = view_month.year;
                input_state.date_time_picker_view_month = view_month.month;
            }
        }
        context.notify();
    }

    /// 关闭指定日期时间弹层。
    fn close_command_filter_date_time_picker(&mut self, index: usize) {
        if let Some(input_state) = self.command_filter_inputs.get_mut(index) {
            input_state.date_time_picker_open = false;
        }
    }

    /// 关闭全部日期时间弹层。
    fn close_all_command_filter_date_time_pickers(&mut self) {
        for input_state in &mut self.command_filter_inputs {
            input_state.date_time_picker_open = false;
        }
    }

    /// 输入焦点切换时收起不相关的日期时间弹层。
    ///
    /// 业务意图：
    /// - 日期时间弹层覆盖在表格上方，用户点击其它输入框时应自然离开当前选择器，避免旧弹层继续遮挡后续操作。
    /// - 点击当前已打开的日期时间输入文本时保留弹层，方便用户边手动编辑边参考日历。
    fn close_command_filter_date_time_pickers_for_input_focus(
        &mut self,
        input_kind: PluginTableInputKind,
    ) {
        let keep_open_index = match input_kind {
            PluginTableInputKind::QuickFilter => None,
            PluginTableInputKind::CommandFilter(index)
                if self.command_filter_inputs.get(index).is_some_and(|state| {
                    state.control_kind == PluginTableCommandFilterControlKind::DateTime
                }) =>
            {
                Some(index)
            }
            PluginTableInputKind::CommandFilter(_) => None,
        };
        for (index, input_state) in self.command_filter_inputs.iter_mut().enumerate() {
            if Some(index) != keep_open_index {
                input_state.date_time_picker_open = false;
            }
        }
    }

    /// 移动日期时间弹层当前月份。
    fn move_command_filter_date_time_picker_month(
        &mut self,
        index: usize,
        delta: i32,
        context: &mut Context<Self>,
    ) {
        if let Some(input_state) = self.command_filter_inputs.get_mut(index) {
            let next = date_time_picker_move_month(
                DateTimePickerViewMonth {
                    year: input_state.date_time_picker_view_year,
                    month: input_state.date_time_picker_view_month,
                },
                delta,
            );
            input_state.date_time_picker_view_year = next.year;
            input_state.date_time_picker_view_month = next.month;
            context.notify();
        }
    }

    /// 选中日期并把标准日期时间文本写回输入框。
    fn select_command_filter_date_time_picker_date(
        &mut self,
        index: usize,
        date: chrono::NaiveDate,
        context: &mut Context<Self>,
    ) {
        let current_text = self
            .command_filter_inputs
            .get(index)
            .map(|state| state.input.text.clone())
            .unwrap_or_default();
        let value = date_time_picker_value_for_selected_date(&current_text, date);
        self.set_command_filter_date_time_value(index, value, context);
    }

    /// 调整日期时间输入框中的时间部分。
    fn adjust_command_filter_date_time_picker_time(
        &mut self,
        index: usize,
        part: DateTimePart,
        delta: i32,
        context: &mut Context<Self>,
    ) {
        let current_text = self
            .command_filter_inputs
            .get(index)
            .map(|state| state.input.text.clone())
            .unwrap_or_default();
        let value = date_time_picker_adjust_time(&current_text, part, delta);
        self.set_command_filter_date_time_value(index, value, context);
    }

    /// 使用当前本地时间填充日期时间输入框。
    fn set_command_filter_date_time_now(&mut self, index: usize, context: &mut Context<Self>) {
        self.set_command_filter_date_time_value(index, app_date_time_now(), context);
    }

    /// 设置日期时间输入框文本。
    ///
    /// 边界条件：
    /// - 写回只更新当前窗口的输入状态，不自动执行插件命令；用户仍通过“应用”或 Enter 触发重算。
    fn set_command_filter_date_time_value(
        &mut self,
        index: usize,
        value: chrono::NaiveDateTime,
        context: &mut Context<Self>,
    ) {
        if let Some(input_state) = self.command_filter_inputs.get_mut(index) {
            input_state.input.set_text(format_app_date_time(value));
            input_state.layout = None;
            input_state.date_time_picker_view_year = value.date().year();
            input_state.date_time_picker_view_month = value.date().month();
            context.notify();
        }
    }

    /// 渲染插件命令过滤器的日期时间弹层。
    ///
    /// 业务意图：
    /// - 弹层复制 xgpui DateTimePicker 的核心交互：月历选择日期、时间部分可步进调整、写回秒级标准格式。
    /// - 该弹层是覆盖在表格上的浮层，必须消费鼠标事件，避免点击日期时同时触发表格行或按钮。
    fn render_command_filter_date_time_picker(
        &self,
        index: usize,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(input_state) = self.command_filter_inputs.get(index) else {
            return div().id("plugin-command-date-time-picker-missing").hidden();
        };
        let view_month = DateTimePickerViewMonth {
            year: input_state.date_time_picker_view_year,
            month: input_state.date_time_picker_view_month,
        };
        let input_text = input_state.input.text.clone();
        let selected_date = parse_app_date_time(&input_text).map(|value| value.date());
        let display_value = parse_app_date_time(&input_text)
            .unwrap_or_else(|| date_time_picker_adjust_time("", DateTimePart::Second, 0));
        let today_date = app_date_time_now().date();
        let weekday_labels = ["一", "二", "三", "四", "五", "六", "日"];

        div()
            .id(SharedString::from(format!(
                "plugin-command-date-time-picker-{index}"
            )))
            .absolute()
            .left(px(0.0))
            .top(px(PLUGIN_TABLE_FILTER_HEIGHT + 6.0))
            .w(px(320.0))
            .p_3()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.menu))
            .shadow_md()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                |_event: &MouseDownEvent, _window: &mut Window, context: &mut App| {
                    context.stop_propagation();
                },
            )
            .on_mouse_down(
                MouseButton::Right,
                |_event: &MouseDownEvent, _window: &mut Window, context: &mut App| {
                    context.stop_propagation();
                },
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mb_2()
                    .child(self.render_date_time_picker_icon_button(
                        index,
                        "prev-month",
                        Icon::ChevronLeft,
                        palette,
                        context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                            view.move_command_filter_date_time_picker_month(index, -1, context);
                            context.stop_propagation();
                        }),
                    ))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .child(format!("{}年{:02}月", view_month.year, view_month.month)),
                    )
                    .child(self.render_date_time_picker_icon_button(
                        index,
                        "next-month",
                        Icon::ChevronRight,
                        palette,
                        context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                            view.move_command_filter_date_time_picker_month(index, 1, context);
                            context.stop_propagation();
                        }),
                    )),
            )
            .child(div().flex().items_center().gap_1().mb_1().children(
                weekday_labels.into_iter().map(|label| {
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(38.0))
                        .h(px(24.0))
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(palette.muted_text))
                        .child(label)
                }),
            ))
            .child(
                div().flex().flex_wrap().gap_1().children(
                    date_time_picker_calendar_days(view_month.year, view_month.month)
                        .into_iter()
                        .map(|day| {
                            let in_month = day.month() == view_month.month;
                            let selected = selected_date == Some(day);
                            let today = today_date == day;
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .w(px(38.0))
                                .h(px(30.0))
                                .rounded(px(6.0))
                                .text_sm()
                                .cursor_pointer()
                                .text_color(rgb(if selected {
                                    palette.on_accent
                                } else if in_month {
                                    palette.text
                                } else {
                                    palette.muted_text
                                }))
                                .when(selected, |cell| cell.bg(rgb(palette.accent)))
                                .when(!selected && today, |cell| cell.bg(rgb(palette.selected)))
                                .hover(move |cell| {
                                    if selected {
                                        cell.bg(rgb(palette.accent_hover))
                                    } else {
                                        cell.bg(rgb(palette.hover))
                                    }
                                })
                                .child(day.day().to_string())
                                .on_mouse_down(
                                    MouseButton::Left,
                                    context.listener(
                                        move |view, _event: &MouseDownEvent, _window, context| {
                                            view.select_command_filter_date_time_picker_date(
                                                index, day, context,
                                            );
                                            context.stop_propagation();
                                        },
                                    ),
                                )
                        }),
                ),
            )
            .child(
                div()
                    .mt_3()
                    .pt_2()
                    .border_t_1()
                    .border_color(rgb(palette.border))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(self.render_date_time_picker_time_stepper(
                        index,
                        "时",
                        display_value.time().hour(),
                        DateTimePart::Hour,
                        palette,
                        context,
                    ))
                    .child(self.render_date_time_picker_time_stepper(
                        index,
                        "分",
                        display_value.time().minute(),
                        DateTimePart::Minute,
                        palette,
                        context,
                    ))
                    .child(self.render_date_time_picker_time_stepper(
                        index,
                        "秒",
                        display_value.time().second(),
                        DateTimePart::Second,
                        palette,
                        context,
                    )),
            )
            .child(
                div()
                    .mt_3()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(self.render_date_time_picker_text_button(
                        "清空",
                        false,
                        palette,
                        context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                            if let Some(input_state) = view.command_filter_inputs.get_mut(index) {
                                input_state.input.set_text(String::new());
                                input_state.layout = None;
                            }
                            view.close_command_filter_date_time_picker(index);
                            context.notify();
                            context.stop_propagation();
                        }),
                    ))
                    .child(self.render_date_time_picker_text_button(
                        "现在",
                        false,
                        palette,
                        context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                            view.set_command_filter_date_time_now(index, context);
                            context.stop_propagation();
                        }),
                    ))
                    .child(self.render_date_time_picker_text_button(
                        "确定",
                        true,
                        palette,
                        context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                            view.close_command_filter_date_time_picker(index);
                            context.notify();
                            context.stop_propagation();
                        }),
                    )),
            )
    }

    /// 渲染日期时间弹层的图标按钮。
    fn render_date_time_picker_icon_button(
        &self,
        index: usize,
        id_suffix: &'static str,
        icon: Icon,
        palette: AppThemePalette,
        on_mouse_down: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "plugin-date-time-picker-{index}-icon-{id_suffix}"
            )))
            .flex()
            .items_center()
            .justify_center()
            .w(px(28.0))
            .h(px(28.0))
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(MainView::render_lucide_icon(
                Some(icon),
                15.0,
                15.0,
                palette.muted_text,
            ))
            .on_mouse_down(MouseButton::Left, on_mouse_down)
    }

    /// 渲染日期时间弹层的时间步进控件。
    fn render_date_time_picker_time_stepper(
        &self,
        index: usize,
        label: &'static str,
        value: u32,
        part: DateTimePart,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "plugin-date-time-picker-{index}-{label}"
            )))
            .flex()
            .items_center()
            .gap_1()
            .child(self.render_date_time_picker_small_button(
                index,
                format!("{label}-decrease"),
                Icon::Minus,
                palette,
                context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                    view.adjust_command_filter_date_time_picker_time(index, part, -1, context);
                    context.stop_propagation();
                }),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .min_w(px(46.0))
                    .h(px(28.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .text_sm()
                    .text_color(rgb(palette.text))
                    .child(format!("{value:02} {label}")),
            )
            .child(self.render_date_time_picker_small_button(
                index,
                format!("{label}-increase"),
                Icon::Plus,
                palette,
                context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                    view.adjust_command_filter_date_time_picker_time(index, part, 1, context);
                    context.stop_propagation();
                }),
            ))
    }

    /// 渲染时间步进的小图标按钮。
    fn render_date_time_picker_small_button(
        &self,
        index: usize,
        id_suffix: String,
        icon: Icon,
        palette: AppThemePalette,
        on_mouse_down: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "plugin-date-time-picker-{index}-{id_suffix}"
            )))
            .flex()
            .items_center()
            .justify_center()
            .w(px(24.0))
            .h(px(24.0))
            .rounded(px(5.0))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(MainView::render_lucide_icon(
                Some(icon),
                13.0,
                13.0,
                palette.muted_text,
            ))
            .on_mouse_down(MouseButton::Left, on_mouse_down)
    }

    /// 渲染日期时间弹层底部文本按钮。
    fn render_date_time_picker_text_button(
        &self,
        label: &'static str,
        primary: bool,
        palette: AppThemePalette,
        on_mouse_down: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(SharedString::from(format!(
                "plugin-date-time-picker-button-{label}"
            )))
            .flex()
            .items_center()
            .justify_center()
            .h(px(28.0))
            .px_3()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(if primary {
                palette.accent
            } else {
                palette.border
            }))
            .bg(rgb(if primary {
                palette.accent
            } else {
                palette.surface
            }))
            .text_sm()
            .text_color(rgb(if primary {
                palette.on_accent
            } else {
                palette.text
            }))
            .cursor_pointer()
            .hover(move |button| {
                button.bg(rgb(if primary {
                    palette.accent_hover
                } else {
                    palette.hover
                }))
            })
            .child(label)
            .on_mouse_down(MouseButton::Left, on_mouse_down)
    }

    /// 渲染插件表格纵向滚动条。
    ///
    /// 业务意图：
    /// - 插件结果可能有上万行，用户需要可见滑块确认当前位置，并能直接拖动到目标区域。
    /// - 滑块和 `uniform_list` 共享同一个滚动句柄，不复制行数据，也不让大表格退化为全量渲染。
    fn render_plugin_table_vertical_scrollbar(
        &self,
        scroll_handle: &UniformListScrollHandle,
        row_count: usize,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) = Self::plugin_table_vertical_scrollbar_metrics(scroll_handle)
            .or_else(|| Self::fallback_plugin_table_vertical_scrollbar_metrics(row_count))
        else {
            return div().id("plugin-table-vertical-scrollbar-empty").hidden();
        };

        div()
            .id("plugin-table-vertical-scrollbar")
            .absolute()
            .top(metrics.thumb_start)
            .right(px(PLUGIN_TABLE_SCROLLBAR_PADDING))
            .w(px(PLUGIN_TABLE_SCROLLBAR_WIDTH))
            .h(metrics.thumb_length)
            .rounded(px(PLUGIN_TABLE_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(palette.scrollbar))
            .cursor_pointer()
            .hover(move |thumb| thumb.bg(rgb(palette.scrollbar_hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.start_plugin_table_scrollbar_drag(
                        LogScrollbarAxis::Vertical,
                        event,
                        context,
                    );
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染插件表格横向滚动条。
    ///
    /// 业务意图：
    /// - 插件任意文本列都可能超过窗口宽度；横向滑块让用户不用依赖触控板即可查看完整内容。
    /// - 只有真实产生横向溢出时才显示，避免普通短表右下角出现无效控件。
    fn render_plugin_table_horizontal_scrollbar(
        &self,
        scroll_handle: &ScrollHandle,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) = Self::plugin_table_horizontal_scrollbar_metrics(scroll_handle) else {
            return div().id("plugin-table-horizontal-scrollbar-empty").hidden();
        };

        div()
            .id("plugin-table-horizontal-scrollbar")
            .absolute()
            .left(metrics.thumb_start)
            .bottom(px(PLUGIN_TABLE_SCROLLBAR_PADDING))
            .w(metrics.thumb_length)
            .h(px(PLUGIN_TABLE_SCROLLBAR_WIDTH))
            .rounded(px(PLUGIN_TABLE_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(palette.scrollbar))
            .cursor_pointer()
            .hover(move |thumb| thumb.bg(rgb(palette.scrollbar_hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.start_plugin_table_scrollbar_drag(
                        LogScrollbarAxis::Horizontal,
                        event,
                        context,
                    );
                    context.stop_propagation();
                }),
            )
    }

    /// 计算插件表格纵向滚动条滑块。
    ///
    /// 边界条件：
    /// - 使用 `uniform_list` 的真实测量结果；首帧尚无测量时由 fallback 提供只读视觉提示。
    /// - 内容高度未超过视口时不显示滚动条，避免短表格出现不能拖动的滑块。
    fn plugin_table_vertical_scrollbar_metrics(
        scroll_handle: &UniformListScrollHandle,
    ) -> Option<LogScrollbarMetrics> {
        let state = scroll_handle.0.borrow();
        let size = state.last_item_size?;
        let viewport_height = size.item.height;
        let content_height = size.contents.height;
        if viewport_height <= px(0.0) || content_height <= viewport_height {
            return None;
        }

        let max_scroll = content_height - viewport_height;
        let scroll_top = (-state.base_handle.offset().y).clamp(px(0.0), max_scroll);
        let track_start = px(PLUGIN_TABLE_SCROLLBAR_PADDING);
        let track_length = (viewport_height - track_start * 2.0).max(px(1.0));
        let min_thumb_length = px(PLUGIN_TABLE_SCROLLBAR_MIN_THUMB_LENGTH).min(track_length);
        let thumb_length = (viewport_height * (viewport_height / content_height))
            .clamp(min_thumb_length, track_length);
        let movable_length = (track_length - thumb_length).max(px(0.0));
        let thumb_start = track_start + movable_length * (scroll_top / max_scroll);

        Some(LogScrollbarMetrics {
            thumb_start,
            thumb_length,
            track_start,
            track_length,
            max_scroll,
            max_scroll_px: f64::from(max_scroll),
        })
    }

    /// 插件表格首帧纵向滚动条兜底提示。
    ///
    /// 业务意图：
    /// - 大表格刚打开时，虚拟列表需要一帧后才有内容高度；临时滑块能立即提示“这里可以滚动”。
    /// - 该兜底没有真实最大滚动距离，因此只用于展示，不参与拖动换算。
    fn fallback_plugin_table_vertical_scrollbar_metrics(
        row_count: usize,
    ) -> Option<LogScrollbarMetrics> {
        if row_count <= 12 {
            return None;
        }

        Some(LogScrollbarMetrics {
            thumb_start: px(PLUGIN_TABLE_SCROLLBAR_PADDING),
            thumb_length: px(PLUGIN_TABLE_SCROLLBAR_MIN_THUMB_LENGTH),
            track_start: px(PLUGIN_TABLE_SCROLLBAR_PADDING),
            track_length: px(PLUGIN_TABLE_SCROLLBAR_MIN_THUMB_LENGTH),
            max_scroll: px(0.0),
            max_scroll_px: 0.0,
        })
    }

    /// 计算插件表格横向滚动条滑块。
    fn plugin_table_horizontal_scrollbar_metrics(
        scroll_handle: &ScrollHandle,
    ) -> Option<LogScrollbarMetrics> {
        let viewport_width = scroll_handle.bounds().size.width;
        let max_scroll = scroll_handle.max_offset().width;
        if viewport_width <= px(0.0) || max_scroll <= px(0.0) {
            return None;
        }

        let content_width = viewport_width + max_scroll;
        let scroll_left = (-scroll_handle.offset().x).clamp(px(0.0), max_scroll);
        let track_start = px(PLUGIN_TABLE_SCROLLBAR_PADDING);
        let track_right_padding =
            px(PLUGIN_TABLE_SCROLLBAR_WIDTH + PLUGIN_TABLE_SCROLLBAR_PADDING * 2.0);
        let track_length = (viewport_width - track_start - track_right_padding).max(px(1.0));
        let min_thumb_length = px(PLUGIN_TABLE_SCROLLBAR_MIN_THUMB_LENGTH).min(track_length);
        let thumb_length =
            (track_length * (viewport_width / content_width)).clamp(min_thumb_length, track_length);
        let movable_length = (track_length - thumb_length).max(px(0.0));
        let thumb_start = track_start + movable_length * (scroll_left / max_scroll);

        Some(LogScrollbarMetrics {
            thumb_start,
            thumb_length,
            track_start,
            track_length,
            max_scroll,
            max_scroll_px: f64::from(max_scroll),
        })
    }

    /// 开始拖动插件表格滚动条。
    ///
    /// 边界条件：
    /// - 如果当前内容没有溢出或布局尚未完成，则忽略按下事件，避免生成一个无法更新的拖动状态。
    /// - 滚动条拖动会结束文本拖选，防止同一轮鼠标事件同时改变滚动和文本选区。
    fn start_plugin_table_scrollbar_drag(
        &mut self,
        axis: LogScrollbarAxis,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let Some(metrics) = self.plugin_table_scrollbar_metrics(axis) else {
            return;
        };
        if metrics.max_scroll <= px(0.0) {
            return;
        }
        let Some(viewport_origin) = self.plugin_table_scrollbar_viewport_axis_origin(axis) else {
            return;
        };

        let pointer_position = match axis {
            LogScrollbarAxis::Vertical => event.position.y,
            LogScrollbarAxis::Horizontal => event.position.x,
        };
        self.table_scrollbar_drag = Some(PluginTableScrollbarDrag {
            axis,
            cursor_offset: pointer_position - viewport_origin - metrics.thumb_start,
        });
        self.finish_table_cell_text_selection(context);
        context.notify();
    }

    /// 根据鼠标移动更新插件表格滚动条拖动。
    fn update_plugin_table_scrollbar_drag(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.table_scrollbar_drag else {
            return;
        };
        if !event.dragging() {
            self.table_scrollbar_drag = None;
            context.notify();
            return;
        }
        let Some(metrics) = self.plugin_table_scrollbar_metrics(drag.axis) else {
            self.table_scrollbar_drag = None;
            context.notify();
            return;
        };
        let Some(viewport_origin) = self.plugin_table_scrollbar_viewport_axis_origin(drag.axis)
        else {
            self.table_scrollbar_drag = None;
            context.notify();
            return;
        };

        let movable_length = (metrics.track_length - metrics.thumb_length).max(px(0.0));
        if metrics.max_scroll <= px(0.0) || movable_length <= px(0.0) {
            return;
        }
        let pointer_position = match drag.axis {
            LogScrollbarAxis::Vertical => event.position.y,
            LogScrollbarAxis::Horizontal => event.position.x,
        };
        let requested_thumb_start = pointer_position - viewport_origin - drag.cursor_offset;
        let thumb_start =
            requested_thumb_start.clamp(metrics.track_start, metrics.track_start + movable_length);
        let scroll_ratio = f64::from((thumb_start - metrics.track_start) / movable_length);
        let scroll_offset = px((metrics.max_scroll_px * scroll_ratio) as f32);

        match drag.axis {
            LogScrollbarAxis::Vertical => {
                let base_scroll_handle = {
                    // 克隆底层句柄后立即释放 RefCell 借用，避免 set_offset 时触发可变借用冲突。
                    self.table_scroll_handle.0.borrow().base_handle.clone()
                };
                let current_offset = base_scroll_handle.offset();
                base_scroll_handle.set_offset(point(current_offset.x, -scroll_offset));
            }
            LogScrollbarAxis::Horizontal => {
                let current_offset = self.table_x_scroll_handle.offset();
                self.table_x_scroll_handle
                    .set_offset(point(-scroll_offset, current_offset.y));
            }
        }
        context.notify();
    }

    /// 结束插件表格滚动条拖动。
    fn finish_plugin_table_scrollbar_drag(&mut self, context: &mut Context<Self>) {
        if self.table_scrollbar_drag.is_some() {
            self.table_scrollbar_drag = None;
            context.notify();
        }
    }

    /// 返回当前轴向的插件表格滚动条测量。
    fn plugin_table_scrollbar_metrics(
        &self,
        axis: LogScrollbarAxis,
    ) -> Option<LogScrollbarMetrics> {
        match axis {
            LogScrollbarAxis::Vertical => {
                Self::plugin_table_vertical_scrollbar_metrics(&self.table_scroll_handle)
            }
            LogScrollbarAxis::Horizontal => {
                Self::plugin_table_horizontal_scrollbar_metrics(&self.table_x_scroll_handle)
            }
        }
    }

    /// 返回当前轴向视口在窗口坐标中的起点。
    fn plugin_table_scrollbar_viewport_axis_origin(
        &self,
        axis: LogScrollbarAxis,
    ) -> Option<Pixels> {
        match axis {
            LogScrollbarAxis::Vertical => {
                let bounds = self.table_scroll_handle.0.borrow().base_handle.bounds();
                (bounds.size.height > px(0.0)).then_some(bounds.top())
            }
            LogScrollbarAxis::Horizontal => {
                let bounds = self.table_x_scroll_handle.bounds();
                (bounds.size.width > px(0.0)).then_some(bounds.left())
            }
        }
    }

    /// 处理插件窗口鼠标移动。
    ///
    /// 业务意图：
    /// - 插件窗口内的文本拖选和滚动条拖动都属于覆盖层交互，必须在根节点统一收束，避免事件穿透到表格行按钮。
    fn handle_plugin_window_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let mut handled = false;
        if let Some(input_kind) = self.dragging_table_input_kind() {
            if event.dragging() {
                self.update_table_input_mouse_selection(input_kind, event.position, context);
            } else {
                self.finish_table_input_mouse_selection(input_kind, context);
            }
            handled = true;
        }
        if self.table_scrollbar_drag.is_some() {
            self.update_plugin_table_scrollbar_drag(event, context);
            handled = true;
        }
        if self
            .table_text_selection
            .as_ref()
            .is_some_and(|selection| selection.dragging)
        {
            if event.dragging() {
                self.update_table_cell_text_selection(event.position, window, context);
            } else {
                self.finish_table_cell_text_selection(context);
            }
            handled = true;
        }
        if handled {
            context.stop_propagation();
        }
    }

    /// 处理插件窗口鼠标释放。
    fn handle_plugin_window_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let had_drag = self.table_scrollbar_drag.is_some()
            || self.dragging_table_input_kind().is_some()
            || self
                .table_text_selection
                .as_ref()
                .is_some_and(|selection| selection.dragging);
        self.finish_plugin_table_scrollbar_drag(context);
        self.finish_all_table_input_mouse_selection(context);
        self.finish_table_cell_text_selection(context);
        if had_drag {
            context.stop_propagation();
        }
    }

    /// 限制单轴滚动容器只响应真实滚轮轴向。
    ///
    /// 业务意图：
    /// - 插件表格同时存在外层横向滚动和内层纵向虚拟列表滚动；纵向滚轮应只驱动纵向列表，不能顺带改变横向位置。
    /// - GPUI 默认会在只有横向滚动的容器上把纵向滚轮量转换成横向滚动量，这会导致用户向下滚动时表格横向漂移。
    ///
    /// 边界条件：
    /// - 真实横向滚动手势、Shift + 滚轮以及自绘横向滚动条仍然可用。
    /// - 该约束只加在插件表格的横向滚动容器上，不改变主窗口、日志正文或其它列表的滚动行为。
    fn restrict_scroll_to_wheel_axis(
        mut element: gpui::Stateful<gpui::Div>,
    ) -> gpui::Stateful<gpui::Div> {
        element.style().restrict_scroll_to_axis = Some(true);
        element
    }

    /// 渲染插件表格。
    ///
    /// 边界条件：
    /// - 表格没有行时仍保留表头和空状态，方便插件表达“扫描成功但无匹配”。
    /// - 单元格文本按原样显示，不做富文本解释，避免插件通过表格内容影响宿主 UI。
    fn render_table(
        &self,
        table: &PluginPageTable,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let column_count = table.headers.len().max(1);
        let column_widths = self.table_column_widths_for_render(table, column_count);
        let stretch_column_index = Self::plugin_table_stretch_column_index(table);
        let row_count = self.table_row_order.len().min(table.rows.len());
        let min_table_width = Self::plugin_table_min_width_from_widths(&column_widths);
        let scroll_handle = self.table_scroll_handle.clone();
        let x_scroll_handle = self.table_x_scroll_handle.clone();
        div()
            .id("plugin-page-table-scroll")
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .rounded(px(8.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(Self::restrict_scroll_to_wheel_axis(
                div()
                    .id("plugin-page-table-x-scroll")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_x_scroll()
                    .track_scroll(&x_scroll_handle)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h_0()
                            .min_w(px(min_table_width))
                            .w_full()
                            .child(self.render_table_header(
                                table,
                                column_count,
                                &column_widths,
                                stretch_column_index,
                                palette,
                                context,
                            ))
                            .child(if row_count == 0 {
                                self.render_empty_table_body(min_table_width, palette)
                            } else {
                                div()
                                    .id("plugin-page-table-virtual-wrapper")
                                    .relative()
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_hidden()
                                    .child(
                                        uniform_list(
                                            "plugin-page-table-virtual-list",
                                            row_count,
                                            context.processor(
                                                move |view,
                                                      range: std::ops::Range<usize>,
                                                      _window,
                                                      _context| {
                                                    let palette = view.palette;
                                                    let Some(table) = view.page.table.as_ref()
                                                    else {
                                                        return Vec::new();
                                                    };
                                                    let column_count = table.headers.len().max(1);
                                                    let column_widths = view
                                                        .table_column_widths_for_render(
                                                            table,
                                                            column_count,
                                                        );
                                                    let stretch_column_index =
                                                        Self::plugin_table_stretch_column_index(
                                                            table,
                                                        );
                                                    let action_column_index =
                                                        Self::plugin_table_action_column_index(
                                                            table,
                                                        );
                                                    range
                                                        .filter_map(|index| {
                                                            let source_index = view
                                                                .table_row_order
                                                                .get(index)
                                                                .copied()
                                                                .unwrap_or(index);
                                                            table.rows.get(source_index).map(|row| {
                                                                let row_actions = table
                                                                    .row_actions
                                                                    .get(source_index)
                                                                    .cloned()
                                                                    .unwrap_or_default();
                                                                (
                                                                    index,
                                                                    source_index,
                                                                    row.clone(),
                                                                    row_actions,
                                                                    action_column_index,
                                                                )
                                                            })
                                                        })
                                                        .map(
                                                            |(
                                                                index,
                                                                source_index,
                                                                row,
                                                                row_actions,
                                                                action_column_index,
                                                            )| {
                                                                view.render_table_row(
                                                                    index,
                                                                    source_index,
                                                                    row,
                                                                    row_actions,
                                                                    action_column_index,
                                                                    stretch_column_index,
                                                                &column_widths,
                                                                palette,
                                                                    _context,
                                                            )
                                                            },
                                                        )
                                                        .collect::<Vec<_>>()
                                                },
                                            ),
                                        )
                                        .size_full()
                                        .track_scroll(scroll_handle),
                                    )
                                    .child(self.render_plugin_table_vertical_scrollbar(
                                        &self.table_scroll_handle,
                                        row_count,
                                        palette,
                                        context,
                                    ))
                            }),
                    ),
            ))
            .child(self.render_plugin_table_horizontal_scrollbar(
                &self.table_x_scroll_handle,
                palette,
                context,
            ))
    }

    /// 渲染插件表格表头。
    fn render_table_header(
        &self,
        table: &PluginPageTable,
        column_count: usize,
        column_widths: &[f32],
        stretch_column_index: Option<usize>,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .w_full()
            .h(px(PLUGIN_TABLE_HEADER_HEIGHT))
            .flex_none()
            .border_b_1()
            .border_color(rgb(palette.border))
            .children((0..column_count).map(|column_index| {
                let header = table.headers.get(column_index).cloned().unwrap_or_default();
                let sort = self
                    .table_sort
                    .filter(|sort| sort.column_index == column_index);
                let sortable = Self::plugin_table_action_column_index(table) != Some(column_index);
                let column_width = column_widths
                    .get(column_index)
                    .copied()
                    .unwrap_or(PLUGIN_TABLE_FALLBACK_COLUMN_WIDTH);
                let stretches = stretch_column_index == Some(column_index);
                div()
                    .when(stretches, |cell| cell.min_w(px(column_width)).flex_1())
                    .when(!stretches, |cell| cell.w(px(column_width)).flex_none())
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted_text))
                    .bg(rgb(palette.panel))
                    .overflow_hidden()
                    .when(sortable, |cell| {
                        cell.cursor_pointer()
                            .hover(move |header| header.bg(rgb(palette.hover)))
                    })
                    .child(div().truncate().child(header))
                    .when_some(sort, |cell, sort| {
                        cell.child(MainView::render_lucide_icon(
                            Some(if sort.ascending {
                                Icon::ChevronUp
                            } else {
                                Icon::ChevronDown
                            }),
                            12.0,
                            12.0,
                            palette.accent,
                        ))
                    })
                    .when(sortable, |cell| {
                        cell.on_mouse_down(
                            MouseButton::Left,
                            context.listener(
                                move |view, _event: &MouseDownEvent, _window, context| {
                                    view.sort_table_by_column(column_index, context);
                                    context.stop_propagation();
                                },
                            ),
                        )
                    })
            }))
    }

    /// 渲染插件表格空状态。
    fn render_empty_table_body(
        &self,
        min_table_width: f32,
        palette: AppThemePalette,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("plugin-page-table-empty-body")
            .flex()
            .min_w(px(min_table_width))
            .w_full()
            .flex_1()
            .items_center()
            .px_3()
            .py_6()
            .text_sm()
            .text_color(rgb(palette.muted_text))
            .child("没有可展示的数据")
    }

    /// 渲染插件表格虚拟列表行。
    ///
    /// 边界条件：
    /// - 行高必须固定，不能让长路径撑高行，否则 `uniform_list` 的可见区间会失真。
    /// - 单元格内容统一截断，避免超长路径或异常插件输出导致水平布局测量过重。
    fn render_table_row(
        &self,
        row_index: usize,
        source_row_index: usize,
        row: Vec<String>,
        row_actions: Vec<PluginTableRowAction>,
        action_column_index: Option<usize>,
        stretch_column_index: Option<usize>,
        column_widths: &[f32],
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Div {
        let column_count = column_widths.len().max(1);
        div()
            .flex()
            .w_full()
            .h(px(PLUGIN_TABLE_ROW_HEIGHT))
            .flex_none()
            .bg(rgb(if row_index % 2 == 0 {
                palette.surface
            } else {
                palette.background
            }))
            .children((0..column_count).map(|column_index| {
                let is_action_column = action_column_index == Some(column_index);
                let cell_text = row.get(column_index).cloned().unwrap_or_default();
                let column_width = column_widths
                    .get(column_index)
                    .copied()
                    .unwrap_or(PLUGIN_TABLE_FALLBACK_COLUMN_WIDTH);
                let stretches = stretch_column_index == Some(column_index);
                div()
                    .when(stretches, |cell| cell.min_w(px(column_width)).flex_1())
                    .when(!stretches, |cell| cell.w(px(column_width)).flex_none())
                    .h_full()
                    .items_center()
                    .px_3()
                    .text_xs()
                    .text_color(rgb(palette.text))
                    .overflow_hidden()
                    .when(!is_action_column, |cell| {
                        let selected_text = cell_text.clone();
                        cell.flex()
                            .items_center()
                            .child(PluginTableSelectableTextElement {
                                view: context.entity(),
                                row_index: source_row_index,
                                column_index,
                                text: cell_text,
                                palette,
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                context.listener(
                                    move |view, event: &MouseDownEvent, window, context| {
                                        view.start_table_cell_text_selection(
                                            source_row_index,
                                            column_index,
                                            selected_text.clone(),
                                            event,
                                            window,
                                            context,
                                        );
                                        context.stop_propagation();
                                    },
                                ),
                            )
                    })
                    .when(is_action_column, |cell| {
                        cell.flex()
                            .gap_1()
                            .children(row_actions.iter().cloned().enumerate().map(
                                |(action_index, action)| {
                                    self.render_table_row_action_button(
                                        row_index,
                                        action_index,
                                        action,
                                        palette,
                                        context,
                                    )
                                },
                            ))
                    })
            }))
    }

    /// 渲染插件表格行内动作按钮。
    ///
    /// 业务意图：
    /// - 插件只能声明“打开另一个声明式页面”的动作；宿主负责渲染成按钮并消费点击事件。
    /// - 这样可以支持汇总表到明细表的钻取，同时避免插件直接获得 GPUI 控件或主窗口内部状态。
    fn render_table_row_action_button(
        &self,
        row_index: usize,
        action_index: usize,
        action: PluginTableRowAction,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let label = action.label.clone();
        div()
            .id(SharedString::from(format!(
                "plugin-table-action-{row_index}-{action_index}"
            )))
            .flex()
            .items_center()
            .justify_center()
            .h(px(24.0))
            .px_2()
            .rounded(px(5.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .text_xs()
            .text_color(rgb(palette.accent))
            .cursor_pointer()
            .hover(move |button| button.bg(rgb(palette.hover)))
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                    view.open_table_row_action(action.clone(), context);
                    context.stop_propagation();
                }),
            )
    }

    /// 执行插件表格行内动作。
    ///
    /// 业务意图：
    /// - 旧插件返回的静态页面直接打开；新插件可返回延迟命令，点击后先打开进度窗口，再等待插件生成结果。
    /// - 这样避免首次页面响应携带所有深层明细数据，尤其适合只在用户下钻时才需要读取的内容。
    fn open_table_row_action(&self, action: PluginTableRowAction, context: &mut Context<Self>) {
        if let Some(page) = action.page.clone() {
            self.open_table_row_action_page(action, *page, context);
            return;
        }

        if let Some(command) = action.command.clone() {
            self.open_table_row_action_command(action, command, context);
            return;
        }

        self.open_table_row_action_page(
            action,
            MainView::plugin_error_page("插件按钮缺少页面或命令，无法执行".to_string()),
            context,
        );
    }

    /// 打开插件表格行内动作声明的静态页面。
    ///
    /// 业务意图：
    /// - “详情”属于当前插件结果的本地钻取操作，不需要重新启动插件进程。
    /// - 每次点击打开独立窗口，避免覆盖汇总窗口，便于用户同时对照汇总和明细。
    fn open_table_row_action_page(
        &self,
        action: PluginTableRowAction,
        page: PluginPage,
        context: &mut Context<Self>,
    ) {
        let title = action.title.clone().unwrap_or_else(|| action.label.clone());
        let palette = self.palette;
        let window_title = title.clone();
        let origin_plugin = self.origin_plugin.clone();
        let origin_log_sources = self.origin_log_sources.clone();
        let origin_log_files = self.origin_log_files.clone();
        let window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(window_title.clone().into()),
                ..Default::default()
            }),
            window_bounds: Some(Self::next_plugin_window_bounds(context)),
            is_resizable: true,
            is_minimizable: true,
            window_min_size: Some(PLUGIN_PAGE_WINDOW_MIN_SIZE),
            ..Default::default()
        };

        if let Ok(window_handle) = context.open_window(window_options, move |window, app| {
            window.set_window_title(&window_title);
            window.activate_window();
            app.new(|context| {
                PluginPageWindowView::new(
                    title,
                    page,
                    palette,
                    origin_plugin,
                    origin_log_sources,
                    origin_log_files,
                    context,
                )
            })
        }) {
            window_handle
                .update(context, |_view, window, _context| {
                    window.focus(&_view.focus_handle);
                    window.activate_window();
                })
                .ok();
        }
    }

    /// 执行插件行内延迟命令并打开独立进度窗口。
    ///
    /// 业务意图：
    /// - 行内明细可能需要读取单个大日志文件，必须在点击后后台执行，不能阻塞当前插件结果窗口。
    /// - 新窗口先展示进度，插件最终返回页面后再原地替换，用户可以继续查看原汇总/详情窗口。
    fn open_table_row_action_command(
        &self,
        action: PluginTableRowAction,
        command: PluginTableRowCommand,
        context: &mut Context<Self>,
    ) {
        let Some(plugin) = self.origin_plugin.clone() else {
            self.open_table_row_action_page(
                action,
                MainView::plugin_error_page("无法确定行内动作所属插件".to_string()),
                context,
            );
            return;
        };
        let title = action.title.clone().unwrap_or_else(|| action.label.clone());
        let palette = self.palette;
        let initial_progress =
            MainView::initial_plugin_progress(format!("正在执行插件：{}", plugin.name), None, "项");
        let running_page = MainView::plugin_running_page(title.clone(), initial_progress.clone());
        let window_title = title.clone();
        let plugin_for_window = Some(plugin.clone());
        let origin_log_sources = self.origin_log_sources.clone();
        let origin_log_files = self.origin_log_files.clone();
        let window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(window_title.clone().into()),
                ..Default::default()
            }),
            window_bounds: Some(Self::next_plugin_window_bounds(context)),
            is_resizable: true,
            is_minimizable: true,
            window_min_size: Some(PLUGIN_PAGE_WINDOW_MIN_SIZE),
            ..Default::default()
        };

        let Ok(window_handle) = context.open_window(window_options, move |window, app| {
            window.set_window_title(&window_title);
            window.activate_window();
            app.new(|context| {
                PluginPageWindowView::new(
                    title,
                    running_page,
                    palette,
                    plugin_for_window,
                    origin_log_sources.clone(),
                    origin_log_files.clone(),
                    context,
                )
            })
        }) else {
            return;
        };
        let _ = window_handle.update(context, |window_view, window, context| {
            window.focus(&window_view.focus_handle);
            window_view.set_progress(initial_progress, context);
        });

        let (event_sender, event_receiver) = mpsc::channel();
        Self::spawn_deferred_action_event_poller(window_handle, event_receiver, context);
        let command_id = command.command_id;
        let command_context = self.hydrate_table_action_context(command.context);
        let origin_log_sources = self.origin_log_sources.clone();
        context
            .spawn(async move |_view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move {
                        let content_source = Self::deferred_plugin_command_content_source(
                            &command_context,
                            &origin_log_sources,
                        )?;
                        if let Some(content_source) = content_source {
                            invoke_plugin_command_with_content_stream(
                                &plugin,
                                &command_id,
                                command_context,
                                Some(event_sender),
                                move |writer, event_sender| {
                                    Self::write_log_source_content_stream(
                                        &content_source,
                                        writer,
                                        event_sender,
                                    )
                                },
                            )
                        } else {
                            invoke_plugin_command_with_progress(
                                &plugin,
                                &command_id,
                                command_context,
                                Some(event_sender),
                            )
                        }
                    })
                    .await;
                app.update(move |app| {
                    Self::apply_deferred_action_result(window_handle, result, palette, app);
                })
                .ok();
            })
            .detach();
    }

    /// 补齐表格延迟动作需要的页面来源快照。
    ///
    /// 业务意图：
    /// - 插件汇总页的行内按钮可以只在首次响应中保存行标识，避免把每行完整明细页提前序列化出来。
    /// - 用户实际点击按钮时，宿主再把当前插件窗口保存的原始日志文件快照放入上下文，由插件在后台生成该行详情。
    ///
    /// 边界条件：
    /// - 如果插件响应已经显式携带 `files`，保持插件提供的快照，兼容第三方插件自定义分页或局部下钻场景。
    /// - 补齐后的上下文仍会在插件调用前经过权限清理，未声明 `logs.content` 的插件拿不到本地读取路径。
    fn hydrate_table_action_context(&self, context: PluginCommandContext) -> PluginCommandContext {
        match context {
            PluginCommandContext::TableAction {
                action_id,
                files,
                data,
            } if files.is_empty() => PluginCommandContext::TableAction {
                action_id,
                files: self.origin_log_files.clone(),
                data,
            },
            other => other,
        }
    }

    /// 返回当前页面命令过滤器的插件命令。
    fn current_command_filter_command(&self) -> Option<PluginTableRowCommand> {
        self.page
            .table
            .as_ref()
            .and_then(|table| table.command_filter.as_ref())
            .map(|filter| filter.command.clone())
    }

    /// 清空插件命令过滤输入。
    fn clear_command_filter_inputs(&mut self) {
        for input_state in &mut self.command_filter_inputs {
            input_state.input.set_text(String::new());
            input_state.date_time_picker_open = false;
            let view_month = date_time_picker_month_from_text("");
            input_state.date_time_picker_view_year = view_month.year;
            input_state.date_time_picker_view_month = view_month.month;
        }
    }

    /// 把当前命令过滤输入合并进插件命令上下文。
    ///
    /// 业务意图：
    /// - 插件负责解析字段值并重新生成表格；宿主只把输入框原始文本按插件声明的 `key` 写回 `TableAction.data`。
    /// - 命令上下文还可能包含下钻参数，合并时必须保留已有字段，只覆盖插件声明的过滤键。
    fn with_current_command_filter_data(
        &self,
        context: PluginCommandContext,
    ) -> PluginCommandContext {
        match context {
            PluginCommandContext::TableAction {
                action_id,
                files,
                mut data,
            } => {
                for input_state in &self.command_filter_inputs {
                    let key = input_state.key.trim();
                    if !key.is_empty() {
                        data.insert(key.to_string(), input_state.input.text.trim().to_string());
                    }
                }
                PluginCommandContext::TableAction {
                    action_id,
                    files,
                    data,
                }
            }
            other => other,
        }
    }

    /// 应用插件命令过滤并在当前插件窗口内替换页面。
    ///
    /// 业务意图：
    /// - 命令过滤由插件重新计算页面；宿主只负责启动同一插件命令并替换当前窗口页面。
    /// - 命令在后台线程执行，当前窗口先展示进度，避免用户在大目录中过滤时误以为界面卡死。
    ///
    /// 边界条件：
    /// - 如果当前窗口缺少来源插件，说明页面不是由可回调插件生成，直接展示中文错误而不是静默失效。
    fn apply_plugin_table_command_filter(
        &mut self,
        command: PluginTableRowCommand,
        context: &mut Context<Self>,
    ) {
        self.close_all_command_filter_date_time_pickers();
        let Some(plugin) = self.origin_plugin.clone() else {
            self.page = MainView::plugin_error_page("无法确定过滤命令所属插件".to_string());
            context.notify();
            return;
        };
        let command_id = command.command_id;
        let command_context = self
            .hydrate_table_action_context(self.with_current_command_filter_data(command.context));
        let palette = self.palette;
        let origin_plugin = self.origin_plugin.clone();
        let origin_log_sources = self.origin_log_sources.clone();
        let origin_log_files = self.origin_log_files.clone();
        let initial_progress =
            MainView::initial_plugin_progress(format!("正在过滤：{}", plugin.name), None, "文件");
        let generation = self.current_window_command_generations.begin();
        self.set_progress(initial_progress.clone(), context);

        let (event_sender, event_receiver) = mpsc::channel();
        Self::spawn_current_window_plugin_event_poller(
            context.entity(),
            generation,
            event_receiver,
            context,
        );
        context
            .spawn(async move |view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move {
                        invoke_plugin_command_with_progress(
                            &plugin,
                            &command_id,
                            command_context,
                            Some(event_sender),
                        )
                    })
                    .await;
                app.update(move |app| {
                    let _ = view.update(app, |window_view, context| {
                        window_view.apply_current_window_command_result(
                            generation,
                            result,
                            palette,
                            origin_plugin,
                            origin_log_sources,
                            origin_log_files,
                            context,
                        );
                    });
                })
                .ok();
            })
            .detach();
    }

    /// 轮询当前插件窗口的后台命令事件。
    fn spawn_current_window_plugin_event_poller(
        window_view: Entity<PluginPageWindowView>,
        generation: usize,
        event_receiver: mpsc::Receiver<PluginCommandRuntimeEvent>,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |_view, app| {
                loop {
                    app.background_executor()
                        .timer(Duration::from_millis(33))
                        .await;
                    let keep_polling = window_view
                        .update(app, |window_view, context| {
                            window_view.drain_current_window_plugin_runtime_events(
                                generation,
                                &event_receiver,
                                context,
                            )
                        })
                        .unwrap_or(false);
                    if !keep_polling {
                        break;
                    }
                }
            })
            .detach();
    }

    /// 取出当前窗口后台命令事件并刷新页面。
    ///
    /// 业务意图：
    /// - 过滤命令可能被后续过滤命令取代，只有最新代次的事件才能更新当前页面。
    /// - 事件在 UI 线程批量应用，避免后台线程直接修改 GPUI 状态。
    fn drain_current_window_plugin_runtime_events(
        &mut self,
        generation: usize,
        event_receiver: &mpsc::Receiver<PluginCommandRuntimeEvent>,
        context: &mut Context<Self>,
    ) -> bool {
        if !self
            .current_window_command_generations
            .is_active(generation)
        {
            return false;
        }
        let mut disconnected = false;
        let mut runtime_events = Vec::new();
        loop {
            match event_receiver.try_recv() {
                Ok(event) => runtime_events.push(event),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        for event in runtime_events {
            self.apply_plugin_runtime_event(event, context);
        }
        !disconnected
    }

    /// 把后台命令结果应用到当前插件窗口。
    fn apply_current_window_command_result(
        &mut self,
        generation: usize,
        result: Result<PluginCommandResponse, String>,
        palette: AppThemePalette,
        origin_plugin: Option<PluginDefinition>,
        origin_log_sources: BTreeMap<String, LogFileSource>,
        origin_log_files: Vec<PluginLogFile>,
        context: &mut Context<Self>,
    ) {
        if !self
            .current_window_command_generations
            .finish_if_active(generation)
        {
            return;
        }
        let (title, page) = match result {
            Ok(PluginCommandResponse::OpenWindow { title, page }) => (title, page),
            Ok(PluginCommandResponse::ShowMessage { level, message }) => {
                let prefix = match level {
                    PluginMessageLevel::Info => "插件提示",
                    PluginMessageLevel::Warning => "插件警告",
                    PluginMessageLevel::Error => "插件错误",
                };
                (
                    prefix.to_string(),
                    MainView::plugin_message_page(prefix, message),
                )
            }
            Ok(PluginCommandResponse::Error { message }) | Err(message) => (
                "插件执行失败".to_string(),
                MainView::plugin_error_page(message),
            ),
        };
        self.set_page(
            title,
            page,
            palette,
            origin_plugin,
            origin_log_sources,
            origin_log_files,
            context,
        );
    }

    /// 返回延迟行命令需要由宿主流式传给插件的日志来源。
    ///
    /// 业务意图：
    /// - 初始汇总解析可以只读取文件名，不能为了后续下钻预先读取或解压上万份日志。
    /// - 用户真正点击延迟按钮时，宿主用触发插件时保存的 `source_key -> LogFileSource` 快照定位单个文件，并把正文按行写入插件 stdin。
    ///
    /// 边界条件：
    /// - 如果插件已经带回 `read_path`，说明它可以直接读取本地文件，不需要宿主内容流。
    /// - 如果只有 `source_key`，必须能在当前窗口快照中找到原始来源；找不到时返回中文错误而不是让插件空跑。
    fn deferred_plugin_command_content_source(
        context: &PluginCommandContext,
        origin_log_sources: &BTreeMap<String, LogFileSource>,
    ) -> Result<Option<LogFileSource>, String> {
        let PluginCommandContext::LogFileAction { file, .. } = context else {
            return Ok(None);
        };
        if file.read_path.is_some() {
            return Ok(None);
        }

        origin_log_sources
            .get(&file.source_key)
            .cloned()
            .map(Some)
            .ok_or_else(|| format!("无法定位插件日志来源：{}", file.display_name))
    }

    /// 把日志来源按行写入插件 stdin 内容流。
    ///
    /// 业务意图：
    /// - 宿主直接读取用户触发下钻的单个日志来源，并把字节流按行转换成插件协议 JSON Lines。
    /// - 普通文件、压缩包成员和已物化成员都通过同一个流式 writer 消费，避免为了插件解析生成临时日志文件或整块日志内存。
    ///
    /// 关键约束：
    /// - 该函数只在后台任务中调用，避免大文件顺序读取阻塞 GPUI 主线程。
    /// - 插件拿到的是当前日志内容流，不获得任意本地路径读取能力；路径权限仍由宿主 `logs.content` 控制。
    fn write_log_source_content_stream(
        source: &LogFileSource,
        writer: &mut dyn Write,
        event_sender: Option<&mpsc::Sender<PluginCommandRuntimeEvent>>,
    ) -> Result<(), String> {
        if let Some(sender) = event_sender {
            let _ = sender.send(PluginCommandRuntimeEvent::Progress(PluginCommandProgress {
                message: "正在流式读取日志正文".to_string(),
                detail: Some(source.display_name()),
                done: 0,
                total: None,
                unit: Some("行".to_string()),
            }));
        }
        let mut line_writer = PluginContentLineWriter::new(writer, event_sender);
        stream_log_source_bytes_to_writer(source, &mut line_writer)
            .map_err(|error| format!("流式读取日志正文失败：{error}"))?;
        line_writer.finish().map(|_| ())
    }

    /// 按插件请求把指定日志来源写入交互式内容流。
    ///
    /// 业务意图：
    /// - 工具栏插件先拿到完整树快照，再用 source_key 请求自己真正需要分析的文件正文；宿主只在这里根据保存的来源快照读取文件。
    /// - 读取失败或 source_key 不存在时写入 `log_content_error` 事件，让插件能继续处理后续文件，而不是让整个分析中断。
    ///
    /// 边界条件：
    /// - `path_label` 只作为 UI 回显，不能参与文件定位；真实读取必须来自 `origin_log_sources`。
    /// - 普通文件、压缩包成员和嵌套压缩包成员都通过 `stream_log_source_bytes_to_writer` 统一读取，避免为压缩包正文另写一套路径权限逻辑。
    fn write_requested_log_source_content_stream(
        request: PluginContentRequest,
        origin_log_sources: &BTreeMap<String, LogFileSource>,
        writer: &mut dyn Write,
        event_sender: Option<&mpsc::Sender<PluginCommandRuntimeEvent>>,
    ) -> Result<(), String> {
        let path_label = request
            .path_label
            .as_deref()
            .filter(|label| !label.trim().is_empty())
            .unwrap_or(&request.source_key);
        let Some(source) = origin_log_sources.get(&request.source_key) else {
            return write_plugin_log_content_error(
                writer,
                &request.source_key,
                "宿主无法定位该日志来源，可能是快照已失效或来源不可读取",
            );
        };

        write_plugin_log_content_begin(writer, &request.source_key, path_label)?;
        let mut line_writer = PluginContentLineWriter::new(writer, event_sender);
        let stream_result = stream_log_source_bytes_to_writer(source, &mut line_writer)
            .map_err(|error| format!("流式读取日志正文失败：{error}"));
        match stream_result {
            Ok(()) => {
                let lines = line_writer.finish()?;
                write_plugin_log_content_end(writer, &request.source_key, lines)
            }
            Err(message) => {
                drop(line_writer);
                write_plugin_log_content_error(writer, &request.source_key, &message)
            }
        }
    }

    /// 按固定步长向 UI 汇报宿主侧内容流进度。
    fn report_content_stream_progress(
        done: u64,
        event_sender: Option<&mpsc::Sender<PluginCommandRuntimeEvent>>,
    ) {
        if (done == 1 || done % 2048 == 0)
            && let Some(sender) = event_sender
        {
            let _ = sender.send(PluginCommandRuntimeEvent::Progress(PluginCommandProgress {
                message: "正在流式读取日志正文".to_string(),
                detail: Some(format!("已发送 {done} 行")),
                done,
                total: None,
                unit: Some("行".to_string()),
            }));
        }
    }

    /// 轮询延迟命令流式事件并更新独立插件窗口。
    fn spawn_deferred_action_event_poller(
        window_handle: WindowHandle<PluginPageWindowView>,
        receiver: mpsc::Receiver<PluginCommandRuntimeEvent>,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |_view, app| {
                loop {
                    app.background_executor()
                        .timer(Duration::from_millis(80))
                        .await;
                    let mut disconnected = false;
                    let mut latest_progress = None;
                    let mut runtime_events = Vec::new();
                    loop {
                        match receiver.try_recv() {
                            Ok(PluginCommandRuntimeEvent::Progress(progress)) => {
                                latest_progress = Some(progress)
                            }
                            Ok(event) => runtime_events.push(event),
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => {
                                disconnected = true;
                                break;
                            }
                        }
                    }
                    if let Some(progress) = latest_progress {
                        let _ = window_handle.update(app, |window_view, _window, context| {
                            window_view.set_progress(progress, context);
                        });
                    }
                    if !runtime_events.is_empty() {
                        let _ = window_handle.update(app, |window_view, _window, context| {
                            for event in runtime_events {
                                window_view.apply_plugin_runtime_event(event, context);
                            }
                        });
                    }
                    if disconnected {
                        break;
                    }
                }
            })
            .detach();
    }

    /// 把延迟命令结果写回独立插件窗口。
    fn apply_deferred_action_result(
        window_handle: WindowHandle<PluginPageWindowView>,
        result: Result<PluginCommandResponse, String>,
        palette: AppThemePalette,
        app: &mut App,
    ) {
        let (title, page) = match result {
            Ok(PluginCommandResponse::OpenWindow { title, page }) => (title, page),
            Ok(PluginCommandResponse::ShowMessage { level, message }) => {
                let prefix = match level {
                    PluginMessageLevel::Info => "插件提示",
                    PluginMessageLevel::Warning => "插件警告",
                    PluginMessageLevel::Error => "插件错误",
                };
                (
                    prefix.to_string(),
                    MainView::plugin_message_page(prefix, message),
                )
            }
            Ok(PluginCommandResponse::Error { message }) | Err(message) => (
                "插件执行失败".to_string(),
                MainView::plugin_error_page(message),
            ),
        };
        let window_title = title.clone();
        let _ = window_handle.update(app, |window_view, window, context| {
            let origin_plugin = window_view.origin_plugin.clone();
            let origin_log_sources = window_view.origin_log_sources.clone();
            let origin_log_files = window_view.origin_log_files.clone();
            window_view.set_page(
                title,
                page,
                palette,
                origin_plugin,
                origin_log_sources,
                origin_log_files,
                context,
            );
            window.set_window_title(&window_title);
            window.focus(&window_view.focus_handle);
            window.activate_window();
        });
    }
}

impl EntityInputHandler for PluginPageWindowView {
    /// 返回当前聚焦输入框指定 UTF-16 范围内的文本。
    ///
    /// 业务意图：
    /// - macOS 和 Windows 的平台输入协议按 UTF-16 位置回调，Rust 字符串按 UTF-8 存储；这里统一做安全边界转换。
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<String> {
        let input_kind = self.focused_table_input_kind(window)?;
        let input = self.table_input_state(input_kind)?;
        let range = MainView::search_input_range_from_utf16(&input.text, range_utf16);
        adjusted_range.replace(MainView::search_input_range_to_utf16(
            &input.text,
            range.clone(),
        ));
        Some(input.text[range].to_string())
    }

    /// 返回当前聚焦输入框的选区。
    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let input_kind = self.focused_table_input_kind(window)?;
        let input = self.table_input_state(input_kind)?;
        Some(UTF16Selection {
            range: MainView::search_input_range_to_utf16(
                &input.text,
                input.selection_range.clone(),
            ),
            reversed: false,
        })
    }

    /// 返回输入法组合文本范围。
    fn marked_text_range(
        &self,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let input_kind = self.focused_table_input_kind(window)?;
        let input = self.table_input_state(input_kind)?;
        input
            .marked_range
            .clone()
            .map(|range| MainView::search_input_range_to_utf16(&input.text, range))
    }

    /// 清除输入法组合文本状态。
    fn unmark_text(&mut self, window: &mut Window, context: &mut Context<Self>) {
        if let Some(input_kind) = self.focused_table_input_kind(window) {
            let Some(input) = self.table_input_state_mut(input_kind) else {
                return;
            };
            input.marked_range = None;
            context.notify();
        }
    }

    /// 用平台提交文本替换当前聚焦输入框中的指定范围。
    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(input_kind) = self.focused_table_input_kind(window) else {
            return;
        };
        let replacement = MainView::sanitize_search_input_text(text);
        {
            let Some(input) = self.table_input_state_mut(input_kind) else {
                return;
            };
            let range = range_utf16
                .map(|range| MainView::search_input_range_from_utf16(&input.text, range))
                .or_else(|| input.marked_range.clone())
                .unwrap_or_else(|| input.selection_range.clone());
            let range = MainView::clamp_search_text_range(&input.text, range);
            input.text.replace_range(range.clone(), &replacement);
            let cursor = range.start + replacement.len();
            input.selection_range = cursor..cursor;
            input.marked_range = None;
        }
        self.after_table_input_text_changed(input_kind, context);
    }

    /// 用平台组合文本替换当前聚焦输入框中的指定范围，并保留组合状态。
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(input_kind) = self.focused_table_input_kind(window) else {
            return;
        };
        let replacement = MainView::sanitize_search_input_text(new_text);
        {
            let Some(input) = self.table_input_state_mut(input_kind) else {
                return;
            };
            let range = range_utf16
                .map(|range| MainView::search_input_range_from_utf16(&input.text, range))
                .or_else(|| input.marked_range.clone())
                .unwrap_or_else(|| input.selection_range.clone());
            let range = MainView::clamp_search_text_range(&input.text, range);
            input.text.replace_range(range.clone(), &replacement);
            input.marked_range = if replacement.is_empty() {
                None
            } else {
                Some(range.start..range.start + replacement.len())
            };
            let selected_range = new_selected_range_utf16
                .map(|utf16_range| {
                    MainView::search_input_range_from_utf16(&replacement, utf16_range)
                })
                .map(|relative_range| {
                    range.start + relative_range.start..range.start + relative_range.end
                })
                .unwrap_or_else(|| {
                    let cursor = range.start + replacement.len();
                    cursor..cursor
                });
            input.selection_range = MainView::clamp_search_text_range(&input.text, selected_range);
        }
        self.after_table_input_text_changed(input_kind, context);
    }

    /// 返回指定文本范围在窗口中的边界，用于 IME 候选框定位。
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let input_kind = self.focused_table_input_kind(window)?;
        let input = self.table_input_state(input_kind)?;
        let Some(layout) = self.table_input_layout(input_kind) else {
            return Some(element_bounds);
        };
        let range = MainView::search_input_range_from_utf16(&input.text, range_utf16);
        let cursor = range.start;
        let cursor_x = layout.bounds.left() - px(layout.horizontal_scroll_px)
            + layout.line.x_for_index(cursor);
        Some(Bounds::new(
            point(cursor_x, layout.bounds.top()),
            size(px(1.0), layout.bounds.bottom() - layout.bounds.top()),
        ))
    }

    /// 根据鼠标位置返回过滤输入框插入点。
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _context: &mut Context<Self>,
    ) -> Option<usize> {
        let input_kind = self.focused_table_input_kind(window)?;
        let input = self.table_input_state(input_kind)?;
        let utf8_index = self.table_input_index_at_position(input_kind, point);
        Some(MainView::search_input_utf16_offset_from_byte(
            &input.text,
            utf8_index,
        ))
    }
}

/// 比较插件表格单元格。
///
/// 业务意图：
/// - 表格列头排序应符合用户直觉：纯数字按数值大小排序，普通文本按大小写不敏感字典序排序。
/// - 时间字符串采用 `YYYY-MM-DD HH:MM:SS.mmm` 这类可按字典序比较的格式，因此无需在宿主反解析时间。
fn compare_plugin_table_cells(left: &str, right: &str) -> Ordering {
    let left_trimmed = left.trim();
    let right_trimmed = right.trim();
    match (left_trimmed.parse::<f64>(), right_trimmed.parse::<f64>()) {
        (Ok(left_number), Ok(right_number)) => left_number
            .partial_cmp(&right_number)
            .unwrap_or(Ordering::Equal),
        _ => {
            let left_lower = left_trimmed.to_lowercase();
            let right_lower = right_trimmed.to_lowercase();
            left_lower
                .cmp(&right_lower)
                .then_with(|| left_trimmed.cmp(right_trimmed))
        }
    }
}

/// 插件窗口内部后台命令代次状态。
///
/// 业务意图：
/// - 插件声明式命令过滤会在同一个插件窗口内反复启动后台插件命令，旧命令可能比新命令更晚返回。
/// - 该状态只服务当前窗口内部的竞争消解，防止旧进度和旧结果覆盖用户最新输入的过滤条件。
///
/// 边界条件：
/// - 代次不写入磁盘，也不跨窗口复用；窗口页面被外部命令替换时应取消当前代次。
/// - 溢出时沿用宿主插件命令的饱和加一策略，真实会话中不会达到 `usize::MAX`。
#[derive(Clone, Debug, PartialEq, Eq)]
struct PluginWindowCommandGenerationState {
    /// 当前仍允许更新窗口的后台命令代次。
    active: Option<usize>,
    /// 下一次后台命令使用的代次。
    next: usize,
}

impl PluginWindowCommandGenerationState {
    /// 创建空闲的窗口命令代次状态。
    fn new() -> Self {
        Self {
            active: None,
            next: 1,
        }
    }

    /// 开始追踪新的窗口内后台命令。
    fn begin(&mut self) -> usize {
        let generation = self.next;
        self.next = self.next.saturating_add(1);
        self.active = Some(generation);
        generation
    }

    /// 判断指定代次是否仍然是当前窗口最新命令。
    fn is_active(&self, generation: usize) -> bool {
        self.active == Some(generation)
    }

    /// 仅在指定代次仍然有效时结束追踪。
    fn finish_if_active(&mut self, generation: usize) -> bool {
        if !self.is_active(generation) {
            return false;
        }
        self.active = None;
        true
    }

    /// 取消当前窗口内后台命令的 UI 更新权限。
    fn cancel(&mut self) {
        self.active = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 覆盖插件表格数值列排序规则。
    ///
    /// 业务意图：
    /// - 插件返回的数值列必须按数值比较，避免字符串排序把 `100` 排在 `20` 前面。
    #[test]
    fn 插件表格单元格数字按数值比较() {
        assert_eq!(compare_plugin_table_cells("20", "100"), Ordering::Less);
    }

    /// 覆盖插件窗口内部后台命令代次的竞争消解。
    ///
    /// 业务意图：
    /// - 插件声明式命令过滤会在同一个窗口内连续启动插件进程，旧进程的结果不能结束或覆盖新进程。
    /// - 该测试只验证纯状态规则，避免依赖 GPUI 窗口和真实插件进程。
    #[test]
    fn 插件窗口命令代次只接受最新任务() {
        let mut state = PluginWindowCommandGenerationState::new();
        let first = state.begin();
        let second = state.begin();

        assert!(!state.finish_if_active(first));
        assert!(state.is_active(second));
        assert!(state.finish_if_active(second));
        assert!(!state.is_active(second));
    }

    /// 覆盖插件表头宽度只按通用文本长度估算。
    ///
    /// 业务意图：
    /// - 宿主不能按插件业务列名做特殊宽度优化；短表头不应超过通用兜底宽度。
    #[test]
    fn 插件表格短表头宽度不超过兜底列宽() {
        assert!(
            PluginPageWindowView::plugin_table_column_width("主要字段")
                <= PLUGIN_TABLE_FALLBACK_COLUMN_WIDTH
        );
    }

    /// 覆盖带动作列的表格不会因为固定列宽过大而触发横向滚动。
    ///
    /// 业务意图：
    /// - 主文本列应作为弹性列吸收剩余空间，而不是用超大固定宽度把操作列挤到横向滚动区域。
    #[test]
    fn 动作表格最小宽度适配插件窗口() {
        let table = PluginPageTable {
            headers: vec![
                "主文本".to_string(),
                "数值".to_string(),
                "来源".to_string(),
                "时间".to_string(),
                "操作".to_string(),
            ],
            rows: Vec::new(),
            command_filter: None,
            row_actions: vec![vec![PluginTableRowAction {
                label: "详情".to_string(),
                title: None,
                page: None,
                command: None,
            }]],
        };

        assert_eq!(
            PluginPageWindowView::plugin_table_stretch_column_index(&table),
            Some(0)
        );
        assert!(
            PluginPageWindowView::plugin_table_min_width(&table, table.headers.len())
                <= f32::from(PLUGIN_PAGE_WINDOW_MIN_SIZE.width) - 80.0
        );
    }

    /// 覆盖插件表格撑满容器时的弹性列选择。
    ///
    /// 业务意图：
    /// - 操作列只承载按钮，不能作为吸收剩余宽度的列；否则大窗口下按钮会被拉到不自然的位置。
    /// - 第一列通常是主要业务字段，应负责撑满插件列表容器。
    #[test]
    fn 插件表格弹性列避开操作列() {
        let table = PluginPageTable {
            headers: vec!["主文本".to_string(), "操作".to_string()],
            rows: vec![vec!["alpha".to_string(), String::new()]],
            command_filter: None,
            row_actions: vec![vec![PluginTableRowAction {
                label: "详情".to_string(),
                title: None,
                page: Some(Box::new(PluginPage::default())),
                command: None,
            }]],
        };

        assert_eq!(
            PluginPageWindowView::plugin_table_stretch_column_index(&table),
            Some(0)
        );
    }

    /// 覆盖插件表格过滤按任意列关键字收窄结果。
    ///
    /// 业务意图：
    /// - 第三方插件表格列语义不固定，过滤必须扫描整行所有单元格，而不是只看某个固定业务列。
    /// - 多关键字同时命中才能保留，方便用户用不同列片段逐步缩小范围。
    #[test]
    fn 插件表格过滤支持任意列多关键字() {
        let row = vec![
            "alpha/workflow".to_string(),
            "owner-a".to_string(),
            "payload-fragment".to_string(),
        ];
        assert!(PluginPageWindowView::plugin_table_row_matches_filter(
            &row,
            "WORKFLOW owner"
        ));
        assert!(PluginPageWindowView::plugin_table_row_matches_filter(
            &row, "payload"
        ));
        assert!(!PluginPageWindowView::plugin_table_row_matches_filter(
            &row,
            "workflow missing"
        ));
    }

    /// 覆盖长文本内容会扩大列宽以触发横向滚动。
    ///
    /// 业务意图：
    /// - 任意插件长文本通常无法在默认列宽内完整展示，宿主需要根据内容估算列宽，让表格在必要时出现横向滚动条。
    #[test]
    fn 插件表格长内容会扩大列宽() {
        let long_text = format!("prefix-{}", "x".repeat(180));
        let table = PluginPageTable {
            headers: vec!["长文本".to_string()],
            rows: vec![vec![long_text]],
            command_filter: None,
            row_actions: Vec::new(),
        };

        let widths = PluginPageWindowView::plugin_table_column_widths(&table, 1);
        assert!(widths[0] > PluginPageWindowView::plugin_table_column_width("长文本"));
    }

    /// 覆盖插件表格文本选区不会切断 UTF-8 字符。
    ///
    /// 业务意图：
    /// - 表格单元格现在支持拖动选择文本片段；中文路径和其它多字节字符都必须安全处理。
    /// - 复制前必须把鼠标命中范围夹到字符边界，避免生成非法字符串或复制失败。
    #[test]
    fn 插件表格文本选区按字符边界裁剪() {
        assert_eq!(
            PluginPageWindowView::normalize_plugin_table_text_range("a中b", 2..99),
            1..5
        );
    }

    /// 覆盖步骤式输出打字机按字符截取正文。
    ///
    /// 业务意图：
    /// - 插件步骤输出可能包含中文路径、中文异常原因和原始日志摘要，打字机效果必须按字符推进。
    /// - 如果按字节截取，多字节中文会在渲染前被切坏，导致窗口正文丢失或 panic。
    #[test]
    fn 插件步骤正文打字机按字符推进() {
        assert_eq!(
            PluginPageWindowView::visible_output_step_text("a中b", 2),
            "a中"
        );
    }

    /// 覆盖步骤输出中的日志路径行识别。
    ///
    /// 业务意图：
    /// - 扫描摘要中的日志路径需要用灰色弱化显示，但普通异常说明仍应使用正文颜色。
    /// - 只识别插件约定的 `- <path>` 形态，避免误把诊断结论或完成摘要染成路径样式。
    #[test]
    fn 插件步骤普通文本能识别日志路径行() {
        assert!(
            PluginPageWindowView::output_step_plain_text_line_is_log_path(
                "- 192.168.9.172downLog.zip!/2026-05-21/server_2026-05-21.log"
            )
        );
        assert!(
            !PluginPageWindowView::output_step_plain_text_line_is_log_path(
                "异常行 2177 [2026-05-21 03:01:45] 单次FGCT增长超过2秒"
            )
        );
    }

    /// 覆盖插件异常上下文内部协议解析。
    ///
    /// 业务意图：
    /// - 插件返回的日志片段标记必须被宿主转换成结构化块，避免用户在流式输出中看到内部协议文本。
    /// - 高亮词和异常行标记会影响截图式日志块渲染，解析错误会直接降低问题定位效率。
    #[test]
    fn 插件步骤日志片段解析为结构化块() {
        let blocks = PluginPageWindowView::parse_output_step_text_blocks(
            "开始\n@@LC_LOG_SNIPPET_BEGIN\n@@LC_LOG_LINE\t42\tissue\t91.2\t2026-05-22 00:00:12\t100\t87.3\t91.2\n@@LC_LOG_SNIPPET_END\n完成\n",
        );

        assert_eq!(blocks.len(), 3);
        assert_eq!(
            blocks[0],
            PluginOutputStepTextBlock::Text(vec!["开始".to_string()])
        );
        let PluginOutputStepTextBlock::LogSnippet(lines) = &blocks[1] else {
            panic!("第二个块应为日志片段");
        };
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].line_number, "42");
        assert!(lines[0].is_issue);
        assert_eq!(lines[0].highlight_terms, vec!["91.2"]);
        assert!(lines[0].text.contains("12    100    87.3"));
        assert_eq!(
            blocks[2],
            PluginOutputStepTextBlock::Text(vec!["完成".to_string(), " ".to_string()])
        );
    }
}

impl Render for PluginPageWindowView {
    fn render(&mut self, window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let palette = self.palette;
        let has_output_steps = !self.page.output_steps.is_empty();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .track_focus(&self.focus_handle)
            .key_context("plugin-page-window")
            .on_key_down(context.listener(Self::handle_plugin_window_key_down))
            .on_mouse_move(context.listener(Self::handle_plugin_window_mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                context.listener(Self::handle_plugin_window_mouse_up),
            )
            .child(
                div()
                    .id("plugin-page-body-scroll")
                    .flex()
                    .flex_col()
                    .gap_4()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .p_5()
                    .when(has_output_steps, |body| {
                        body.child(self.render_output_steps(palette, window))
                    })
                    .when(!has_output_steps, |body| {
                        body.when_some(self.page.description.clone(), |body, description| {
                            body.child(
                                div()
                                    .text_sm()
                                    .line_height(px(20.0))
                                    .text_color(rgb(palette.muted_text))
                                    .child(description),
                            )
                        })
                        .when_some(self.page.progress.as_ref(), |body, progress| {
                            body.child(self.render_progress_card(progress, palette))
                        })
                        .when(!self.page.stats.is_empty(), |body| {
                            body.child(self.render_stat_grid(palette))
                        })
                        .when(!self.page.output.is_empty(), |body| {
                            body.child(self.render_output_flow(palette))
                        })
                        .when_some(
                            self.page.table.as_ref(),
                            |body, table| {
                                body.child(self.render_table_filter_bar(table, palette, context))
                                    .child(self.render_table(table, palette, context))
                            },
                        )
                    }),
            )
    }
}

impl MainView {
    /// 从插件日志文件快照中提取宿主内部来源映射。
    ///
    /// 业务意图：
    /// - 首次插件调用只把可序列化字段发给外部进程；宿主仍需要保存 `source_key -> LogFileSource`，供后续行内延迟命令按原始选择读取正文。
    /// - 映射只在当前插件窗口生命周期内使用，不写入磁盘，也不随左侧树后续选择变化。
    fn plugin_log_source_map(files: &[PluginLogFile]) -> BTreeMap<String, LogFileSource> {
        files
            .iter()
            .filter_map(|file| {
                file.host_source
                    .clone()
                    .map(|source| (file.source_key.clone(), source))
            })
            .collect()
    }

    /// 把插件命令响应分发到宿主 UI。
    ///
    /// 业务意图：
    /// - 插件命令在后台线程完成后回到 `App` 上下文；这里统一处理打开窗口、展示消息和错误状态。
    /// - 响应分发不依赖发起位置，日志树、笔记树和未来导航页都可以复用。
    pub(in crate::app) fn handle_plugin_command_result_after_main_update(
        main_view: Entity<MainView>,
        generation: usize,
        origin_plugin: Option<PluginDefinition>,
        origin_log_sources: BTreeMap<String, LogFileSource>,
        origin_log_files: Vec<PluginLogFile>,
        result: Result<PluginCommandResponse, String>,
        app: &mut App,
    ) {
        let should_apply = main_view.update(app, |view, context| {
            if view.plugins.active_command_generation != Some(generation) {
                return false;
            }
            view.plugins.active_command_generation = None;
            context.notify();
            true
        });
        if !should_apply {
            return;
        }

        match result {
            Ok(PluginCommandResponse::OpenWindow { title, page }) => {
                Self::open_plugin_page_window_after_main_update(
                    main_view,
                    title,
                    page,
                    origin_plugin,
                    origin_log_sources,
                    origin_log_files,
                    app,
                );
            }
            Ok(PluginCommandResponse::ShowMessage { level, message }) => {
                let prefix = match level {
                    PluginMessageLevel::Info => "插件提示",
                    PluginMessageLevel::Warning => "插件警告",
                    PluginMessageLevel::Error => "插件错误",
                };
                main_view.update(app, |view, context| {
                    view.plugins.status_message = Some(format!("{prefix}：{message}"));
                    context.notify();
                });
                Self::open_plugin_page_window_after_main_update(
                    main_view,
                    "插件提示".to_string(),
                    Self::plugin_message_page(prefix, message),
                    origin_plugin,
                    origin_log_sources,
                    origin_log_files,
                    app,
                );
            }
            Ok(PluginCommandResponse::Error { message }) | Err(message) => {
                main_view.update(app, |view, context| {
                    view.plugins.status_message = Some(format!("插件执行失败：{message}"));
                    context.notify();
                });
                Self::open_plugin_page_window_after_main_update(
                    main_view,
                    "插件执行失败".to_string(),
                    Self::plugin_error_page(message),
                    origin_plugin,
                    origin_log_sources,
                    origin_log_files,
                    app,
                );
            }
        }
    }

    /// 打开或复用插件声明式页面窗口。
    ///
    /// 边界条件：
    /// - 如果旧窗口句柄已经失效，清空句柄并创建新窗口；窗口关闭时同步清理主视图状态。
    pub(in crate::app) fn open_plugin_page_window_after_main_update(
        main_view: Entity<MainView>,
        title: String,
        page: PluginPage,
        origin_plugin: Option<PluginDefinition>,
        origin_log_sources: BTreeMap<String, LogFileSource>,
        origin_log_files: Vec<PluginLogFile>,
        app: &mut App,
    ) {
        let (existing_window, palette) =
            main_view.update(app, |view, _| (view.plugins.page_window, view.palette()));
        if let Some(window_handle) = existing_window {
            let reused_title = title.clone();
            if window_handle
                .update(app, |window_view, window, context| {
                    window_view.set_page(
                        title.clone(),
                        page.clone(),
                        palette,
                        origin_plugin.clone(),
                        origin_log_sources.clone(),
                        origin_log_files.clone(),
                        context,
                    );
                    window.set_window_title(&reused_title);
                    window.focus(&window_view.focus_handle);
                    window.activate_window();
                })
                .is_ok()
            {
                return;
            }
            main_view.update(app, |view, _| {
                view.plugins.page_window = None;
            });
        }

        let main_view_for_close = main_view.clone();
        let window_options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(title.clone().into()),
                ..Default::default()
            }),
            window_bounds: Some(PluginPageWindowView::next_plugin_window_bounds(app)),
            is_resizable: true,
            is_minimizable: true,
            window_min_size: Some(PLUGIN_PAGE_WINDOW_MIN_SIZE),
            ..Default::default()
        };

        match app.open_window(window_options, move |window, app| {
            window.on_window_should_close(app, move |_, app| {
                main_view_for_close.update(app, |view, context| {
                    view.plugins.page_window = None;
                    view.plugins.active_command_generation = None;
                    context.notify();
                });
                true
            });
            app.new(|context| {
                PluginPageWindowView::new(
                    title,
                    page,
                    palette,
                    origin_plugin,
                    origin_log_sources,
                    origin_log_files,
                    context,
                )
            })
        }) {
            Ok(window_handle) => {
                let _ = window_handle.update(app, |window_view, window, _context| {
                    window.focus(&window_view.focus_handle);
                    window.activate_window();
                });
                main_view.update(app, |view, context| {
                    view.plugins.page_window = Some(window_handle);
                    context.notify();
                });
            }
            Err(error) => {
                main_view.update(app, |view, context| {
                    view.plugins.status_message = Some(format!("打开插件窗口失败：{error}"));
                    view.plugins.page_window = None;
                    context.notify();
                });
            }
        }
    }

    /// 在当前主视图事件结束后打开或复用插件窗口。
    ///
    /// 业务意图：
    /// - GPUI 禁止在 `MainView` 更新过程中创建一个马上读取 `MainView` 的窗口；旧插件快速返回时尤其容易触发实体重入 panic。
    /// - 因此这里只调度一次后续 App 更新，并用命令代次确认当前进度页仍然有效，避免旧进度页覆盖已经到达的最终结果。
    pub(in crate::app) fn schedule_open_plugin_page_window_from_context(
        &self,
        main_view: Entity<MainView>,
        generation: usize,
        title: String,
        page: PluginPage,
        origin_plugin: Option<PluginDefinition>,
        origin_log_sources: BTreeMap<String, LogFileSource>,
        origin_log_files: Vec<PluginLogFile>,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |_view, app| {
                app.update(move |app| {
                    let should_open = main_view.update(app, |view, _| {
                        view.plugins.active_command_generation == Some(generation)
                    });
                    if should_open {
                        Self::open_plugin_page_window_after_main_update(
                            main_view,
                            title,
                            page,
                            origin_plugin,
                            origin_log_sources,
                            origin_log_files,
                            app,
                        );
                    }
                })
                .ok();
            })
            .detach();
    }

    /// 构造插件运行中的初始页面。
    pub(in crate::app) fn plugin_running_page(
        title: String,
        progress: PluginCommandProgress,
    ) -> PluginPage {
        PluginPage {
            title,
            description: Some("插件正在后台处理数据，处理完成后会自动替换为结果页面。".to_string()),
            stats: Vec::new(),
            output: Vec::new(),
            output_steps: Vec::new(),
            table: None,
            progress: Some(progress),
        }
    }

    /// 构造插件步骤式运行初始页面。
    ///
    /// 业务意图：
    /// - 步骤式插件点击后需要立即显示首个具体步骤，不展示通用进度条。
    /// - 页面只携带步骤模型，后续 stdout 事件追加正文；旧插件仍使用 `plugin_running_page`。
    pub(in crate::app) fn plugin_step_running_page(
        title: String,
        initial_step: PluginInitialOutputStep,
    ) -> PluginPage {
        PluginPage {
            title,
            description: None,
            stats: Vec::new(),
            output: Vec::new(),
            output_steps: vec![PluginOutputStep {
                id: initial_step.id,
                loading_text: initial_step.loading_text,
                done_text: None,
                status: PluginOutputStepStatus::Running,
                content: String::new(),
            }],
            table: None,
            progress: None,
        }
    }

    /// 构造插件提示页面。
    fn plugin_message_page(prefix: &str, message: String) -> PluginPage {
        PluginPage {
            title: prefix.to_string(),
            description: Some(message),
            stats: Vec::new(),
            output: Vec::new(),
            output_steps: Vec::new(),
            table: None,
            progress: None,
        }
    }

    /// 构造插件错误页面。
    fn plugin_error_page(message: String) -> PluginPage {
        PluginPage {
            title: "插件执行失败".to_string(),
            description: Some(message),
            stats: Vec::new(),
            output: Vec::new(),
            output_steps: Vec::new(),
            table: None,
            progress: None,
        }
    }

    /// 创建插件命令的初始进度。
    pub(in crate::app) fn initial_plugin_progress(
        message: String,
        total: Option<u64>,
        unit: &str,
    ) -> PluginCommandProgress {
        PluginCommandProgress {
            message,
            detail: Some("正在启动插件进程".to_string()),
            done: 0,
            total,
            unit: Some(unit.to_string()),
        }
    }

    /// 启动插件流式事件轮询任务。
    ///
    /// 业务意图：
    /// - 后台插件进程通过标准库通道传回进度和瀑布流输出；UI 线程定时批量取出，避免跨线程直接修改 GPUI 状态。
    /// - 代次不匹配时停止轮询，防止旧插件命令覆盖新命令窗口。
    pub(in crate::app) fn spawn_plugin_event_poller(
        &self,
        generation: usize,
        receiver: mpsc::Receiver<PluginCommandRuntimeEvent>,
        context: &mut Context<Self>,
    ) {
        context
            .spawn(async move |view, app| {
                loop {
                    app.background_executor()
                        .timer(Duration::from_millis(80))
                        .await;
                    let keep_polling = view
                        .update(app, |view, context| {
                            view.drain_plugin_runtime_events(generation, &receiver, context)
                        })
                        .unwrap_or(false);
                    if !keep_polling {
                        break;
                    }
                }
            })
            .detach();
    }

    /// 从通道取出插件运行时事件并刷新窗口。
    fn drain_plugin_runtime_events(
        &mut self,
        generation: usize,
        receiver: &mpsc::Receiver<PluginCommandRuntimeEvent>,
        context: &mut Context<Self>,
    ) -> bool {
        if self.plugins.active_command_generation != Some(generation) {
            return false;
        }
        let mut disconnected = false;
        let mut latest_progress = None;
        let mut runtime_events = Vec::new();
        loop {
            match receiver.try_recv() {
                Ok(PluginCommandRuntimeEvent::Progress(progress)) => {
                    latest_progress = Some(progress)
                }
                Ok(event) => runtime_events.push(event),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        if let Some(progress) = latest_progress {
            self.plugins.status_message = Some(progress.message.clone());
            if let Some(window_handle) = self.plugins.page_window {
                let _ = window_handle.update(context, |window_view, _window, context| {
                    window_view.set_progress(progress, context);
                });
            }
            context.notify();
        }
        if !runtime_events.is_empty() {
            if let Some(window_handle) = self.plugins.page_window {
                let _ = window_handle.update(context, |window_view, _window, context| {
                    for event in runtime_events {
                        window_view.apply_plugin_runtime_event(event, context);
                    }
                });
            }
            context.notify();
        }
        !disconnected
    }

    /// 返回 manifest 图标名对应的宿主图标。
    ///
    /// 业务意图：
    /// - 插件 manifest 只保存字符串，宿主需要把少量允许图标映射到已注册的 Lucide 字体，未知值回退到扩展图标。
    pub(in crate::app) fn plugin_menu_icon(icon_name: Option<&str>) -> Icon {
        match icon_name.unwrap_or_default() {
            "Search" => Icon::Search,
            "ChartNoAxesCombined" => Icon::ChartNoAxesCombined,
            "FolderOpen" => Icon::FolderOpen,
            "FileText" => Icon::FileText,
            "Settings" => Icon::Settings,
            "RefreshCw" => Icon::RefreshCw,
            "Trash2" => Icon::Trash2,
            _ => Icon::FileArchive,
        }
    }

    /// 从日志树右键菜单调用插件命令。
    ///
    /// 边界条件：
    /// - 没有候选日志时直接写入中文提示，不启动插件进程。
    /// - 插件运行放到后台执行器，避免第三方进程慢启动导致 UI 线程卡住。
    pub(in crate::app) fn invoke_log_tree_plugin_menu(
        &mut self,
        plugin_id: String,
        menu_id: String,
        command_id: String,
        files: Vec<PluginLogFile>,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if files.is_empty() {
            self.plugins.status_message = Some("没有可发送给插件的日志文件".to_string());
            return;
        }
        let Some(plugin) = self
            .plugins
            .definitions
            .iter()
            .find(|plugin| plugin.id == plugin_id && plugin.active())
            .cloned()
        else {
            self.plugins.status_message = Some("插件未启用或加载失败，无法执行命令".to_string());
            return;
        };
        let generation = self.plugins.begin_command_generation();
        let title = format!("{} 正在处理", plugin.name);
        let initial_progress = Self::initial_plugin_progress(
            format!("正在执行插件：{}", plugin.name),
            Some(files.len() as u64),
            "文件",
        );
        let (event_sender, event_receiver) = mpsc::channel();
        self.plugins.status_message = Some(initial_progress.message.clone());
        let origin_plugin = Some(plugin.clone());
        let origin_log_sources = Self::plugin_log_source_map(&files);
        let origin_log_files = files.clone();
        self.schedule_open_plugin_page_window_from_context(
            context.entity(),
            generation,
            title.clone(),
            Self::plugin_running_page(title, initial_progress),
            origin_plugin.clone(),
            origin_log_sources.clone(),
            origin_log_files.clone(),
            context,
        );
        self.spawn_plugin_event_poller(generation, event_receiver, context);
        let command_context = PluginCommandContext::LogTreeMenu { menu_id, files };
        let main_view = context.entity();
        context
            .spawn(async move |_view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move {
                        invoke_plugin_command_with_progress(
                            &plugin,
                            &command_id,
                            command_context,
                            Some(event_sender),
                        )
                    })
                    .await;
                app.update(move |app| {
                    Self::handle_plugin_command_result_after_main_update(
                        main_view,
                        generation,
                        origin_plugin,
                        origin_log_sources,
                        origin_log_files,
                        result,
                        app,
                    );
                })
                .ok();
            })
            .detach();
    }

    /// 从日志分析页工具栏调用插件命令。
    ///
    /// 业务意图：
    /// - 工具栏插件面向当前整棵日志树，需要扫描所有已加载路径，而不是右键选中项。
    /// - 宿主负责把日志树快照和插件设置合并后传入，插件不能自行访问未加载目录或读取全局配置。
    ///
    /// 边界条件：
    /// - 未加载日志树时不启动插件进程，避免用户误以为插件会扫描任意本地目录。
    /// - 插件设置读取失败会回退 manifest 默认值；保存设置时的错误在设置页展示，不影响工具栏入口可用性。
    pub(in crate::app) fn invoke_log_toolbar_plugin_action(
        &mut self,
        plugin_id: String,
        toolbar_id: String,
        command_id: String,
        _window: &mut Window,
        context: &mut Context<Self>,
    ) {
        let Some(plugin) = self
            .plugins
            .definitions
            .iter()
            .find(|plugin| plugin.id == plugin_id && plugin.active())
            .cloned()
        else {
            self.plugins.status_message = Some("插件未启用或加载失败，无法执行命令".to_string());
            return;
        };
        let Some(manifest) = plugin.manifest.as_ref() else {
            self.plugins.status_message = Some("插件 manifest 不可用，无法执行命令".to_string());
            return;
        };
        let toolbar_contribution = manifest
            .contributes
            .log_toolbar
            .iter()
            .find(|contribution| contribution.id == toolbar_id)
            .cloned();
        let settings = load_plugin_settings(manifest);
        let toolbar_snapshot_filter = toolbar_contribution
            .as_ref()
            .map(|contribution| {
                // 插件 manifest 声明哪些设置键可用于工具栏快照预过滤；宿主只读取对应通配规则，
                // 不解析设置键的业务含义，避免主程序固化某个插件的日志类型。
                contribution
                    .snapshot_filter_setting_keys
                    .iter()
                    .filter_map(|key| settings.get(key))
                    .map(String::as_str)
                    .filter(|patterns| !patterns.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(";")
            })
            .filter(|patterns| !patterns.trim().is_empty());
        let toolbar_single_file_archive_filter = toolbar_contribution
            .as_ref()
            .map(|contribution| {
                // 插件可额外声明哪些规则代表单文件压缩日志；宿主只使用这些通配规则决定是否跳过
                // 对应压缩包的目录展开，不关心该压缩包属于线程、业务还是其它日志类型。
                contribution
                    .snapshot_single_file_archive_setting_keys
                    .iter()
                    .filter_map(|key| settings.get(key))
                    .map(String::as_str)
                    .filter(|patterns| !patterns.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(";")
            })
            .filter(|patterns| !patterns.trim().is_empty());
        let (tree, toolbar_snapshot_cache_key, cached_toolbar_snapshot) = match &self.log.load_state
        {
            LogTreeLoadState::Loaded(tree_state) => {
                let cache_key = LoadedLogTreeState::plugin_toolbar_snapshot_cache_key(
                    &tree_state.tree,
                    toolbar_snapshot_filter.as_deref(),
                    toolbar_single_file_archive_filter.as_deref(),
                );
                let cached = self.plugins.toolbar_snapshot_cache.get(&cache_key).cloned();
                (tree_state.tree.clone(), cache_key, cached)
            }
            _ => {
                self.plugins.status_message = Some("请先加载日志，再执行插件分析".to_string());
                return;
            }
        };
        let generation = self.plugins.begin_command_generation();
        let title = toolbar_contribution
            .as_ref()
            .map(|contribution| contribution.title.clone())
            .unwrap_or_else(|| format!("{} 正在处理", plugin.name));
        let initial_progress = PluginCommandProgress {
            message: format!("正在收集日志树快照：{}", plugin.name),
            detail: Some("正在后台遍历当前加载的日志树和压缩包目录".to_string()),
            done: 0,
            total: None,
            unit: Some("条目".to_string()),
        };
        let initial_page = toolbar_contribution
            .as_ref()
            .and_then(|contribution| contribution.initial_step.clone())
            .map(|initial_step| Self::plugin_step_running_page(title.clone(), initial_step))
            .unwrap_or_else(|| Self::plugin_running_page(title.clone(), initial_progress.clone()));
        let (event_sender, event_receiver) = mpsc::channel();
        self.plugins.status_message = Some(initial_progress.message.clone());
        let origin_plugin = Some(plugin.clone());
        let initial_origin_log_files = Vec::new();
        self.schedule_open_plugin_page_window_from_context(
            context.entity(),
            generation,
            title.clone(),
            initial_page,
            origin_plugin.clone(),
            BTreeMap::new(),
            initial_origin_log_files,
            context,
        );
        self.spawn_plugin_event_poller(generation, event_receiver, context);
        let main_view = context.entity();
        let event_sender_for_snapshot = event_sender.clone();
        let plugin_name_for_worker = plugin.name.clone();
        context
            .spawn(async move |_view, app| {
                let (snapshot_for_cache, origin_log_sources, origin_log_files, result) = app
                    .background_executor()
                    .spawn(async move {
                        let (files, snapshot_for_cache) =
                            if let Some(cached_files) = cached_toolbar_snapshot {
                                let _ = event_sender_for_snapshot.send(
                                    PluginCommandRuntimeEvent::Progress(PluginCommandProgress {
                                        message: format!(
                                            "正在执行插件：{}",
                                            plugin_name_for_worker
                                        ),
                                        detail: Some(format!(
                                            "已复用日志树快照缓存，候选日志 {} 个",
                                            cached_files.len()
                                        )),
                                        done: 0,
                                        total: Some(cached_files.len() as u64),
                                        unit: Some("条目".to_string()),
                                    }),
                                );
                                (cached_files, None)
                            } else {
                                let _ = event_sender_for_snapshot.send(
                                    PluginCommandRuntimeEvent::Progress(PluginCommandProgress {
                                        message: format!(
                                            "正在收集日志树快照：{}",
                                            plugin_name_for_worker
                                        ),
                                        detail: Some(
                                            "正在后台遍历当前加载的日志树和压缩包目录".to_string(),
                                        ),
                                        done: 0,
                                        total: None,
                                        unit: Some("条目".to_string()),
                                    }),
                                );

                                // 日志树快照会读取压缩包目录元数据，尤其是线程日志目录下的大量
                                // `thread_*.zip`；放在后台线程执行，避免点击工具栏按钮后阻塞主窗口。
                                let files =
                                    LoadedLogTreeState::plugin_log_files_for_toolbar_tree_snapshot_with_filter(
                                        &tree,
                                        toolbar_snapshot_filter.as_deref(),
                                        toolbar_single_file_archive_filter.as_deref(),
                                    );
                                let snapshot_for_cache = Some(files.clone());
                                (files, snapshot_for_cache)
                            };
                        let origin_log_sources = Self::plugin_log_source_map(&files);
                        let origin_log_files = files.clone();
                        let _ = event_sender_for_snapshot.send(
                            PluginCommandRuntimeEvent::Progress(PluginCommandProgress {
                                message: format!("正在执行插件：{}", plugin_name_for_worker),
                                detail: Some(format!(
                                    "已收集 {} 个日志树条目，正在启动插件进程",
                                    files.len()
                                )),
                                done: 0,
                                total: Some(files.len() as u64),
                                unit: Some("条目".to_string()),
                            }),
                        );

                        let command_context = PluginCommandContext::LogToolbarAction {
                            toolbar_id,
                            files,
                            settings,
                        };
                        let allows_log_content = plugin.manifest.as_ref().is_some_and(|manifest| {
                            manifest
                                .permissions
                                .iter()
                                .any(|permission| permission == "logs.content")
                        });
                        let result = if allows_log_content {
                            let origin_log_sources_for_content = origin_log_sources.clone();
                            invoke_plugin_command_with_interactive_content_stream(
                                &plugin,
                                &command_id,
                                command_context,
                                Some(event_sender),
                                move |request, writer, event_sender| {
                                    PluginPageWindowView::write_requested_log_source_content_stream(
                                        request,
                                        &origin_log_sources_for_content,
                                        writer,
                                        event_sender,
                                    )
                                },
                            )
                        } else {
                            invoke_plugin_command_with_progress(
                                &plugin,
                                &command_id,
                                command_context,
                                Some(event_sender),
                            )
                        };
                        (snapshot_for_cache, origin_log_sources, origin_log_files, result)
                    })
                    .await;
                app.update(move |app| {
                    if let Some(files) = snapshot_for_cache {
                        let _ = main_view.update(app, |view, _context| {
                            view.plugins
                                .store_toolbar_snapshot_cache(toolbar_snapshot_cache_key, files);
                        });
                    }
                    Self::handle_plugin_command_result_after_main_update(
                        main_view,
                        generation,
                        origin_plugin,
                        origin_log_sources,
                        origin_log_files,
                        result,
                        app,
                    );
                })
                .ok();
            })
            .detach();
    }

    /// 从笔记树右键菜单调用插件命令。
    ///
    /// 业务意图：
    /// - 第一版只发送右键节点 ID、标题和类型，不读取笔记正文，也不提供写入接口，满足插件最小权限原则。
    pub(in crate::app) fn invoke_notes_tree_plugin_menu(
        &mut self,
        plugin_id: String,
        menu_id: String,
        command_id: String,
        target: PluginNoteTreeTarget,
        context: &mut Context<Self>,
    ) {
        let Some(plugin) = self
            .plugins
            .definitions
            .iter()
            .find(|plugin| plugin.id == plugin_id && plugin.active())
            .cloned()
        else {
            self.plugins.status_message = Some("插件未启用或加载失败，无法执行命令".to_string());
            return;
        };
        let generation = self.plugins.begin_command_generation();
        let title = format!("{} 正在处理", plugin.name);
        let initial_progress =
            Self::initial_plugin_progress(format!("正在执行插件：{}", plugin.name), None, "项");
        let (event_sender, event_receiver) = mpsc::channel();
        self.plugins.status_message = Some(initial_progress.message.clone());
        let origin_plugin = Some(plugin.clone());
        self.schedule_open_plugin_page_window_from_context(
            context.entity(),
            generation,
            title.clone(),
            Self::plugin_running_page(title, initial_progress),
            origin_plugin.clone(),
            BTreeMap::new(),
            Vec::new(),
            context,
        );
        self.spawn_plugin_event_poller(generation, event_receiver, context);
        let command_context = PluginCommandContext::NotesTreeMenu { menu_id, target };
        let main_view = context.entity();
        context
            .spawn(async move |_view, app| {
                let result = app
                    .background_executor()
                    .spawn(async move {
                        invoke_plugin_command_with_progress(
                            &plugin,
                            &command_id,
                            command_context,
                            Some(event_sender),
                        )
                    })
                    .await;
                app.update(move |app| {
                    Self::handle_plugin_command_result_after_main_update(
                        main_view,
                        generation,
                        origin_plugin,
                        BTreeMap::new(),
                        Vec::new(),
                        result,
                        app,
                    );
                })
                .ok();
            })
            .detach();
    }
}
