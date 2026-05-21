// 线程日志分析堆栈详情独立窗口。
//
// 业务意图：
// - 线程分析主窗口负责展示多快照时间线；完整线程堆栈较长，不适合塞进悬浮气泡或时间线单元。
// - 本窗口只消费 `ThreadTimelineCell` 中已经解析好的完整堆栈，点击色块时弹出；需要跳到主日志时由窗口内按钮显式触发。
//
// 边界条件：
// - 窗口不回读日志文件，因此压缩包成员、临时物化文件和大日志不会因为详情展示产生额外 I/O。
// - 当前只在同一线程名的已解析堆栈样本之间切换；如果分析结果被替换，窗口会展示空状态，避免继续误导用户查看旧堆栈。

use std::collections::{HashMap, HashSet};

use super::*;
use crate::highlighting::HighlightMode;

/// 线程堆栈详情窗口默认宽度。
pub(in crate::app) const THREAD_STACK_WINDOW_WIDTH: f32 = 860.0;
/// 线程堆栈详情窗口默认高度。
pub(in crate::app) const THREAD_STACK_WINDOW_HEIGHT: f32 = 640.0;
/// 线程堆栈详情窗口最小宽度。
pub(in crate::app) const THREAD_STACK_WINDOW_MIN_WIDTH: f32 = 560.0;
/// 线程堆栈详情窗口最小高度。
pub(in crate::app) const THREAD_STACK_WINDOW_MIN_HEIGHT: f32 = 360.0;
/// 线程堆栈详情列表行高。
const THREAD_STACK_WINDOW_ROW_HEIGHT: f32 = 22.0;
/// 线程堆栈详情行号列宽。
const THREAD_STACK_WINDOW_LINE_NUMBER_WIDTH: f32 = 44.0;
/// 线程堆栈详情正文左侧内边距。
const THREAD_STACK_WINDOW_TEXT_LEFT_PADDING: f32 = 8.0;
/// 线程堆栈详情正文右侧保留宽度。
const THREAD_STACK_WINDOW_TEXT_RIGHT_PADDING: f32 = 18.0;
/// 线程堆栈详情每行出现率标签预留宽度。
///
/// 业务意图：
/// - 出现率标签追加在堆栈文本后面，横向滚动范围必须提前为 `100%` 这类短标签保留空间。
/// - 这里只影响滚动范围估算，不改变真实文本排版，避免为所有行执行昂贵测量。
const THREAD_STACK_WINDOW_PRESENCE_LABEL_WIDTH: f32 = 46.0;
/// 线程堆栈详情等宽字体估算字号。
///
/// 业务意图：
/// - 详情窗口使用 `text_xs` 渲染堆栈文本，GPUI 的实际字号由主题文本样式决定。
/// - 横向滚动条需要在布局前知道最长行的大致宽度，因此这里用固定估算值为每一行设置最小宽度。
const THREAD_STACK_WINDOW_TEXT_FONT_SIZE: f32 = 12.0;
/// 线程堆栈详情等宽字符宽度估算比例。
///
/// 边界条件：
/// - 不做逐字符排版测量，避免为了滚动条在渲染热路径重复 shape 文本；估算只用于让长行撑出横向滚动范围。
const THREAD_STACK_WINDOW_MONOSPACE_WIDTH_RATIO: f32 = 0.62;

/// 线程堆栈详情滚动条拖动状态。
///
/// 业务意图：
/// - 详情窗口只有一个堆栈正文列表，不需要 tab 维度；拖动状态只保存轴向和鼠标按下点在滑块内的偏移。
/// - 状态独立于线程分析主窗口，避免详情窗口拖动滚动条时误影响主时间线窗口。
#[derive(Clone, Copy)]
pub(in crate::app) struct ThreadStackScrollbarDrag {
    /// 当前拖动的滚动轴。
    axis: LogScrollbarAxis,
    /// 鼠标按下点相对滑块起点的偏移。
    cursor_offset: Pixels,
}

/// 线程堆栈详情中一行可展示内容。
///
/// 业务意图：
/// - 详情窗口每行除原始堆栈文本外，还需要展示该行在“命中当前线程的日志文件”中的出现率。
/// - 将百分比和文本提前配对，虚拟列表渲染时只负责绘制当前可见行，不在热路径重复扫描全部堆栈样本。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct ThreadStackDisplayLine {
    /// 原始堆栈行文本。
    pub(in crate::app) text: String,
    /// 当前行在命中该线程的不同日志来源中的出现百分比。
    ///
    /// 边界条件：
    /// - 百分比已经夹紧到 `0..=100`，渲染层可以直接格式化为用户可见文案。
    pub(in crate::app) presence_percent: u8,
}

/// 线程日志分析堆栈详情窗口。
///
/// 业务意图：
/// - 该窗口展示同一线程在选中日志快照中的完整堆栈列表，并通过左右按钮切换当前堆栈。
/// - 主日志跳转必须由右上角按钮显式触发，避免用户只是查看堆栈时主窗口自动切换日志。
pub(in crate::app) struct ThreadStackWindowView {
    /// 主窗口视图实体，用于同步主题。
    main_view: Entity<MainView>,
    /// 当前可切换的同名线程堆栈样本。
    ///
    /// 业务意图：
    /// - 列表按分析矩阵中的快照顺序保存，保证左右切换和时间线横轴顺序一致。
    stacks: Vec<Arc<ThreadTimelineCell>>,
    /// 当前展示的堆栈下标。
    active_index: usize,
    /// 堆栈正文虚拟列表滚动句柄。
    stack_scroll_handle: UniformListScrollHandle,
    /// 堆栈详情窗口焦点句柄。
    ///
    /// 业务意图：
    /// - 堆栈正文不是系统文本控件，点击后必须主动获取焦点，`Ctrl/Cmd+C` 和 `Ctrl/Cmd+A` 才能落到当前详情窗口而不是主窗口。
    /// - 焦点只服务本窗口生命周期，不写入主视图状态，避免多个详情窗口之间互相污染键盘事件。
    stack_focus_handle: gpui::FocusHandle,
    /// 当前详情正文的只读文本选择范围。
    ///
    /// 业务意图：
    /// - 线程堆栈详情使用虚拟列表绘制，GPUI 不会自动提供跨行选区；这里复用日志正文的行列模型，让复制、全选和高亮规则一致。
    /// - 选择绑定到当前详情窗口，切换堆栈样本时清空，避免旧行列套用到新堆栈文本。
    text_selection: Option<LogTextSelection>,
    /// 当前鼠标拖拽选择的固定起点。
    ///
    /// 边界条件：
    /// - 鼠标释放、切换堆栈、关闭窗口或拖动滚动条时必须清空；最终选区仍保留在 `text_selection` 中供复制使用。
    selection_drag_anchor: Option<LogTextPosition>,
    /// 堆栈正文自绘滚动条拖动状态。
    ///
    /// 边界条件：
    /// - 切换堆栈、释放鼠标、鼠标拖动状态丢失或内容不再可滚动时必须清空，避免后续鼠标移动继续改变正文滚动位置。
    stack_scrollbar_drag: Option<ThreadStackScrollbarDrag>,
    /// 主窗口状态变更订阅。
    _main_view_subscription: gpui::Subscription,
}

impl ThreadStackWindowView {
    /// 创建线程堆栈详情窗口。
    ///
    /// 边界条件：
    /// - 初始下标会被夹紧到样本范围内；空样本用于分析结果替换后的空状态，不应触发 panic。
    pub(in crate::app) fn new(
        main_view: Entity<MainView>,
        stacks: Vec<Arc<ThreadTimelineCell>>,
        active_index: usize,
        context: &mut Context<Self>,
    ) -> Self {
        let observed_main_view = main_view.clone();
        let main_view_subscription = context.observe(&observed_main_view, |_, _, context| {
            context.notify();
        });
        let active_index = Self::clamped_active_index(&stacks, active_index);
        Self {
            main_view,
            stacks,
            active_index,
            stack_scroll_handle: UniformListScrollHandle::new(),
            stack_focus_handle: context.focus_handle(),
            text_selection: None,
            selection_drag_anchor: None,
            stack_scrollbar_drag: None,
            _main_view_subscription: main_view_subscription,
        }
    }

    /// 替换当前窗口展示的堆栈集合。
    ///
    /// 业务意图：
    /// - 用户点击其它线程色块时复用已有详情窗口，直接切换为新线程的堆栈集合并激活窗口。
    /// - 堆栈切换后重置滚动位置，避免上一段长堆栈的滚动偏移套用到新堆栈导致首行不可见。
    pub(in crate::app) fn update_stacks(
        &mut self,
        stacks: Vec<Arc<ThreadTimelineCell>>,
        active_index: usize,
        context: &mut Context<Self>,
    ) {
        self.active_index = Self::clamped_active_index(&stacks, active_index);
        self.stacks = stacks;
        self.stack_scroll_handle = UniformListScrollHandle::new();
        self.text_selection = None;
        self.selection_drag_anchor = None;
        self.stack_scrollbar_drag = None;
        context.notify();
    }

    /// 返回当前窗口标题。
    ///
    /// 业务意图：
    /// - 标题栏展示线程名，用户同时打开主分析窗口和详情窗口时可以快速区分上下文。
    pub(in crate::app) fn window_title_for_cell(cell: Option<&Arc<ThreadTimelineCell>>) -> String {
        cell.map(|cell| format!("线程堆栈 - {}", cell.thread_name))
            .unwrap_or_else(|| "线程堆栈".to_string())
    }

    /// 聚焦线程堆栈详情正文。
    ///
    /// 业务意图：
    /// - 窗口打开或复用后，用户应能直接使用 `Ctrl/Cmd+A` 全选和 `Ctrl/Cmd+C` 复制，不必先额外点击正文。
    /// - 对外只暴露“聚焦正文”这个行为，不暴露内部 `FocusHandle` 字段，避免其它模块依赖详情窗口的实现细节。
    pub(in crate::app) fn focus_stack_body(&self, window: &mut Window) {
        window.focus(&self.stack_focus_handle);
    }

    /// 返回当前展示的线程堆栈样本。
    fn current_cell(&self) -> Option<&Arc<ThreadTimelineCell>> {
        self.stacks.get(self.active_index)
    }

    /// 将活动下标夹紧到样本范围内。
    ///
    /// 边界条件：
    /// - 空列表固定返回 0，渲染层会展示空状态；非空列表返回合法下标。
    pub(in crate::app) fn clamped_active_index(
        stacks: &[Arc<ThreadTimelineCell>],
        active_index: usize,
    ) -> usize {
        active_index.min(stacks.len().saturating_sub(1))
    }

    /// 生成当前线程堆栈的完整展示行。
    ///
    /// 业务意图：
    /// - 详情窗口必须优先展示完整 `stack_lines`；如果旧数据缺失完整片段，则回退到悬浮气泡预览，保证窗口仍有可读内容。
    pub(in crate::app) fn thread_stack_lines_for_cell(cell: &ThreadTimelineCell) -> Vec<String> {
        if !cell.stack_lines.is_empty() {
            return cell.stack_lines.clone();
        }
        if !cell.preview_lines.is_empty() {
            return cell.preview_lines.clone();
        }
        vec!["未识别到线程堆栈内容".to_string()]
    }

    /// 生成当前活动堆栈的展示行，并附加跨文件出现率。
    ///
    /// 业务意图：
    /// - 用户需要知道某个栈帧是所有同名线程样本都存在的共性，还是只在少数日志文件中出现的差异。
    /// - 分母使用“命中当前线程的不同日志来源数”，同一日志文件中多个快照只算一个来源，符合用户按文件排查的视角。
    ///
    /// 边界条件：
    /// - 空样本返回一行空状态文本，百分比固定为 100%，避免除零和空窗口。
    /// - 行匹配使用归一化后的文本：忽略缩进，归一化 `0x...` 地址，并把线程头归一到线程名，减少线程号、nid、对象地址变化带来的误差。
    pub(in crate::app) fn thread_stack_display_lines_for_active_stack(
        stacks: &[Arc<ThreadTimelineCell>],
        active_index: usize,
    ) -> Vec<ThreadStackDisplayLine> {
        let Some(active_cell) = stacks.get(Self::clamped_active_index(stacks, active_index)) else {
            return vec![ThreadStackDisplayLine {
                text: "没有可展示的线程堆栈".to_string(),
                presence_percent: 100,
            }];
        };
        let active_lines = Self::thread_stack_lines_for_cell(active_cell);
        let denominator = stacks
            .iter()
            .map(|cell| cell.source.stable_key())
            .collect::<HashSet<_>>()
            .len()
            .max(1);
        let sources_by_line = Self::thread_stack_presence_sources_by_line(stacks);

        active_lines
            .into_iter()
            .map(|line| {
                let presence_key =
                    Self::normalized_stack_line_presence_key(&line, &active_cell.thread_name);
                let source_count = sources_by_line
                    .get(&presence_key)
                    .map(HashSet::len)
                    .unwrap_or(1);
                ThreadStackDisplayLine {
                    text: line,
                    presence_percent: Self::stack_line_presence_percent(source_count, denominator),
                }
            })
            .collect()
    }

    /// 按归一化堆栈行统计出现过该行的日志来源集合。
    ///
    /// 业务意图：
    /// - 该统计只在当前线程的堆栈样本集合内进行，不跨线程扫描，避免业务线程名称相同以外的数据污染百分比。
    /// - `HashSet` 以来源稳定键去重，同一文件内多个快照命中同一行仍只按一个文件计算。
    fn thread_stack_presence_sources_by_line(
        stacks: &[Arc<ThreadTimelineCell>],
    ) -> HashMap<String, HashSet<String>> {
        let mut sources_by_line = HashMap::<String, HashSet<String>>::new();
        for cell in stacks {
            let source_key = cell.source.stable_key();
            for line in Self::thread_stack_lines_for_cell(cell) {
                let presence_key =
                    Self::normalized_stack_line_presence_key(&line, &cell.thread_name);
                sources_by_line
                    .entry(presence_key)
                    .or_default()
                    .insert(source_key.clone());
            }
        }
        sources_by_line
    }

    /// 生成堆栈行出现率统计使用的稳定键。
    ///
    /// 业务意图：
    /// - Java thread dump 中线程头、对象地址和本地线程 ID 常带有运行时动态值，直接逐字匹配会把同一语义行误判为不同。
    /// - 这里做轻量归一化即可满足百分比提示，不改变窗口中展示的原始文本。
    ///
    /// 边界条件：
    /// - 线程头按当前线程名归一化，保证 `#290`、`tid=0x...` 等差异不会降低第一行出现率。
    /// - 非线程头只折叠空白并把十六进制地址归一化为 `0x*`，避免过度解析 Java 栈帧格式。
    pub(in crate::app) fn normalized_stack_line_presence_key(
        line: &str,
        thread_name: &str,
    ) -> String {
        let trimmed = line.trim();
        let thread_header_prefix = format!("\"{thread_name}\"");
        if trimmed.starts_with(&thread_header_prefix) {
            return format!("thread-header:{thread_name}");
        }
        Self::replace_hex_literals_for_presence_key(trimmed)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// 把日志行中的十六进制动态值归一化。
    ///
    /// 业务意图：
    /// - `tid=0x...`、`nid=0x...` 和 `- locked <0x...>` 这类值在不同快照或文件里通常不同，但它们不影响栈帧语义。
    /// - 手写线性扫描即可处理常见 ASCII 片段，不引入正则依赖，也不会产生回溯性能风险。
    fn replace_hex_literals_for_presence_key(text: &str) -> String {
        let mut normalized = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(character) = chars.next() {
            if character == '0' && matches!(chars.peek(), Some('x' | 'X')) {
                normalized.push_str("0x*");
                let _ = chars.next();
                while matches!(chars.peek(), Some(next) if next.is_ascii_hexdigit()) {
                    let _ = chars.next();
                }
            } else {
                normalized.push(character);
            }
        }
        normalized
    }

    /// 将命中文件数换算成百分比。
    ///
    /// 边界条件：
    /// - 分母为 0 时按 100% 处理，避免空样本或异常数据导致除零；正常路径分母至少为 1。
    /// - 使用四舍五入，确保 1/5 显示为 20%，2/3 显示为 67%，更符合用户直觉。
    pub(in crate::app) fn stack_line_presence_percent(
        source_count: usize,
        denominator: usize,
    ) -> u8 {
        if denominator == 0 {
            return 100;
        }
        (((source_count.min(denominator) * 100) + denominator / 2) / denominator).min(100) as u8
    }

    /// 选择上一个或下一个堆栈样本。
    ///
    /// 边界条件：
    /// - 到达首尾时保持当前下标不变；空列表不响应导航。
    fn select_stack_by_delta(
        &mut self,
        delta: isize,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.stacks.is_empty() {
            return;
        }
        let max_index = self.stacks.len().saturating_sub(1);
        let next_index = if delta.is_negative() {
            self.active_index.saturating_sub(delta.unsigned_abs())
        } else {
            self.active_index
                .saturating_add(delta as usize)
                .min(max_index)
        };
        if next_index == self.active_index {
            return;
        }
        self.active_index = next_index;
        self.stack_scroll_handle = UniformListScrollHandle::new();
        self.stack_scrollbar_drag = None;
        self.text_selection = None;
        self.selection_drag_anchor = None;
        window.set_window_title(&Self::window_title_for_cell(self.current_cell()));
        context.notify();
    }

    /// 计算线程堆栈详情正文的估算内容宽度。
    ///
    /// 业务意图：
    /// - GPUI 虚拟列表只有可见行参与布局；如果最长堆栈行暂时不在视口中，横向滚动条可能无法立即出现。
    /// - 这里按全部已解析堆栈行估算最小宽度，并应用到每一行，让横向滚动范围在首帧后稳定可用。
    ///
    /// 边界条件：
    /// - 制表符按 4 列展开，和日志正文展示的常用宽度保持一致。
    /// - 中文等宽度可能与 ASCII 不完全一致；估算只决定滚动范围，真实字符仍由 GPUI 文本系统绘制。
    pub(in crate::app) fn estimated_stack_content_width(lines: &[String]) -> f32 {
        let max_columns = lines
            .iter()
            .map(|line| Self::stack_line_display_columns(line))
            .max()
            .unwrap_or(0);
        let char_width = (THREAD_STACK_WINDOW_TEXT_FONT_SIZE
            * THREAD_STACK_WINDOW_MONOSPACE_WIDTH_RATIO)
            .max(1.0);
        THREAD_STACK_WINDOW_LINE_NUMBER_WIDTH
            + THREAD_STACK_WINDOW_TEXT_LEFT_PADDING
            + THREAD_STACK_WINDOW_TEXT_RIGHT_PADDING
            + THREAD_STACK_WINDOW_PRESENCE_LABEL_WIDTH
            + max_columns as f32 * char_width
    }

    /// 估算单行堆栈文本显示列数。
    ///
    /// 业务意图：
    /// - 横向滚动条只需要稳定估算，不需要为每个字符创建布局对象；按字符扫描可以避免额外依赖和主线程排版成本。
    pub(in crate::app) fn stack_line_display_columns(line: &str) -> usize {
        line.chars().fold(0usize, |columns, character| {
            if character == '\t' {
                columns + (4 - columns % 4)
            } else {
                columns + 1
            }
        })
    }

    /// 返回当前活动堆栈的原始文本行。
    ///
    /// 业务意图：
    /// - 选区复制和全选必须基于真实堆栈文本，而不是基于带出现率标签的展示节点，避免把辅助百分比复制到诊断内容里。
    fn active_stack_text_lines(&self) -> Vec<String> {
        if self.current_cell().is_some() {
            Self::thread_stack_display_lines_for_active_stack(&self.stacks, self.active_index)
                .into_iter()
                .map(|line| line.text)
                .collect()
        } else {
            vec!["没有可展示的线程堆栈".to_string()]
        }
    }

    /// 从给定堆栈行和选择范围中提取可复制文本。
    ///
    /// 业务意图：
    /// - 详情窗口、单元测试和未来右键菜单都需要同一套选择截取规则，避免屏幕高亮和剪贴板内容不一致。
    /// - 该函数只处理纯文本数据，不依赖 GPUI 窗口，便于覆盖中文、跨行和空选区等边界。
    ///
    /// 边界条件：
    /// - 选区端点会按实际行数和字符数夹紧，避免旧选区在堆栈切换后造成越界。
    /// - 跨行选择保留 `\n`，但每行内部仍按 UTF-8 字节边界截取，中文不会被切坏。
    pub(in crate::app) fn selected_stack_text_from_lines(
        lines: &[String],
        selection: &LogTextSelection,
    ) -> Option<String> {
        if selection.is_empty() || lines.is_empty() {
            return None;
        }

        let (start, end) = selection.normalized();
        if start.line_index >= lines.len() {
            return None;
        }
        let end_line_index = end.line_index.min(lines.len().saturating_sub(1));
        if start.line_index > end_line_index {
            return None;
        }

        let mut selected_text = String::new();
        for line_index in start.line_index..=end_line_index {
            if line_index > start.line_index {
                selected_text.push('\n');
            }
            let line = &lines[line_index];
            let Some((start_column, end_column)) =
                MainView::selection_columns_for_line(selection, line_index, line)
            else {
                continue;
            };
            let start_byte = MainView::byte_index_for_char_column(line, start_column);
            let end_byte = MainView::byte_index_for_char_column(line, end_column);
            if start_byte < end_byte {
                selected_text.push_str(&line[start_byte..end_byte]);
            }
        }

        (!selected_text.is_empty()).then_some(selected_text)
    }

    /// 构造覆盖全部堆栈文本的选择范围。
    ///
    /// 业务意图：
    /// - `Ctrl/Cmd+A` 应只选中当前堆栈正文，不包含标题、按钮、行号或出现率标签，方便用户直接复制完整线程堆栈。
    ///
    /// 边界条件：
    /// - 空行集合返回 `None`；最后一行为空时焦点列为 0，仍可表达完整跨行选择。
    pub(in crate::app) fn full_stack_selection_for_lines(
        lines: &[String],
    ) -> Option<LogTextSelection> {
        let last_line_index = lines.len().checked_sub(1)?;
        let last_column = lines[last_line_index].chars().count();
        Some(LogTextSelection {
            anchor: LogTextPosition {
                line_index: 0,
                column: 0,
            },
            focus: LogTextPosition {
                line_index: last_line_index,
                column: last_column,
            },
        })
    }

    /// 复制当前详情窗口选中的堆栈文本。
    ///
    /// 业务意图：
    /// - 线程详情窗口是只读查看器，复制操作只应在存在非空选区时写入剪贴板，避免拦截其它控件的复制快捷键。
    fn copy_selected_stack_text(&self, context: &mut Context<Self>) -> bool {
        let Some(selection) = self.text_selection.as_ref() else {
            return false;
        };
        let Some(text) =
            Self::selected_stack_text_from_lines(&self.active_stack_text_lines(), selection)
        else {
            return false;
        };
        context.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// 选中当前堆栈正文的全部文本。
    ///
    /// 业务意图：
    /// - 用户通常会在详情窗口里一键复制完整堆栈；全选只作用于正文，不影响主窗口日志选区和其它输入框。
    fn select_all_stack_text(&mut self, context: &mut Context<Self>) -> bool {
        let lines = self.active_stack_text_lines();
        let Some(selection) = Self::full_stack_selection_for_lines(&lines) else {
            return false;
        };
        self.text_selection = Some(selection);
        self.selection_drag_anchor = None;
        context.notify();
        true
    }

    /// 把鼠标横坐标换算为详情正文中的字符位置。
    ///
    /// 业务意图：
    /// - 详情正文和主日志正文一样是自绘虚拟列表，鼠标命中必须同时考虑行号列、正文内边距和横向滚动偏移。
    /// - 使用 GPUI 文本系统 shape 当前行，可以让中文、空格和平台字体渲染差异下的选择位置尽量贴近屏幕文本。
    ///
    /// 边界条件：
    /// - 指针落在正文左侧时归到第 0 列，落到行尾右侧时归到最后一列。
    /// - 当前堆栈详情不展开制表符，命中和复制都基于原始堆栈行文本，避免视觉文本与复制文本不一致。
    fn stack_text_position_from_pointer(
        &self,
        line_index: usize,
        line: &str,
        pointer_x: Pixels,
        window: &mut Window,
    ) -> LogTextPosition {
        let scroll_state = self.stack_scroll_handle.0.borrow();
        let bounds = scroll_state.base_handle.bounds();
        if bounds.size.width <= px(0.0) {
            return LogTextPosition {
                line_index,
                column: 0,
            };
        }

        let text_origin_x = bounds.left()
            + scroll_state.base_handle.offset().x
            + px(THREAD_STACK_WINDOW_LINE_NUMBER_WIDTH + THREAD_STACK_WINDOW_TEXT_LEFT_PADDING);
        let text_relative_x = pointer_x - text_origin_x;
        if line.is_empty() || text_relative_x <= px(0.0) {
            return LogTextPosition {
                line_index,
                column: 0,
            };
        }

        let mut text_style = window.text_style();
        text_style.font_family = LOG_VIEWER_FONT_FAMILY.into();
        text_style.font_size = px(THREAD_STACK_WINDOW_TEXT_FONT_SIZE).into();
        let run = TextRun {
            len: line.len(),
            font: text_style.font(),
            color: text_style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let shaped_line = window.text_system().shape_line(
            SharedString::from(line.to_string()),
            font_size,
            &[run],
            None,
        );
        let byte_index = shaped_line.closest_index_for_x(text_relative_x);
        LogTextPosition {
            line_index,
            column: MainView::char_column_for_byte_index(line, byte_index),
        }
    }

    /// 开始线程堆栈详情正文选区。
    ///
    /// 业务意图：
    /// - 点击正文后把焦点切到详情窗口，使复制和全选快捷键处理当前堆栈，而不是透传给主日志窗口。
    /// - 双击和三连击复用主日志正文的选词、整行规则，减少同一类只读文本控件的交互差异。
    fn start_stack_text_selection(
        &mut self,
        line_index: usize,
        line: &str,
        event: &MouseDownEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.stack_scrollbar_drag.is_some() {
            return;
        }
        window.focus(&self.stack_focus_handle);
        let position =
            self.stack_text_position_from_pointer(line_index, line, event.position.x, window);
        let selection = match event.click_count {
            0 | 1 => LogTextSelection {
                anchor: position,
                focus: position,
            },
            2 => MainView::word_selection_for_position(line_index, line, position).unwrap_or(
                LogTextSelection {
                    anchor: position,
                    focus: position,
                },
            ),
            _ => MainView::line_selection_for_line(line_index, line),
        };
        self.text_selection = Some(selection);
        self.selection_drag_anchor = (event.click_count <= 1).then_some(position);
        context.notify();
    }

    /// 根据鼠标拖动更新线程堆栈详情正文选区。
    ///
    /// 边界条件：
    /// - 拖动滚动条时必须停止文本拖选，避免同一次鼠标动作同时滚动和修改选区。
    /// - 鼠标释放后保留最终选区，但清空拖动锚点。
    fn update_stack_text_selection(
        &mut self,
        line_index: usize,
        line: &str,
        event: &MouseMoveEvent,
        window: &mut Window,
        context: &mut Context<Self>,
    ) {
        if self.stack_scrollbar_drag.is_some() {
            self.stop_stack_text_selection(context);
            return;
        }
        if !event.dragging() {
            self.stop_stack_text_selection(context);
            return;
        }
        let Some(anchor) = self.selection_drag_anchor else {
            return;
        };
        let focus =
            self.stack_text_position_from_pointer(line_index, line, event.position.x, window);
        self.text_selection = Some(LogTextSelection { anchor, focus });
        context.notify();
    }

    /// 结束线程堆栈详情正文拖选。
    fn stop_stack_text_selection(&mut self, context: &mut Context<Self>) {
        if self.selection_drag_anchor.take().is_some() {
            context.notify();
        }
    }

    /// 处理详情窗口键盘快捷键。
    ///
    /// 业务意图：
    /// - 详情窗口根节点只处理只读文本查看器需要的快捷键：复制和全选，其它按键继续交给平台或父级默认行为。
    fn handle_stack_key_down(&mut self, event: &KeyDownEvent, context: &mut Context<Self>) {
        if MainView::is_copy_keystroke(&event.keystroke) {
            if self.copy_selected_stack_text(context) {
                context.stop_propagation();
            }
            return;
        }
        if MainView::is_select_all_keystroke(&event.keystroke) {
            if self.select_all_stack_text(context) {
                context.stop_propagation();
            }
        }
    }

    /// 从主窗口打开当前堆栈对应的日志位置。
    ///
    /// 业务意图：
    /// - 线程分析色块点击不再自动跳转主窗口；用户确认需要查看原始日志时，通过右上角按钮显式打开对应来源和行号。
    /// - 打开后激活主窗口，方便用户立即检查原始日志上下文。
    ///
    /// 边界条件：
    /// - 当前窗口为空状态时忽略按钮事件，避免分析结果被替换后误打开旧来源。
    fn open_current_stack_in_main_window(&mut self, context: &mut Context<Self>) {
        let Some(cell) = self.current_cell().cloned() else {
            return;
        };
        let source = cell.source.clone();
        let line_index = cell.line_index;
        let main_window = self.main_view.update(context, |view, context| {
            view.open_log_source_at_line(source, line_index, context);
            view.main_window
        });
        if let Some(main_window) = main_window {
            let _ = main_window.update(context, |_view, window, _context| {
                window.activate_window();
            });
        }
    }

    /// 渲染堆栈切换按钮。
    ///
    /// 业务意图：
    /// - 左右按钮使用固定命中区域，切换时不改变标题区布局；禁用态保留占位避免按钮跳动。
    fn render_navigation_button(
        &self,
        id: &'static str,
        icon: Icon,
        enabled: bool,
        delta: isize,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .w(px(30.0))
            .h(px(30.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(if enabled {
                palette.text
            } else {
                palette.muted_text
            }))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
            })
            .when(!enabled, |button| button.opacity(0.45))
            .child(MainView::render_lucide_icon(
                Some(icon),
                16.0,
                16.0,
                if enabled {
                    palette.text
                } else {
                    palette.muted_text
                },
            ))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, window, context| {
                    if enabled {
                        view.select_stack_by_delta(delta, window, context);
                    }
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染从主窗口打开当前日志位置的按钮。
    ///
    /// 业务意图：
    /// - 该按钮是详情窗口唯一会改变主日志窗口的入口；文本说明明确，避免和左右堆栈切换按钮混淆。
    /// - 空状态下按钮禁用但保留宽度，避免分析结果替换时标题区布局跳动。
    fn render_open_in_main_button(
        &self,
        enabled: bool,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id("thread-stack-open-in-main")
            .flex()
            .items_center()
            .justify_center()
            .gap_1()
            .h(px(30.0))
            .px_2()
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(if enabled {
                palette.text
            } else {
                palette.muted_text
            }))
            .when(enabled, |button| {
                button
                    .cursor_pointer()
                    .hover(move |button| button.bg(rgb(palette.hover)))
            })
            .when(!enabled, |button| button.opacity(0.45))
            .child(MainView::render_lucide_icon(
                Some(Icon::ExternalLink),
                14.0,
                14.0,
                if enabled {
                    palette.text
                } else {
                    palette.muted_text
                },
            ))
            .child("主窗口打开")
            .on_mouse_down(
                MouseButton::Left,
                context.listener(move |view, _event: &MouseDownEvent, _window, context| {
                    if enabled {
                        view.open_current_stack_in_main_window(context);
                    }
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染标题和导航区。
    ///
    /// 业务意图：
    /// - 顶部同时展示线程名、来源、时间和当前序号，让用户切换堆栈时不需要回看主时间线确认位置。
    fn render_header(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let current_cell = self.current_cell();
        let thread_title = current_cell
            .map(|cell| cell.thread_name.clone())
            .unwrap_or_else(|| "未选择线程堆栈".to_string());
        let subtitle = current_cell
            .map(|cell| {
                let thread_id = cell.thread_id.as_deref().unwrap_or("未识别 ID");
                format!(
                    "{} · {} · {} · 第 {} / {} 个堆栈 · 行 {}",
                    cell.source.display_name(),
                    cell.time_label,
                    thread_id,
                    self.active_index + 1,
                    self.stacks.len(),
                    cell.line_index + 1
                )
            })
            .unwrap_or_else(|| "当前分析结果已替换，请重新点击线程色块".to_string());
        let can_go_previous = self.active_index > 0 && !self.stacks.is_empty();
        let can_go_next = self.active_index + 1 < self.stacks.len();

        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.panel))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(palette.text))
                            .truncate()
                            .child(thread_title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_text))
                            .truncate()
                            .child(subtitle),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.render_open_in_main_button(
                        current_cell.is_some(),
                        palette,
                        context,
                    ))
                    .child(self.render_navigation_button(
                        "thread-stack-previous",
                        Icon::ChevronLeft,
                        can_go_previous,
                        -1,
                        palette,
                        context,
                    ))
                    .child(self.render_navigation_button(
                        "thread-stack-next",
                        Icon::ChevronRight,
                        can_go_next,
                        1,
                        palette,
                        context,
                    )),
            )
    }

    /// 渲染完整堆栈正文。
    ///
    /// 业务意图：
    /// - 堆栈正文使用虚拟列表，避免极长线程片段一次性创建大量行元素。
    /// - 行号帮助用户和主日志窗口中的真实位置对照；文本使用日志等宽字体保持堆栈缩进。
    /// - 正文容器不保留外侧内边距，让行号背景和代码内容贴齐窗口内容区边缘，避免详情窗口里出现额外留白。
    fn render_stack_body(
        &self,
        palette: AppThemePalette,
        syntax_theme: SyntaxTheme,
        context: &mut Context<Self>,
    ) -> impl IntoElement {
        let display_lines = if self.current_cell().is_some() {
            Self::thread_stack_display_lines_for_active_stack(&self.stacks, self.active_index)
        } else {
            vec![ThreadStackDisplayLine {
                text: "没有可展示的线程堆栈".to_string(),
                presence_percent: 100,
            }]
        };
        let stack_count = display_lines.len();
        let stack_text_lines = display_lines
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        let estimated_content_width = Self::estimated_stack_content_width(&stack_text_lines);
        let stack_handle = self.stack_scroll_handle.clone();
        let text_selection = self.text_selection.clone();
        let suppress_hover =
            self.stack_scrollbar_drag.is_some() || self.selection_drag_anchor.is_some();

        div()
            .relative()
            .flex_1()
            .min_h(px(0.0))
            .overflow_hidden()
            .child(
                uniform_list(
                    "thread-stack-detail-list",
                    stack_count,
                    context.processor(
                        move |_view, range: std::ops::Range<usize>, _window, context| {
                            range
                                .filter_map(|index| {
                                    display_lines.get(index).cloned().map(|line| (index, line))
                                })
                                .map(|(index, display_line)| {
                                    let line = display_line.text;
                                    let line_for_mouse_down = line.clone();
                                    let line_for_mouse_move = line.clone();
                                    let presence_label =
                                        format!("{}%", display_line.presence_percent);
                                    let mut highlights = highlight_line(
                                        HighlightMode::JavaThread,
                                        &line,
                                        None,
                                        syntax_theme,
                                    );
                                    if let Some(selection) = &text_selection
                                        && let Some(range) = MainView::selected_byte_range_for_line(
                                            selection, index, &line,
                                        )
                                    {
                                        // 线程详情正文和主日志正文使用同一种选区覆盖规则，避免选区与语法高亮重叠时出现背景冲突。
                                        highlights =
                                            MainView::combine_log_highlights_with_selection(
                                                highlights, range, palette,
                                            );
                                    }
                                    div()
                                        .h(px(THREAD_STACK_WINDOW_ROW_HEIGHT))
                                        .w_full()
                                        .min_w(px(estimated_content_width))
                                        .flex()
                                        .items_center()
                                        .text_xs()
                                        .font_family(LOG_VIEWER_FONT_FAMILY)
                                        .when(!suppress_hover, |row| {
                                            row.hover(move |row| row.bg(rgb(palette.hover)))
                                        })
                                        .child(
                                            div()
                                                .w(px(THREAD_STACK_WINDOW_LINE_NUMBER_WIDTH))
                                                .flex_none()
                                                .h_full()
                                                .flex()
                                                .items_center()
                                                .justify_end()
                                                .pr_2()
                                                .border_r_1()
                                                .border_color(rgb(palette.border))
                                                .bg(rgb(palette.panel))
                                                .text_color(rgb(palette.muted_text))
                                                .text_right()
                                                .child((index + 1).to_string()),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .h_full()
                                                .flex()
                                                .items_center()
                                                .pl(px(THREAD_STACK_WINDOW_TEXT_LEFT_PADDING))
                                                .pr(px(THREAD_STACK_WINDOW_TEXT_RIGHT_PADDING))
                                                .text_color(rgb(palette.text))
                                                .whitespace_nowrap()
                                                .child(
                                                    StyledText::new(line)
                                                        .with_highlights(highlights),
                                                )
                                                .child(
                                                    div()
                                                        .ml_2()
                                                        .flex_none()
                                                        .text_size(px(10.0))
                                                        .text_color(rgb(palette.muted_text))
                                                        .opacity(0.72)
                                                        .child(presence_label),
                                                ),
                                        )
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            context.listener(
                                                move |view,
                                                      event: &MouseDownEvent,
                                                      window,
                                                      context| {
                                                    view.start_stack_text_selection(
                                                        index,
                                                        &line_for_mouse_down,
                                                        event,
                                                        window,
                                                        context,
                                                    );
                                                    context.stop_propagation();
                                                },
                                            ),
                                        )
                                        .on_mouse_move(context.listener(
                                            move |view,
                                                  event: &MouseMoveEvent,
                                                  window,
                                                  context| {
                                                view.update_stack_text_selection(
                                                    index,
                                                    &line_for_mouse_move,
                                                    event,
                                                    window,
                                                    context,
                                                );
                                            },
                                        ))
                                })
                                .collect::<Vec<_>>()
                        },
                    ),
                )
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .size_full()
                .track_scroll(stack_handle),
            )
            .child(self.render_vertical_scrollbar(palette, context))
            .child(self.render_horizontal_scrollbar(palette, context))
    }

    /// 渲染线程堆栈详情纵向滚动条。
    ///
    /// 业务意图：
    /// - 完整线程堆栈可能超过窗口高度；显式滑块既提示当前位置，也允许用户直接拖动到深处堆栈帧。
    fn render_vertical_scrollbar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) = self.scrollbar_metrics(LogScrollbarAxis::Vertical) else {
            return div().id("thread-stack-vertical-scrollbar-empty").hidden();
        };

        div()
            .id("thread-stack-vertical-scrollbar")
            .absolute()
            .top(metrics.thumb_start)
            .right(px(LOG_VIEWER_SCROLLBAR_PADDING))
            .w(px(LOG_VIEWER_SCROLLBAR_WIDTH))
            .h(metrics.thumb_length)
            .rounded(px(LOG_VIEWER_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(palette.scrollbar))
            .cursor_pointer()
            .hover(move |thumb| thumb.bg(rgb(palette.scrollbar_hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.start_scrollbar_drag(LogScrollbarAxis::Vertical, event, context);
                    context.stop_propagation();
                }),
            )
    }

    /// 渲染线程堆栈详情横向滚动条。
    ///
    /// 业务意图：
    /// - 线程头、类名和 Oracle JDBC 等堆栈帧经常很长；横向滑块让用户在没有触控板横向手势时也能查看完整行。
    fn render_horizontal_scrollbar(
        &self,
        palette: AppThemePalette,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(metrics) = self.scrollbar_metrics(LogScrollbarAxis::Horizontal) else {
            return div().id("thread-stack-horizontal-scrollbar-empty").hidden();
        };

        div()
            .id("thread-stack-horizontal-scrollbar")
            .absolute()
            .left(metrics.thumb_start)
            .bottom(px(LOG_VIEWER_SCROLLBAR_PADDING))
            .w(metrics.thumb_length)
            .h(px(LOG_VIEWER_SCROLLBAR_WIDTH))
            .rounded(px(LOG_VIEWER_SCROLLBAR_WIDTH / 2.0))
            .bg(rgb(palette.scrollbar))
            .cursor_pointer()
            .hover(move |thumb| thumb.bg(rgb(palette.scrollbar_hover)))
            .on_mouse_down(
                MouseButton::Left,
                context.listener(|view, event: &MouseDownEvent, _window, context| {
                    view.start_scrollbar_drag(LogScrollbarAxis::Horizontal, event, context);
                    context.stop_propagation();
                }),
            )
    }

    /// 返回指定轴向的滚动条测量结果。
    fn scrollbar_metrics(&self, axis: LogScrollbarAxis) -> Option<LogScrollbarMetrics> {
        match axis {
            LogScrollbarAxis::Vertical => {
                MainView::log_vertical_scrollbar_metrics(&self.stack_scroll_handle)
            }
            LogScrollbarAxis::Horizontal => MainView::log_horizontal_scrollbar_metrics(
                &self.stack_scroll_handle,
                THREAD_STACK_WINDOW_LINE_NUMBER_WIDTH,
            ),
        }
    }

    /// 开始拖动线程堆栈详情滚动条。
    ///
    /// 边界条件：
    /// - 布局首帧尚未产生滚动范围或内容没有溢出时，不创建拖动状态。
    fn start_scrollbar_drag(
        &mut self,
        axis: LogScrollbarAxis,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        let Some(metrics) = self.scrollbar_metrics(axis) else {
            return;
        };
        if metrics.max_scroll <= px(0.0) {
            return;
        }
        let Some(viewport_origin) =
            MainView::uniform_list_viewport_axis_origin(&self.stack_scroll_handle, axis)
        else {
            return;
        };
        let pointer_position = match axis {
            LogScrollbarAxis::Vertical => event.position.y,
            LogScrollbarAxis::Horizontal => event.position.x,
        };
        self.stack_scrollbar_drag = Some(ThreadStackScrollbarDrag {
            axis,
            cursor_offset: pointer_position - viewport_origin - metrics.thumb_start,
        });
        self.selection_drag_anchor = None;
        context.notify();
    }

    /// 根据鼠标移动更新线程堆栈详情滚动条拖动。
    fn update_scrollbar_drag(&mut self, event: &MouseMoveEvent, context: &mut Context<Self>) {
        let Some(drag) = self.stack_scrollbar_drag else {
            return;
        };
        if !event.dragging() {
            self.stack_scrollbar_drag = None;
            context.notify();
            return;
        }
        let Some(metrics) = self.scrollbar_metrics(drag.axis) else {
            self.stack_scrollbar_drag = None;
            context.notify();
            return;
        };
        let Some(viewport_origin) =
            MainView::uniform_list_viewport_axis_origin(&self.stack_scroll_handle, drag.axis)
        else {
            self.stack_scrollbar_drag = None;
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
        let scroll_offset =
            metrics.max_scroll * ((thumb_start - metrics.track_start) / movable_length);
        let base_scroll_handle = { self.stack_scroll_handle.0.borrow().base_handle.clone() };
        let current_offset = base_scroll_handle.offset();
        match drag.axis {
            LogScrollbarAxis::Vertical => {
                base_scroll_handle.set_offset(point(current_offset.x, -scroll_offset));
            }
            LogScrollbarAxis::Horizontal => {
                base_scroll_handle.set_offset(point(-scroll_offset, current_offset.y));
            }
        }
        context.notify();
    }

    /// 结束线程堆栈详情滚动条拖动。
    fn stop_scrollbar_drag(&mut self, context: &mut Context<Self>) {
        if self.stack_scrollbar_drag.take().is_some() {
            context.notify();
        }
    }
}

impl Render for ThreadStackWindowView {
    /// 渲染线程堆栈详情窗口。
    fn render(&mut self, _window: &mut Window, context: &mut Context<Self>) -> impl IntoElement {
        let (palette, syntax_theme) = {
            let main_view = self.main_view.read(context);
            (
                main_view.palette(),
                main_view.effective_theme().syntax_theme(),
            )
        };
        div()
            .track_focus(&self.stack_focus_handle)
            .key_context("thread-stack-window")
            .on_key_down(
                context.listener(|view, event: &KeyDownEvent, _window, context| {
                    view.handle_stack_key_down(event, context);
                }),
            )
            .on_mouse_move(
                context.listener(|view, event: &MouseMoveEvent, _window, context| {
                    if view.stack_scrollbar_drag.is_some() {
                        view.update_scrollbar_drag(event, context);
                        context.stop_propagation();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    let mut consumed = false;
                    if view.stack_scrollbar_drag.is_some() {
                        view.stop_scrollbar_drag(context);
                        consumed = true;
                    }
                    if view.selection_drag_anchor.is_some() {
                        view.stop_stack_text_selection(context);
                        consumed = true;
                    }
                    if consumed {
                        context.stop_propagation();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                context.listener(|view, _event: &MouseUpEvent, _window, context| {
                    let mut consumed = false;
                    if view.stack_scrollbar_drag.is_some() {
                        view.stop_scrollbar_drag(context);
                        consumed = true;
                    }
                    if view.selection_drag_anchor.is_some() {
                        view.stop_stack_text_selection(context);
                        consumed = true;
                    }
                    if consumed {
                        context.stop_propagation();
                    }
                }),
            )
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(palette.background))
            .child(self.render_header(palette, context))
            .child(self.render_stack_body(palette, syntax_theme, context))
    }
}
