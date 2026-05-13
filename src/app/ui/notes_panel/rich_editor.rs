// 笔记富文本编辑器 UI 状态和自绘元素。
//
// 业务意图：
// - 该文件承载笔记正文的纯 Rust / GPUI 富文本编辑能力，避免引入 WebView 或 JS 构建链。
// - 文档模型来自 `crate::notes`，本文件只保存焦点、选区、撤销栈、排版缓存和工具栏临时状态。
//
// 边界条件：
// - 文本位置使用富文本文档的线性 UTF-8 字节下标；输入法回调中的 UTF-16 下标会在 shell 适配层转换。
// - 普通正文默认按可用宽度软换行，代码块保持横向滚动；字号、颜色、加粗等局部样式按 run 自绘并参与复制粘贴。

use std::{collections::HashMap, ops::Range};

use super::*;

/// 富文本工具栏可切换的内联样式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum NoteRichTextInlineCommand {
    /// 加粗。
    Bold,
    /// 斜体。
    Italic,
    /// 下划线。
    Underline,
    /// 删除线。
    Strikethrough,
}

/// 富文本编辑器撤销快照。
#[derive(Clone)]
pub(in crate::app) struct NoteRichTextEditorSnapshot {
    /// 快照中的完整文档。
    document: NoteRichTextDocument,
    /// 快照中的选区。
    selection_range: Range<usize>,
    /// 快照中的待输入样式。
    pending_style: NoteRichTextStyle,
}

/// 单个富文本绘制片段排版信息。
#[derive(Clone)]
pub(in crate::app) struct NoteRichTextFragmentLayout {
    /// 片段在整篇笔记纯文本中的 UTF-8 范围。
    pub(in crate::app) byte_range: Range<usize>,
    /// 片段左上角横坐标。
    pub(in crate::app) x: Pixels,
    /// 片段对应的 GPUI shaped line。
    pub(in crate::app) line: ShapedLine,
}

/// 单行富文本排版缓存。
#[derive(Clone)]
pub(in crate::app) struct NoteRichTextLineLayout {
    /// 行在整篇笔记纯文本中的 UTF-8 范围，不包含块间换行。
    pub(in crate::app) byte_range: Range<usize>,
    /// 行绘制边界。
    pub(in crate::app) bounds: Bounds<Pixels>,
    /// 正文内容起始横坐标；列表前缀不参与文本命中。
    pub(in crate::app) content_left: Pixels,
    /// 行内片段排版结果。
    pub(in crate::app) fragments: Vec<NoteRichTextFragmentLayout>,
}

/// 单个代码块容器的排版缓存。
#[derive(Clone)]
pub(in crate::app) struct NoteRichTextCodeBlockLayout {
    /// 代码块组起始块下标；连续代码行共享同一个横向滚动偏移。
    pub(in crate::app) block_index: usize,
    /// 代码块组在线性纯文本中的范围，不包含块组后面的分隔换行。
    ///
    /// 业务意图：
    /// - 语言标签点击和“点击代码块下方跳出代码块”都需要把视觉代码块映射回模型位置。
    /// - 范围只覆盖当前连续同语言代码块组，避免相邻普通段落或其它语言代码块被误改。
    pub(in crate::app) byte_range: Range<usize>,
    /// 代码块整体绘制边界。
    pub(in crate::app) bounds: Bounds<Pixels>,
    /// 代码块内容最后一行的底部坐标。
    ///
    /// 业务意图：
    /// - 鼠标点击代码块内容下方、横向滚动条上方的空白区时，应把光标移出代码块而不是继续落在末行末尾。
    pub(in crate::app) content_bottom: Pixels,
    /// 代码块左上角语言标签边界。
    ///
    /// 业务意图：
    /// - 语言选择入口迁移到代码块内部，命中该边界即可打开语言下拉，不再占用顶部全局工具栏。
    pub(in crate::app) language_label_bounds: Bounds<Pixels>,
    /// 代码内容可见视口宽度。
    pub(in crate::app) viewport_width: Pixels,
    /// 代码内容真实宽度。
    pub(in crate::app) content_width: Pixels,
    /// 横向滚动条轨道；内容未超宽时为空。
    pub(in crate::app) scrollbar_track: Option<Bounds<Pixels>>,
    /// 横向滚动条滑块；内容未超宽时为空。
    pub(in crate::app) scrollbar_thumb: Option<Bounds<Pixels>>,
}

/// 代码块横向滚动条拖拽状态。
pub(in crate::app) struct NoteRichTextCodeScrollDrag {
    /// 被拖拽的代码块组起始块下标。
    block_index: usize,
    /// 拖拽开始时鼠标横坐标。
    start_x: Pixels,
    /// 拖拽开始时内容横向偏移。
    start_scroll_left: f32,
    /// 最大可滚动距离。
    max_scroll_left: f32,
    /// 滑块可移动的轨道长度。
    movable_track_width: f32,
}

/// 富文本编辑器状态。
pub(in crate::app) struct RichTextEditorState {
    /// 当前右侧展示或编辑的富文本文档。
    pub(in crate::app) document: NoteRichTextDocument,
    /// 当前线性 UTF-8 选区；空选区表示光标。
    pub(in crate::app) selection_range: Range<usize>,
    /// 输入法组合文本范围。
    pub(in crate::app) marked_range: Option<Range<usize>>,
    /// 正文焦点句柄。
    pub(in crate::app) focus: gpui::FocusHandle,
    /// 鼠标拖选锚点。
    pub(in crate::app) drag_anchor: Option<usize>,
    /// 无选区时后续输入继承的样式。
    pub(in crate::app) pending_style: NoteRichTextStyle,
    /// 最近一次排版缓存，供鼠标命中和 IME 候选窗口定位。
    pub(in crate::app) last_layouts: Vec<NoteRichTextLineLayout>,
    /// 最近一次正文元素边界。
    pub(in crate::app) last_bounds: Option<Bounds<Pixels>>,
    /// 最近一次按实际宽度测得的正文总高度。
    ///
    /// 业务意图：
    /// - 普通正文开启自动换行后，真实高度依赖编辑器宽度；布局阶段还拿不到最终宽度，因此先使用上次测量值，
    ///   绘制阶段再用真实宽度修正并触发下一次布局。
    measured_visual_height: Option<(f32, f32)>,
    /// 最近一次代码块容器排版缓存，供横向滚动命中。
    pub(in crate::app) last_code_blocks: Vec<NoteRichTextCodeBlockLayout>,
    /// 代码块横向滚动偏移，按代码块组起始块下标保存。
    ///
    /// 边界条件：
    /// - 富文本文档没有稳定块 ID，编辑导致块下标变化时偏移允许自然重置，不能影响内容正确性。
    pub(in crate::app) code_block_scroll_left: HashMap<usize, f32>,
    /// 代码块横向滚动条拖拽状态。
    pub(in crate::app) code_scroll_drag: Option<NoteRichTextCodeScrollDrag>,
    /// 撤销栈。
    undo_stack: Vec<NoteRichTextEditorSnapshot>,
    /// 重做栈。
    redo_stack: Vec<NoteRichTextEditorSnapshot>,
    /// 字号菜单是否打开。
    pub(in crate::app) font_size_menu_open: bool,
    /// 颜色菜单是否打开。
    pub(in crate::app) color_menu_open: bool,
    /// 背景颜色菜单是否打开。
    pub(in crate::app) background_color_menu_open: bool,
    /// 代码块语言菜单是否打开。
    pub(in crate::app) code_language_menu_open: bool,
    /// 代码块语言菜单锚点。
    ///
    /// 业务意图：
    /// - 语言下拉由代码块左上角标签触发，菜单需要贴近触发标签绘制；该字段保存最近一次命中的标签窗口坐标。
    pub(in crate::app) code_language_menu_anchor: Option<Bounds<Pixels>>,
}

impl RichTextEditorState {
    /// 创建正文编辑器状态。
    pub(in crate::app) fn new(context: &mut Context<MainView>) -> Self {
        Self {
            document: NoteRichTextDocument::empty(),
            selection_range: 0..0,
            marked_range: None,
            focus: context.focus_handle(),
            drag_anchor: None,
            pending_style: NoteRichTextStyle::default(),
            last_layouts: Vec::new(),
            last_bounds: None,
            measured_visual_height: None,
            last_code_blocks: Vec::new(),
            code_block_scroll_left: HashMap::new(),
            code_scroll_drag: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            font_size_menu_open: false,
            color_menu_open: false,
            background_color_menu_open: false,
            code_language_menu_open: false,
            code_language_menu_anchor: None,
        }
    }

    /// 用新文档重置编辑器。
    pub(in crate::app) fn reset_document(&mut self, document: NoteRichTextDocument) {
        self.document = document;
        self.document.normalize();
        self.selection_range = 0..0;
        self.marked_range = None;
        self.drag_anchor = None;
        self.pending_style = self.document.style_at_position(0);
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.font_size_menu_open = false;
        self.color_menu_open = false;
        self.background_color_menu_open = false;
        self.code_language_menu_open = false;
        self.code_language_menu_anchor = None;
        self.last_layouts.clear();
        self.last_bounds = None;
        self.measured_visual_height = None;
        self.last_code_blocks.clear();
        self.code_block_scroll_left.clear();
        self.code_scroll_drag = None;
    }

    /// 返回当前线性纯文本。
    pub(in crate::app) fn plain_text(&self) -> String {
        self.document.plain_text()
    }

    /// 返回当前选区，并确保位于合法 UTF-8 边界。
    pub(in crate::app) fn clamped_selection(&self) -> Range<usize> {
        clamp_note_rich_text_range(&self.plain_text(), self.selection_range.clone())
    }

    /// 判断当前是否有非空选区。
    pub(in crate::app) fn has_selection(&self) -> bool {
        let selection = self.clamped_selection();
        selection.start < selection.end
    }

    /// 设置正文选区。
    pub(in crate::app) fn set_selection(&mut self, range: Range<usize>) {
        self.marked_range = None;
        self.selection_range = clamp_note_rich_text_range(&self.plain_text(), range);
        self.pending_style = self.document.style_at_position(self.selection_range.end);
    }

    /// 返回可保存的富文本 JSON。
    pub(in crate::app) fn serialized_content(&self) -> Result<String, String> {
        self.document.to_json()
    }

    /// 返回显示高度估算，供自绘元素参与滚动布局。
    pub(in crate::app) fn visual_height(&self) -> f32 {
        if let Some((_width, height)) = self.measured_visual_height {
            return height.max(220.0);
        }
        let mut height = NOTES_RICH_TEXT_VERTICAL_PADDING * 2.0;
        let mut index = 0;
        while index < self.document.blocks.len() {
            let block = &self.document.blocks[index];
            if block.kind == NoteRichTextBlockKind::CodeBlock {
                let language = block.code_language.clone();
                let mut line_count = 0;
                while let Some(next) = self.document.blocks.get(index + line_count) {
                    if next.kind != NoteRichTextBlockKind::CodeBlock
                        || next.code_language != language
                    {
                        break;
                    }
                    line_count += 1;
                }
                height += code_block_visual_height(line_count);
                index += line_count.max(1);
            } else {
                height += rich_text_block_line_height(block);
                index += 1;
            }
        }
        height.max(220.0)
    }

    /// 保存最近一次排版结果。
    pub(in crate::app) fn store_layout(
        &mut self,
        layouts: Vec<NoteRichTextLineLayout>,
        code_blocks: Vec<NoteRichTextCodeBlockLayout>,
        bounds: Bounds<Pixels>,
        measured_height: f32,
    ) -> bool {
        let measured_width = f32::from(bounds.size.width);
        let height_changed = self.measured_visual_height.is_none_or(|(width, height)| {
            (width - measured_width).abs() > 0.5 || (height - measured_height).abs() > 0.5
        });
        self.last_layouts = layouts;
        self.last_code_blocks = code_blocks;
        self.last_bounds = Some(bounds);
        self.measured_visual_height = Some((measured_width, measured_height));
        height_changed
    }

    /// 根据鼠标位置计算插入点。
    pub(in crate::app) fn index_for_point(&self, position: Point<Pixels>) -> usize {
        if self.last_layouts.is_empty() {
            return self.document.len();
        }
        let mut nearest = self.document.len();
        for layout in &self.last_layouts {
            if position.y < layout.bounds.top() {
                return layout.byte_range.start;
            }
            nearest = layout.byte_range.end;
            if position.y <= layout.bounds.bottom() {
                if layout.fragments.is_empty() || position.x <= layout.content_left {
                    return layout.byte_range.start;
                }
                for fragment in &layout.fragments {
                    let fragment_left = fragment.x;
                    let fragment_right = fragment.x + fragment.line.width;
                    if position.x <= fragment_right {
                        let local = fragment
                            .line
                            .closest_index_for_x((position.x - fragment_left).max(px(0.0)));
                        return clamp_note_rich_text_boundary(
                            &self.plain_text(),
                            fragment.byte_range.start + local,
                        );
                    }
                }
                return layout.byte_range.end;
            }
        }
        nearest
    }

    /// 返回指定范围的屏幕边界，用于 IME 候选窗口。
    pub(in crate::app) fn bounds_for_range(
        &self,
        range: Range<usize>,
        fallback: Bounds<Pixels>,
    ) -> Bounds<Pixels> {
        let range = clamp_note_rich_text_range(&self.plain_text(), range);
        let cursor = range.start;
        for layout in &self.last_layouts {
            if cursor >= layout.byte_range.start && cursor <= layout.byte_range.end {
                for fragment in &layout.fragments {
                    if cursor >= fragment.byte_range.start && cursor <= fragment.byte_range.end {
                        let local = cursor.saturating_sub(fragment.byte_range.start);
                        return Bounds::new(
                            point(
                                fragment.x + fragment.line.x_for_index(local),
                                layout.bounds.top(),
                            ),
                            size(px(1.0), layout.bounds.bottom() - layout.bounds.top()),
                        );
                    }
                }
                return Bounds::new(
                    point(layout.content_left, layout.bounds.top()),
                    size(px(1.0), layout.bounds.bottom() - layout.bounds.top()),
                );
            }
        }
        fallback
    }

    /// 开始一次可撤销修改。
    fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(NoteRichTextEditorSnapshot {
            document: self.document.clone(),
            selection_range: self.selection_range.clone(),
            pending_style: self.pending_style.clone(),
        });
        if self.undo_stack.len() > NOTE_RICH_TEXT_HISTORY_LIMIT {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    /// 应用一次修改后的通用收尾。
    fn finish_mutation(&mut self, cursor: usize) {
        let text = self.plain_text();
        let cursor = clamp_note_rich_text_boundary(&text, cursor);
        self.selection_range = cursor..cursor;
        self.marked_range = None;
        self.pending_style = self.document.style_at_position(cursor);
    }

    /// 用纯文本替换当前或指定选区。
    pub(in crate::app) fn replace_range_with_text(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
    ) {
        let replacement = text.replace("\r\n", "\n").replace('\r', "\n");
        let range = range
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.clamped_selection());
        let range = clamp_note_rich_text_range(&self.plain_text(), range);
        self.push_undo_snapshot();
        self.document.replace_range_with_text(
            range.clone(),
            &replacement,
            self.pending_style.clone(),
        );
        self.finish_mutation(range.start + replacement.len());
    }

    /// 用富文本片段替换当前选区。
    pub(in crate::app) fn replace_selection_with_document(
        &mut self,
        document: &NoteRichTextDocument,
    ) {
        let range = self.clamped_selection();
        let inserted_len = document.len();
        self.push_undo_snapshot();
        self.document
            .replace_range_with_document(range.clone(), document);
        self.finish_mutation(range.start + inserted_len);
    }

    /// 删除选区或光标前一个 UTF-8 字符。
    pub(in crate::app) fn delete_backward(&mut self) {
        let text = self.plain_text();
        let selection = self.clamped_selection();
        let range = if selection.start < selection.end {
            selection
        } else if let Some((previous, _)) = text[..selection.start].char_indices().next_back() {
            previous..selection.start
        } else {
            return;
        };
        self.push_undo_snapshot();
        self.document.delete_range(range.clone());
        self.finish_mutation(range.start);
    }

    /// 删除选区或光标后一个 UTF-8 字符。
    pub(in crate::app) fn delete_forward(&mut self) {
        let text = self.plain_text();
        let selection = self.clamped_selection();
        let range = if selection.start < selection.end {
            selection
        } else if let Some((offset, character)) = text[selection.end..].char_indices().next() {
            let start = selection.end + offset;
            start..start + character.len_utf8()
        } else {
            return;
        };
        self.push_undo_snapshot();
        self.document.delete_range(range.clone());
        self.finish_mutation(range.start);
    }

    /// 应用内联样式；无选区时只更新待输入样式。
    pub(in crate::app) fn apply_style_patch(&mut self, patch: NoteRichTextStylePatch) {
        if self.has_selection() {
            let range = self.clamped_selection();
            self.push_undo_snapshot();
            self.document.apply_style_patch(range, &patch);
        } else {
            self.pending_style.apply_patch(&patch);
        }
    }

    /// 切换加粗、斜体、下划线或删除线。
    pub(in crate::app) fn toggle_inline_command(&mut self, command: NoteRichTextInlineCommand) {
        let current = self.current_inline_command_active(command);
        let next = !current;
        let patch = match command {
            NoteRichTextInlineCommand::Bold => NoteRichTextStylePatch {
                bold: Some(next),
                ..Default::default()
            },
            NoteRichTextInlineCommand::Italic => NoteRichTextStylePatch {
                italic: Some(next),
                ..Default::default()
            },
            NoteRichTextInlineCommand::Underline => NoteRichTextStylePatch {
                underline: Some(next),
                ..Default::default()
            },
            NoteRichTextInlineCommand::Strikethrough => NoteRichTextStylePatch {
                strikethrough: Some(next),
                ..Default::default()
            },
        };
        self.apply_style_patch(patch);
    }

    /// 判断工具栏按钮是否处于激活状态。
    pub(in crate::app) fn current_inline_command_active(
        &self,
        command: NoteRichTextInlineCommand,
    ) -> bool {
        let style = if self.has_selection() {
            self.document
                .style_at_position(self.clamped_selection().start)
        } else {
            self.pending_style.clone()
        };
        match command {
            NoteRichTextInlineCommand::Bold => style.bold,
            NoteRichTextInlineCommand::Italic => style.italic,
            NoteRichTextInlineCommand::Underline => style.underline,
            NoteRichTextInlineCommand::Strikethrough => style.strikethrough,
        }
    }

    /// 切换选区所在块的列表类型。
    pub(in crate::app) fn toggle_list_kind(&mut self, kind: NoteRichTextBlockKind) {
        let range = self.clamped_selection();
        self.push_undo_snapshot();
        self.document.toggle_blocks_kind(range, kind);
    }

    /// 切换选区所在块的代码块状态。
    pub(in crate::app) fn toggle_code_block(&mut self) {
        let range = self.clamped_selection();
        self.push_undo_snapshot();
        self.document
            .toggle_blocks_kind(range, NoteRichTextBlockKind::CodeBlock);
    }

    /// 设置当前代码块语言。
    pub(in crate::app) fn set_code_language(&mut self, language: &str) {
        let range = self.clamped_selection();
        self.push_undo_snapshot();
        self.document.set_code_language_for_range(range, language);
    }

    /// 判断当前光标或选区起点是否位于代码块中。
    pub(in crate::app) fn current_block_is_code(&self) -> bool {
        self.document
            .block_kind_at_position(self.clamped_selection().start)
            == NoteRichTextBlockKind::CodeBlock
    }

    /// 根据最近一次代码块语言标签命中位置，返回语言下拉菜单在编辑器容器内的相对坐标。
    ///
    /// 业务意图：
    /// - 语言菜单现在由代码块左上角标签触发，必须贴近具体代码块，而不是固定显示在顶部工具栏。
    /// - GPUI 自绘元素缓存的是窗口坐标，菜单浮层是编辑器容器内的绝对布局，因此这里使用正文自绘边界做坐标换算。
    ///
    /// 边界条件：
    /// - 首次绘制或滚动过程中可能暂时没有有效缓存，此时退回到代码块常见的左上位置，保证菜单仍可见。
    pub(in crate::app) fn code_language_menu_position(&self) -> (Pixels, Pixels) {
        let Some(anchor) = self.code_language_menu_anchor else {
            return (
                px(NOTES_RICH_TEXT_HORIZONTAL_PADDING),
                px(NOTES_RICH_TEXT_TOOLBAR_HEIGHT + NOTES_RICH_TEXT_VERTICAL_PADDING),
            );
        };
        let Some(bounds) = self.last_bounds else {
            return (
                px(NOTES_RICH_TEXT_HORIZONTAL_PADDING),
                px(NOTES_RICH_TEXT_TOOLBAR_HEIGHT + NOTES_RICH_TEXT_VERTICAL_PADDING),
            );
        };
        (
            (anchor.left() - bounds.left()).max(px(4.0)),
            (anchor.bottom() - bounds.top() + px(NOTES_RICH_TEXT_TOOLBAR_HEIGHT + 2.0))
                .max(px(NOTES_RICH_TEXT_TOOLBAR_HEIGHT)),
        )
    }

    /// 命中代码块语言标签并打开语言菜单。
    ///
    /// 业务意图：
    /// - 代码语言属于代码块自身属性，入口放在代码块左上角更贴合用户心智。
    /// - 点击标签时同步选择当前视觉代码块组，后续选择语言会作用于该组所有连续代码行。
    pub(in crate::app) fn start_code_language_selection(
        &mut self,
        position: Point<Pixels>,
    ) -> bool {
        let Some(layout) = self
            .last_code_blocks
            .iter()
            .find(|layout| bounds_contains_point(layout.language_label_bounds, position))
            .cloned()
        else {
            return false;
        };
        self.set_selection(layout.byte_range.clone());
        self.code_language_menu_anchor = Some(layout.language_label_bounds);
        self.code_language_menu_open = true;
        self.font_size_menu_open = false;
        self.color_menu_open = false;
        self.background_color_menu_open = false;
        self.drag_anchor = None;
        true
    }

    /// 点击代码块内容下方时，把光标移动到代码块之后。
    ///
    /// 业务意图：
    /// - 代码块内部 `Enter` 会继续输入代码行，但用户点击代码块下方空白区域时通常是在表达“结束代码块，继续写正文”。
    /// - 如果代码块后没有普通段落，本方法会懒创建一个空段落，避免光标被困在最后一个代码块里。
    ///
    /// 边界条件：
    /// - 横向滚动条和语言标签有独立交互，命中这些区域时不能触发跳出代码块。
    /// - 插入空段落会进入撤销栈；仅移动到已有段落开头不产生可撤销内容修改。
    pub(in crate::app) fn place_cursor_after_code_block_at_point(
        &mut self,
        position: Point<Pixels>,
    ) -> bool {
        let Some(layout) = self
            .last_code_blocks
            .iter()
            .find(|layout| {
                let next_line_top = self
                    .last_layouts
                    .iter()
                    .filter(|line| line.bounds.top() > layout.bounds.bottom())
                    .map(|line| line.bounds.top())
                    .min_by(|left, right| {
                        f32::from(*left)
                            .partial_cmp(&f32::from(*right))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                let before_next_line = next_line_top.is_none_or(|top| position.y < top);
                position.y > layout.content_bottom
                    && before_next_line
                    && !bounds_contains_point(layout.language_label_bounds, position)
                    && !layout
                        .scrollbar_track
                        .is_some_and(|track| bounds_contains_point(track, position))
            })
            .cloned()
        else {
            return false;
        };
        let snapshot = NoteRichTextEditorSnapshot {
            document: self.document.clone(),
            selection_range: self.selection_range.clone(),
            pending_style: self.pending_style.clone(),
        };
        let Some((cursor, changed)) = self
            .document
            .ensure_paragraph_after_code_block_at_position(layout.byte_range.end)
        else {
            return false;
        };
        if changed {
            self.undo_stack.push(snapshot);
            if self.undo_stack.len() > NOTE_RICH_TEXT_HISTORY_LIMIT {
                self.undo_stack.remove(0);
            }
            self.redo_stack.clear();
        }
        self.finish_mutation(cursor);
        self.drag_anchor = None;
        true
    }

    /// 尝试按滚轮事件更新代码块横向滚动。
    ///
    /// 业务意图：
    /// - 整篇笔记仍由外层滚动容器负责纵向滚动；只有鼠标位于代码块上且产生横向滚动时才消费事件。
    /// - macOS 触控板会直接产生 `delta.x`，Windows 常见 Shift+滚轮产生纵向 delta，这里同时兼容两种输入。
    pub(in crate::app) fn scroll_code_block_at_point(
        &mut self,
        position: Point<Pixels>,
        delta: Point<Pixels>,
        shift: bool,
    ) -> bool {
        let horizontal_delta = if shift && f32::from(delta.y).abs() > 0.0 {
            delta.y
        } else {
            delta.x
        };
        if f32::from(horizontal_delta).abs() <= f32::EPSILON {
            return false;
        }
        let Some(layout) = self
            .last_code_blocks
            .iter()
            .find(|layout| {
                bounds_contains_point(layout.bounds, position)
                    && layout.content_width > layout.viewport_width
            })
            .cloned()
        else {
            return false;
        };
        let max_scroll = (layout.content_width - layout.viewport_width).max(px(0.0));
        let current = self
            .code_block_scroll_left
            .get(&layout.block_index)
            .copied()
            .unwrap_or(0.0);
        let next = (current - f32::from(horizontal_delta)).clamp(0.0, f32::from(max_scroll));
        self.code_block_scroll_left.insert(layout.block_index, next);
        true
    }

    /// 开始拖拽代码块横向滚动条。
    ///
    /// 业务意图：
    /// - 代码块滚动条位于自绘元素内部，必须先消费鼠标按下，否则事件会被正文选区逻辑当作拖选文本处理。
    pub(in crate::app) fn start_code_block_scroll_drag(&mut self, position: Point<Pixels>) -> bool {
        let Some(layout) = self
            .last_code_blocks
            .iter()
            .find(|layout| {
                layout
                    .scrollbar_track
                    .is_some_and(|track| bounds_contains_point(track, position))
            })
            .cloned()
        else {
            return false;
        };
        let Some(track) = layout.scrollbar_track else {
            return false;
        };
        let Some(thumb) = layout.scrollbar_thumb else {
            return false;
        };
        let max_scroll_left =
            f32::from((layout.content_width - layout.viewport_width).max(px(0.0)));
        if max_scroll_left <= 0.0 {
            return false;
        }
        let movable_track_width = f32::from((track.size.width - thumb.size.width).max(px(1.0)));
        let start_scroll_left = if bounds_contains_point(thumb, position) {
            self.code_block_scroll_left
                .get(&layout.block_index)
                .copied()
                .unwrap_or(0.0)
        } else {
            let ratio = ((f32::from(position.x - track.left())
                - f32::from(thumb.size.width) / 2.0)
                / movable_track_width)
                .clamp(0.0, 1.0);
            let next = max_scroll_left * ratio;
            self.code_block_scroll_left.insert(layout.block_index, next);
            next
        };
        self.code_scroll_drag = Some(NoteRichTextCodeScrollDrag {
            block_index: layout.block_index,
            start_x: position.x,
            start_scroll_left,
            max_scroll_left,
            movable_track_width,
        });
        true
    }

    /// 更新代码块横向滚动条拖拽。
    pub(in crate::app) fn update_code_block_scroll_drag(
        &mut self,
        position: Point<Pixels>,
    ) -> bool {
        let Some(drag) = &self.code_scroll_drag else {
            return false;
        };
        let delta_ratio = f32::from(position.x - drag.start_x) / drag.movable_track_width.max(1.0);
        let next = (drag.start_scroll_left + drag.max_scroll_left * delta_ratio)
            .clamp(0.0, drag.max_scroll_left);
        self.code_block_scroll_left.insert(drag.block_index, next);
        true
    }

    /// 结束代码块横向滚动条拖拽。
    pub(in crate::app) fn finish_code_block_scroll_drag(&mut self) -> bool {
        self.code_scroll_drag.take().is_some()
    }

    /// 撤销最近一次修改。
    pub(in crate::app) fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo_stack.pop() else {
            return false;
        };
        self.redo_stack.push(NoteRichTextEditorSnapshot {
            document: self.document.clone(),
            selection_range: self.selection_range.clone(),
            pending_style: self.pending_style.clone(),
        });
        self.document = snapshot.document;
        self.selection_range = snapshot.selection_range;
        self.pending_style = snapshot.pending_style;
        self.marked_range = None;
        true
    }

    /// 重做最近一次撤销。
    pub(in crate::app) fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push(NoteRichTextEditorSnapshot {
            document: self.document.clone(),
            selection_range: self.selection_range.clone(),
            pending_style: self.pending_style.clone(),
        });
        self.document = snapshot.document;
        self.selection_range = snapshot.selection_range;
        self.pending_style = snapshot.pending_style;
        self.marked_range = None;
        true
    }

    /// 移动光标。
    pub(in crate::app) fn move_cursor(&mut self, target: usize, extend_selection: bool) {
        let text = self.plain_text();
        let target = clamp_note_rich_text_boundary(&text, target);
        if extend_selection {
            self.selection_range.end = target;
            self.selection_range = clamp_note_rich_text_range(&text, self.selection_range.clone());
        } else {
            self.selection_range = target..target;
        }
        self.marked_range = None;
        self.pending_style = self.document.style_at_position(target);
    }

    /// 返回当前选区富文本片段。
    pub(in crate::app) fn selected_document(&self) -> Option<NoteRichTextDocument> {
        self.has_selection()
            .then(|| self.document.fragment_for_range(self.clamped_selection()))
    }

    /// 返回当前选区纯文本。
    pub(in crate::app) fn selected_text(&self) -> Option<String> {
        self.has_selection()
            .then(|| self.document.plain_text_range(self.clamped_selection()))
    }
}

/// 富文本自绘元素。
pub(in crate::app) struct NoteRichTextElement {
    /// 主视图实体。
    pub(in crate::app) view: Entity<MainView>,
    /// 是否为编辑态。
    pub(in crate::app) editable: bool,
    /// 当前主题。
    pub(in crate::app) palette: AppThemePalette,
    /// 当前有效主题，用于 syntect 选择亮色/暗色高亮。
    pub(in crate::app) theme: EffectiveTheme,
}

impl IntoElement for NoteRichTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for NoteRichTextElement {
    type RequestLayoutState = ();
    type PrepaintState = Option<NoteRichTextPrepaint>;

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
        style.size.height = px(self.view.read(context).notes.rich_editor.visual_height()).into();
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
        let (document, selection, cursor_visible, focused, code_block_scrolls) = {
            let view = self.view.read(context);
            (
                view.notes.rich_editor.document.clone(),
                view.notes.rich_editor.clamped_selection(),
                view.search_text_cursor_visible(),
                view.notes.rich_editor.focus.is_focused(window),
                view.notes.rich_editor.code_block_scroll_left.clone(),
            )
        };
        let layout_bounds = note_rich_text_layout_bounds(bounds, window);
        Some(layout_rich_text_document(
            &document,
            selection,
            self.editable && focused && cursor_visible,
            layout_bounds,
            self.palette,
            self.theme,
            &code_block_scrolls,
            window,
        ))
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
        let focus = self.view.read(context).notes.rich_editor.focus.clone();
        if self.editable {
            window.handle_input(
                &focus,
                ElementInputHandler::new(bounds, self.view.clone()),
                context,
            );
        }
        let Some(prepaint) = prepaint.take() else {
            return;
        };
        for quad in prepaint.background_quads {
            window.paint_quad(quad);
        }
        for quad in prepaint.selection_quads {
            window.paint_quad(quad);
        }
        for decoration in &prepaint.decorations {
            decoration
                .line
                .paint(
                    point(decoration.x, decoration.y),
                    decoration.line_height,
                    window,
                    context,
                )
                .ok();
        }
        for line in &prepaint.lines {
            for fragment in &line.fragments {
                paint_rich_text_fragment(
                    fragment,
                    line.bounds.top(),
                    line.bounds.bottom() - line.bounds.top(),
                    window,
                    context,
                );
            }
        }
        if let Some(cursor) = prepaint.cursor_quad {
            window.paint_quad(cursor);
        }
        if focus.is_focused(window) {
            window.request_animation_frame();
        }
        self.view.update(context, |view, context| {
            let height_changed = view.notes.rich_editor.store_layout(
                prepaint.lines,
                prepaint.code_blocks,
                prepaint.layout_bounds,
                prepaint.measured_height,
            );
            if height_changed {
                context.notify();
            }
        });
    }
}

pub(in crate::app) struct NoteRichTextPrepaint {
    layout_bounds: Bounds<Pixels>,
    lines: Vec<NoteRichTextLineLayout>,
    code_blocks: Vec<NoteRichTextCodeBlockLayout>,
    decorations: Vec<NoteRichTextDecorationFragment>,
    background_quads: Vec<PaintQuad>,
    selection_quads: Vec<PaintQuad>,
    cursor_quad: Option<PaintQuad>,
    measured_height: f32,
}

struct NoteRichTextDecorationFragment {
    x: Pixels,
    y: Pixels,
    line_height: Pixels,
    line: ShapedLine,
}

fn layout_rich_text_document(
    document: &NoteRichTextDocument,
    selection: Range<usize>,
    show_cursor: bool,
    bounds: Bounds<Pixels>,
    palette: AppThemePalette,
    theme: EffectiveTheme,
    code_block_scrolls: &HashMap<usize, f32>,
    window: &mut Window,
) -> NoteRichTextPrepaint {
    let text = document.plain_text();
    let selection = clamp_note_rich_text_range(&text, selection);
    let mut lines = Vec::new();
    let mut code_blocks = Vec::new();
    let mut decorations = Vec::new();
    let mut background_quads = Vec::new();
    let mut selection_quads = Vec::new();
    let mut cursor_quad = None;
    let mut y = bounds.top() + px(NOTES_RICH_TEXT_VERTICAL_PADDING);
    let mut offset = 0;
    let mut ordered_index = 1;
    let mut block_index = 0;

    while block_index < document.blocks.len() {
        let block = &document.blocks[block_index];
        if block.kind == NoteRichTextBlockKind::CodeBlock {
            let language = block
                .code_language
                .clone()
                .unwrap_or_else(|| "text".to_string());
            let group_start = block_index;
            let group_offset = offset;
            let mut group_end = block_index;
            let mut group_text_lines = Vec::new();
            let mut group_ranges = Vec::new();
            while let Some(next) = document.blocks.get(group_end) {
                if next.kind != NoteRichTextBlockKind::CodeBlock
                    || next.code_language.as_deref().unwrap_or("text") != language.as_str()
                {
                    break;
                }
                let line_text = next.plain_text();
                let line_start = offset;
                let line_end = line_start + line_text.len();
                group_ranges.push(line_start..line_end);
                group_text_lines.push(line_text);
                offset = line_end + usize::from(group_end + 1 < document.blocks.len());
                group_end += 1;
            }
            layout_code_block_group(
                group_start,
                group_offset,
                &language,
                &group_text_lines,
                &group_ranges,
                selection.clone(),
                show_cursor,
                bounds,
                &mut y,
                palette,
                theme,
                code_block_scrolls,
                window,
                &mut lines,
                &mut code_blocks,
                &mut decorations,
                &mut background_quads,
                &mut selection_quads,
                &mut cursor_quad,
            );
            block_index = group_end;
            continue;
        }

        let prefix_text = match block.kind {
            NoteRichTextBlockKind::Paragraph => None,
            NoteRichTextBlockKind::UnorderedListItem => Some("• ".to_string()),
            NoteRichTextBlockKind::OrderedListItem => {
                let prefix = format!("{ordered_index}. ");
                ordered_index += 1;
                Some(prefix)
            }
            NoteRichTextBlockKind::CodeBlock => None,
        };
        let block_end = layout_wrapped_rich_text_block(
            block,
            offset,
            prefix_text.as_deref(),
            selection.clone(),
            show_cursor,
            bounds,
            &mut y,
            palette,
            window,
            &mut lines,
            &mut decorations,
            &mut background_quads,
            &mut selection_quads,
            &mut cursor_quad,
        );
        offset = block_end + usize::from(block_index + 1 < document.blocks.len());
        block_index += 1;
    }
    let measured_height =
        f32::from((y - bounds.top()) + px(NOTES_RICH_TEXT_VERTICAL_PADDING)).max(220.0);

    NoteRichTextPrepaint {
        layout_bounds: bounds,
        lines,
        code_blocks,
        decorations,
        background_quads,
        selection_quads,
        cursor_quad,
        measured_height,
    }
}

#[allow(clippy::too_many_arguments)]
/// 排版一个普通富文本块，并按可用宽度拆成多条视觉行。
///
/// 业务意图：
/// - 普通段落和列表默认自动换行，编辑态和只读态都复用这里生成的视觉行缓存。
/// - 代码块不走该路径，继续保留横向滚动。
///
/// 边界条件：
/// - 文本范围仍使用文档线性 UTF-8 字节下标，换行只影响视觉行，不改变富文本 JSON。
/// - 列表前缀只绘制在第一条视觉行，后续行与正文起点对齐，避免续行压到项目符号或编号。
fn layout_wrapped_rich_text_block(
    block: &NoteRichTextBlock,
    block_start: usize,
    prefix_text: Option<&str>,
    selection: Range<usize>,
    show_cursor: bool,
    bounds: Bounds<Pixels>,
    y: &mut Pixels,
    palette: AppThemePalette,
    window: &mut Window,
    lines: &mut Vec<NoteRichTextLineLayout>,
    decorations: &mut Vec<NoteRichTextDecorationFragment>,
    background_quads: &mut Vec<PaintQuad>,
    selection_quads: &mut Vec<PaintQuad>,
    cursor_quad: &mut Option<PaintQuad>,
) -> usize {
    let line_height = px(rich_text_block_line_height(block));
    let block_text = block.plain_text();
    let block_end = block_start + block_text.len();
    let base_left = bounds.left() + px(NOTES_RICH_TEXT_HORIZONTAL_PADDING);
    let content_right = bounds.right() - px(NOTES_RICH_TEXT_HORIZONTAL_PADDING);
    let mut content_left = base_left;
    if let Some(prefix_text) = prefix_text {
        let prefix_line = shape_rich_text_fragment(
            prefix_text,
            &NoteRichTextStyle::default(),
            rgb(palette.muted_text).into(),
            window,
        );
        let width = prefix_line.width;
        decorations.push(NoteRichTextDecorationFragment {
            x: base_left,
            y: *y,
            line_height,
            line: prefix_line,
        });
        content_left += width;
    }

    if block.runs.is_empty() {
        if show_cursor
            && cursor_quad.is_none()
            && selection.start == selection.end
            && selection.start >= block_start
            && selection.start <= block_end
        {
            *cursor_quad = Some(fill(
                Bounds::new(point(content_left, *y), size(px(1.5), line_height)),
                rgb(palette.accent),
            ));
        }
        lines.push(NoteRichTextLineLayout {
            byte_range: block_start..block_end,
            bounds: Bounds::new(
                point(bounds.left(), *y),
                size(bounds.size.width, line_height),
            ),
            content_left,
            fragments: Vec::new(),
        });
        *y += line_height;
        return block_end;
    }

    let mut line_fragments = Vec::new();
    let mut line_start = block_start;
    let mut line_end = block_start;
    let mut x = content_left;
    let mut offset = block_start;
    for run in &block.runs {
        if run.text.is_empty() {
            continue;
        }
        let full_line = shape_rich_text_fragment(
            &run.text,
            &run.style,
            rich_text_color_to_hsla(run.style.color, palette.text),
            window,
        );
        let mut local_start = 0;
        while local_start < run.text.len() {
            if x >= content_right && !line_fragments.is_empty() {
                push_wrapped_visual_line(
                    bounds,
                    *y,
                    line_height,
                    content_left,
                    line_start..line_end,
                    &mut line_fragments,
                    lines,
                );
                *y += line_height;
                line_start = line_end;
                x = content_left;
            }
            let available = (content_right - x).max(px(1.0));
            let full_start_x = full_line.x_for_index(local_start);
            let remaining_width = full_line.width - full_start_x;
            let local_end = if remaining_width <= available {
                run.text.len()
            } else {
                let tentative = clamp_note_rich_text_boundary(
                    &run.text,
                    full_line.closest_index_for_x(full_start_x + available),
                );
                let preferred = preferred_soft_wrap_end(&run.text, local_start, tentative);
                let preferred_x = full_line.x_for_index(preferred) - full_start_x;
                let use_preferred = preferred < tentative && preferred_x >= available * 0.88;
                if use_preferred { preferred } else { tentative }
                    .max(next_char_boundary_after(&run.text, local_start))
            };
            let local_end = local_end.min(run.text.len());
            let range = offset + local_start..offset + local_end;
            let text = &run.text[local_start..local_end];
            let line = shape_rich_text_fragment(
                text,
                &run.style,
                rich_text_color_to_hsla(run.style.color, palette.text),
                window,
            );
            if let Some(background_color) = run.style.background_color {
                background_quads.push(fill(
                    Bounds::new(point(x, *y), size(line.width, line_height)),
                    rich_text_color_to_hsla(Some(background_color), palette.text),
                ));
            }
            push_selection_for_fragment(
                &range,
                &selection,
                x,
                *y,
                line_height,
                &line,
                palette,
                selection_quads,
            );
            if show_cursor
                && cursor_quad.is_none()
                && selection.start == selection.end
                && selection.start >= range.start
                && selection.start <= range.end
            {
                let local = selection.start.saturating_sub(range.start).min(range.len());
                *cursor_quad = Some(fill(
                    Bounds::new(
                        point(x + line.x_for_index(local), *y),
                        size(px(1.5), line_height),
                    ),
                    rgb(palette.accent),
                ));
            }
            let width = line.width;
            line_fragments.push(NoteRichTextFragmentLayout {
                byte_range: range.clone(),
                x,
                line,
            });
            line_end = range.end;
            x += width;
            local_start = local_end;
            if local_start < run.text.len() {
                push_wrapped_visual_line(
                    bounds,
                    *y,
                    line_height,
                    content_left,
                    line_start..line_end,
                    &mut line_fragments,
                    lines,
                );
                *y += line_height;
                line_start = line_end;
                x = content_left;
            }
        }
        offset += run.text.len();
    }

    if show_cursor
        && cursor_quad.is_none()
        && selection.start == selection.end
        && selection.start >= block_start
        && selection.start <= block_end
    {
        *cursor_quad = Some(fill(
            Bounds::new(point(x, *y), size(px(1.5), line_height)),
            rgb(palette.accent),
        ));
    }
    push_wrapped_visual_line(
        bounds,
        *y,
        line_height,
        content_left,
        line_start..block_end,
        &mut line_fragments,
        lines,
    );
    *y += line_height;
    block_end
}

/// 保存一条自动换行后的视觉行。
///
/// 业务意图：
/// - 视觉行是鼠标命中、拖选、复制和 IME 定位的共同基础；即使它来自同一个富文本块，也必须拥有自己的边界。
fn push_wrapped_visual_line(
    bounds: Bounds<Pixels>,
    y: Pixels,
    line_height: Pixels,
    content_left: Pixels,
    byte_range: Range<usize>,
    fragments: &mut Vec<NoteRichTextFragmentLayout>,
    lines: &mut Vec<NoteRichTextLineLayout>,
) {
    lines.push(NoteRichTextLineLayout {
        byte_range,
        bounds: Bounds::new(
            point(bounds.left(), y),
            size(bounds.size.width, line_height),
        ),
        content_left,
        fragments: std::mem::take(fragments),
    });
}

/// 返回自动换行优先使用的 UTF-8 断点。
///
/// 业务意图：
/// - 英文和路径类文本优先在空白字符后换行；中文等没有空格的文本按字符边界强制换行。
/// - 该函数只返回原始文本中的字节下标，不改写内容，因此复制和保存仍保留用户输入的空白。
pub(in crate::app) fn preferred_soft_wrap_end(text: &str, start: usize, tentative: usize) -> usize {
    let start = clamp_note_rich_text_boundary(text, start);
    let tentative = clamp_note_rich_text_boundary(text, tentative).min(text.len());
    if tentative <= start {
        return next_char_boundary_after(text, start);
    }
    if tentative >= text.len() {
        return text.len();
    }
    let slice = &text[start..tentative];
    for (offset, character) in slice.char_indices().rev() {
        if character.is_whitespace() {
            let candidate = start + offset + character.len_utf8();
            if candidate > start {
                return candidate;
            }
        }
    }
    tentative
}

/// 返回指定 UTF-8 边界后的下一个字符边界。
fn next_char_boundary_after(text: &str, start: usize) -> usize {
    let start = clamp_note_rich_text_boundary(text, start).min(text.len());
    text[start..]
        .chars()
        .next()
        .map(|character| start + character.len_utf8())
        .unwrap_or(text.len())
}

#[allow(clippy::too_many_arguments)]
/// 排版一组连续代码块行。
///
/// 业务意图：
/// - 富文本模型仍按块间换行保存内容，因此一个多行代码块在模型中表现为连续 `CodeBlock` 行。
/// - 渲染时把这些连续行合并成一个带语言标签、语法高亮和横向滚动条的视觉容器，避免每一行都出现独立边框。
///
/// 边界条件：
/// - 代码内容超宽时只绘制当前横向滚动范围内的文本片段，防止自绘文字越过代码块背景遮挡普通正文。
/// - 选区和光标只在可见片段内绘制；隐藏内容需要先横向滚动后再选中。
fn layout_code_block_group(
    block_index: usize,
    group_offset: usize,
    language: &str,
    text_lines: &[String],
    line_ranges: &[Range<usize>],
    selection: Range<usize>,
    show_cursor: bool,
    bounds: Bounds<Pixels>,
    y: &mut Pixels,
    palette: AppThemePalette,
    theme: EffectiveTheme,
    code_block_scrolls: &HashMap<usize, f32>,
    window: &mut Window,
    lines: &mut Vec<NoteRichTextLineLayout>,
    code_blocks: &mut Vec<NoteRichTextCodeBlockLayout>,
    decorations: &mut Vec<NoteRichTextDecorationFragment>,
    background_quads: &mut Vec<PaintQuad>,
    selection_quads: &mut Vec<PaintQuad>,
    cursor_quad: &mut Option<PaintQuad>,
) {
    let group_width =
        (bounds.right() - bounds.left() - px(NOTES_RICH_TEXT_HORIZONTAL_PADDING * 2.0))
            .max(px(120.0));
    let group_left = bounds.left() + px(NOTES_RICH_TEXT_HORIZONTAL_PADDING);
    let group_top = *y + px(NOTES_CODE_BLOCK_VERTICAL_GAP * 0.5);
    let line_height = px(NOTES_CODE_BLOCK_LINE_HEIGHT);
    let content_left = group_left + px(NOTES_CODE_BLOCK_HORIZONTAL_PADDING);
    let viewport_width =
        (group_width - px(NOTES_CODE_BLOCK_HORIZONTAL_PADDING * 2.0)).max(px(20.0));
    let highlighted_lines = highlight_app_code_block_with_language(
        &text_lines.join("\n"),
        (language != "text").then_some(language),
        theme,
    );
    let mut full_width = px(0.0);
    let mut full_lines = Vec::new();
    for (index, text) in text_lines.iter().enumerate() {
        let highlighted = highlighted_lines
            .get(index)
            .cloned()
            .unwrap_or(AppMarkdownCodeLine {
                text: text.clone(),
                highlights: Vec::new(),
            });
        let line = shape_code_line(&highlighted, text, 0, text.len(), palette, window);
        full_width = full_width.max(line.width);
        full_lines.push((highlighted, line));
    }
    let max_scroll = (full_width - viewport_width).max(px(0.0));
    let scroll_left = px(code_block_scrolls
        .get(&block_index)
        .copied()
        .unwrap_or(0.0)
        .clamp(0.0, f32::from(max_scroll)));
    let has_horizontal_scroll = full_width > viewport_width;
    let group_height = px(code_block_container_height(
        text_lines.len(),
        has_horizontal_scroll,
    ));
    let group_bounds = Bounds::new(
        point(group_left, group_top),
        size(group_width, group_height),
    );
    let mut code_background = rgb(palette.panel);
    code_background.a = 0.92;
    background_quads.push(fill(group_bounds, code_background));

    let label_text = if language.is_empty() {
        "text"
    } else {
        language
    };
    let label_line = shape_code_label(label_text, palette, window);
    let language_label_bounds = Bounds::new(
        point(
            group_left + px(NOTES_CODE_BLOCK_HORIZONTAL_PADDING * 0.5),
            group_top + px(4.0),
        ),
        size(
            label_line.width + px(NOTES_CODE_BLOCK_HORIZONTAL_PADDING),
            px(NOTES_CODE_BLOCK_HEADER_HEIGHT - 6.0),
        ),
    );
    background_quads.push(fill(language_label_bounds, rgb(palette.surface)));
    decorations.push(NoteRichTextDecorationFragment {
        x: language_label_bounds.left() + px(NOTES_CODE_BLOCK_HORIZONTAL_PADDING * 0.5),
        y: group_top + px(4.0),
        line_height: px(NOTES_CODE_BLOCK_HEADER_HEIGHT - 4.0),
        line: label_line,
    });

    let mut line_top = group_top
        + px(NOTES_CODE_BLOCK_HEADER_HEIGHT)
        + px(NOTES_CODE_BLOCK_VERTICAL_PADDING * 0.4);
    for (line_index, (highlighted, full_line)) in full_lines.into_iter().enumerate() {
        let range = line_ranges
            .get(line_index)
            .cloned()
            .unwrap_or(group_offset..group_offset);
        let text = text_lines.get(line_index).map(String::as_str).unwrap_or("");
        let mut fragments = Vec::new();
        if !text.is_empty() {
            let visible_start =
                clamp_note_rich_text_boundary(text, full_line.closest_index_for_x(scroll_left));
            let mut visible_end = clamp_note_rich_text_boundary(
                text,
                full_line.closest_index_for_x((scroll_left + viewport_width).min(full_line.width)),
            );
            if visible_end <= visible_start {
                visible_end = text.len();
            }
            let line = shape_code_line(
                &highlighted,
                text,
                visible_start,
                visible_end.min(text.len()),
                palette,
                window,
            );
            let fragment_range =
                range.start + visible_start..range.start + visible_end.min(text.len());
            let fragment_x =
                content_left - (scroll_left - full_line.x_for_index(visible_start)).max(px(0.0));
            push_selection_for_fragment(
                &fragment_range,
                &selection,
                fragment_x,
                line_top,
                line_height,
                &line,
                palette,
                selection_quads,
            );
            if show_cursor
                && cursor_quad.is_none()
                && selection.start == selection.end
                && selection.start >= fragment_range.start
                && selection.start <= fragment_range.end
            {
                let local = selection.start.saturating_sub(fragment_range.start);
                *cursor_quad = Some(fill(
                    Bounds::new(
                        point(fragment_x + line.x_for_index(local), line_top),
                        size(px(1.5), line_height),
                    ),
                    rgb(palette.accent),
                ));
            }
            fragments.push(NoteRichTextFragmentLayout {
                byte_range: fragment_range,
                x: fragment_x,
                line,
            });
        }
        if show_cursor
            && cursor_quad.is_none()
            && selection.start == selection.end
            && selection.start >= range.start
            && selection.start <= range.end
        {
            *cursor_quad = Some(fill(
                Bounds::new(point(content_left, line_top), size(px(1.5), line_height)),
                rgb(palette.accent),
            ));
        }
        lines.push(NoteRichTextLineLayout {
            byte_range: range,
            bounds: Bounds::new(point(group_left, line_top), size(group_width, line_height)),
            content_left,
            fragments,
        });
        line_top += line_height;
    }
    let content_bottom = line_top;

    let (scrollbar_track, scrollbar_thumb) = if has_horizontal_scroll {
        let track_left = content_left;
        let track_top = content_bottom + px(4.0);
        let track_width = viewport_width;
        let track_bounds = Bounds::new(
            point(track_left, track_top),
            size(track_width, px(NOTES_CODE_BLOCK_SCROLLBAR_HEIGHT)),
        );
        let thumb_width = (track_width * (viewport_width / full_width))
            .max(px(28.0))
            .min(track_width);
        let scroll_ratio = if max_scroll > px(0.0) {
            scroll_left / max_scroll
        } else {
            0.0
        };
        let thumb_left = track_left + (track_width - thumb_width) * scroll_ratio;
        let thumb_bounds = Bounds::new(
            point(thumb_left, track_top + px(2.0)),
            size(thumb_width, px(NOTES_CODE_BLOCK_SCROLLBAR_HEIGHT - 4.0)),
        );
        let mut track_color = rgb(palette.border);
        track_color.a = 0.5;
        background_quads.push(fill(track_bounds, track_color));
        background_quads.push(fill(thumb_bounds, rgb(palette.muted_text)));
        (Some(track_bounds), Some(thumb_bounds))
    } else {
        (None, None)
    };

    code_blocks.push(NoteRichTextCodeBlockLayout {
        block_index,
        byte_range: group_offset
            ..line_ranges
                .last()
                .map(|range| range.end)
                .unwrap_or(group_offset),
        bounds: group_bounds,
        content_bottom,
        language_label_bounds,
        viewport_width,
        content_width: full_width,
        scrollbar_track,
        scrollbar_thumb,
    });
    *y = group_bounds.bottom() + px(NOTES_CODE_BLOCK_VERTICAL_GAP * 0.5);
}

fn paint_rich_text_fragment(
    fragment: &NoteRichTextFragmentLayout,
    top: Pixels,
    line_height: Pixels,
    window: &mut Window,
    context: &mut App,
) {
    let origin = point(fragment.x, top);
    fragment
        .line
        .paint(origin, line_height, window, context)
        .ok();
}

/// 返回笔记正文排版使用的有效边界。
///
/// 业务意图：
/// - GPUI 滚动容器中的自绘元素在部分场景会拿到偏窄的子元素 bounds，导致右侧仍有可视空间时提前换行。
/// - 笔记正文所在页面本身占据主窗口右侧区域，因此排版宽度以元素 bounds 和窗口右边界两者中的较大者为准。
fn note_rich_text_layout_bounds(bounds: Bounds<Pixels>, window: &Window) -> Bounds<Pixels> {
    let viewport_width = window.viewport_size().width;
    let viewport_available = (viewport_width - bounds.left()).max(bounds.size.width);
    Bounds::new(bounds.origin, size(viewport_available, bounds.size.height))
}

/// 为单个可见文本片段生成选区背景。
///
/// 实现原因：
/// - 普通富文本和代码块可见片段都需要相同的 UTF-8 范围交集逻辑，集中处理可以避免中文和 emoji 选区被截断。
fn push_selection_for_fragment(
    range: &Range<usize>,
    selection: &Range<usize>,
    x: Pixels,
    y: Pixels,
    line_height: Pixels,
    line: &ShapedLine,
    palette: AppThemePalette,
    selection_quads: &mut Vec<PaintQuad>,
) {
    if selection.start >= selection.end || !ranges_intersect(range, selection) {
        return;
    }
    let local_start = selection.start.saturating_sub(range.start).min(range.len());
    let local_end = selection.end.saturating_sub(range.start).min(range.len());
    if local_start >= local_end {
        return;
    }
    let mut selection_color = rgb(palette.accent);
    selection_color.a = 0.30;
    selection_quads.push(fill(
        Bounds::from_corners(
            point(x + line.x_for_index(local_start), y),
            point(x + line.x_for_index(local_end), y + line_height),
        ),
        selection_color,
    ));
}

/// 按代码块规则塑形一行可见代码。
///
/// 业务意图：
/// - 代码块始终使用应用内置等宽字体和 syntect 高亮颜色，不读取富文本 run 的字号、颜色、粗斜体等样式。
/// - `start..end` 表示横向滚动后当前可见的 UTF-8 字节范围，用于避免超宽代码行绘制到容器外。
fn shape_code_line(
    highlighted: &AppMarkdownCodeLine,
    fallback_text: &str,
    start: usize,
    end: usize,
    palette: AppThemePalette,
    window: &mut Window,
) -> ShapedLine {
    let text = if highlighted.text == fallback_text {
        highlighted.text.as_str()
    } else {
        fallback_text
    };
    let start = clamp_note_rich_text_boundary(text, start);
    let end = clamp_note_rich_text_boundary(text, end)
        .max(start)
        .min(text.len());
    let visible = &text[start..end];
    let mut runs = Vec::new();
    let mut cursor = start;
    for (highlight_range, highlight) in &highlighted.highlights {
        let overlap_start = highlight_range.start.max(start).min(end);
        let overlap_end = highlight_range.end.min(end).max(overlap_start);
        if overlap_start > cursor {
            runs.push(code_text_run(
                overlap_start - cursor,
                rgb(palette.text).into(),
                window,
            ));
        }
        if overlap_end > overlap_start {
            runs.push(code_text_run(
                overlap_end - overlap_start,
                highlight.color.unwrap_or_else(|| rgb(palette.text).into()),
                window,
            ));
        }
        cursor = cursor.max(overlap_end);
    }
    if end > cursor {
        runs.push(code_text_run(
            end - cursor,
            rgb(palette.text).into(),
            window,
        ));
    }
    if runs.is_empty() && !visible.is_empty() {
        runs.push(code_text_run(
            visible.len(),
            rgb(palette.text).into(),
            window,
        ));
    }
    window.text_system().shape_line(
        SharedString::from(visible.to_string()),
        px(NOTE_RICH_TEXT_DEFAULT_FONT_SIZE_PX as f32),
        &runs,
        None,
    )
}

/// 创建代码块文本 run。
fn code_text_run(len: usize, color: gpui::Hsla, window: &mut Window) -> TextRun {
    let mut font = window.text_style().font();
    // 代码块固定使用应用内置等宽字体，避免 macOS / Windows 默认字体差异导致横向滚动宽度不一致。
    font.family = LOG_VIEWER_FONT_FAMILY.into();
    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// 塑形代码块语言标签。
fn shape_code_label(text: &str, palette: AppThemePalette, window: &mut Window) -> ShapedLine {
    let mut style = NoteRichTextStyle::default();
    style.font_size_px = 12;
    shape_rich_text_fragment(text, &style, rgb(palette.muted_text).into(), window)
}

fn shape_rich_text_fragment(
    text: &str,
    style: &NoteRichTextStyle,
    color: gpui::Hsla,
    window: &mut Window,
) -> ShapedLine {
    let mut font = window.text_style().font();
    // 笔记正文显式使用应用内置 JetBrains Mono 字体族，避免不同系统默认 UI 字体和字体缺失
    // 让 Regular / Italic face 匹配结果不一致。Italic 字体启动时和 Regular 一起注册。
    font.family = LOG_VIEWER_FONT_FAMILY.into();
    if style.bold {
        font = font.bold();
    }
    if style.italic {
        font = font.italic();
    }
    let run = TextRun {
        len: text.len(),
        font,
        color,
        background_color: None,
        underline: style.underline.then_some(UnderlineStyle {
            color: Some(color),
            thickness: px(1.0),
            wavy: false,
        }),
        strikethrough: style.strikethrough.then_some(StrikethroughStyle {
            color: Some(color),
            thickness: px(1.0),
        }),
    };
    window.text_system().shape_line(
        SharedString::from(text.to_string()),
        px(style.font_size_px as f32),
        &[run],
        None,
    )
}

fn rich_text_color_to_hsla(color: Option<NoteRichTextColor>, fallback: u32) -> gpui::Hsla {
    color
        .map(|color| rgb(((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32))
        .unwrap_or_else(|| rgb(fallback))
        .into()
}

pub(in crate::app) fn rich_text_block_line_height(block: &NoteRichTextBlock) -> f32 {
    if block.kind == NoteRichTextBlockKind::CodeBlock {
        return NOTES_CODE_BLOCK_LINE_HEIGHT;
    }
    let max_size = block
        .runs
        .iter()
        .map(|run| run.style.font_size_px)
        .max()
        .unwrap_or(NOTE_RICH_TEXT_DEFAULT_FONT_SIZE_PX) as f32;
    (max_size * 1.55).max(NOTES_TEXT_LINE_HEIGHT)
}

/// 计算代码块容器高度。
///
/// 业务意图：
/// - 自绘元素需要在布局阶段提前给出总高度，否则外层滚动容器无法正确显示纵向滚动条。
pub(in crate::app) fn code_block_visual_height(line_count: usize) -> f32 {
    NOTES_CODE_BLOCK_VERTICAL_GAP + code_block_container_height(line_count, true)
}

/// 计算代码块灰底容器自身高度。
///
/// 业务意图：
/// - 代码块与正文之间的外部间距由布局流程在容器上下各加一半，容器内部只保留语言标签、代码行和必要 padding。
/// - 只有内容真正超宽时才预留横向滚动条高度，避免普通短代码块底部出现大块空白。
fn code_block_container_height(line_count: usize, has_horizontal_scroll: bool) -> f32 {
    let scrollbar_height = if has_horizontal_scroll {
        NOTES_CODE_BLOCK_SCROLLBAR_HEIGHT + 10.0
    } else {
        6.0
    };
    NOTES_CODE_BLOCK_HEADER_HEIGHT
        + NOTES_CODE_BLOCK_VERTICAL_PADDING * 0.4
        + NOTES_CODE_BLOCK_LINE_HEIGHT * line_count.max(1) as f32
        + scrollbar_height
}

/// 判断窗口坐标是否落在边界内。
fn bounds_contains_point(bounds: Bounds<Pixels>, point: Point<Pixels>) -> bool {
    point.x >= bounds.left()
        && point.x <= bounds.right()
        && point.y >= bounds.top()
        && point.y <= bounds.bottom()
}

fn ranges_intersect(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}
