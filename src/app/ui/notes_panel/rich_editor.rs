// 笔记富文本编辑器 UI 状态和自绘元素。
//
// 业务意图：
// - 该文件承载笔记正文的纯 Rust / GPUI 富文本编辑能力，避免引入 WebView 或 JS 构建链。
// - 文档模型来自 `crate::notes`，本文件只保存焦点、选区、撤销栈、排版缓存和工具栏临时状态。
//
// 边界条件：
// - 文本位置使用富文本文档的线性 UTF-8 字节下标；输入法回调中的 UTF-16 下标会在 shell 适配层转换。
// - 第一版不做复杂软换行，超长行横向裁切；字号、颜色、加粗等局部样式仍按 run 自绘并参与复制粘贴。

use std::ops::Range;

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
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            font_size_menu_open: false,
            color_menu_open: false,
            background_color_menu_open: false,
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
        self.last_layouts.clear();
        self.last_bounds = None;
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
        let mut height = NOTES_RICH_TEXT_VERTICAL_PADDING * 2.0;
        for block in &self.document.blocks {
            height += rich_text_block_line_height(block);
        }
        height.max(220.0)
    }

    /// 保存最近一次排版结果。
    pub(in crate::app) fn store_layout(
        &mut self,
        layouts: Vec<NoteRichTextLineLayout>,
        bounds: Bounds<Pixels>,
    ) {
        self.last_layouts = layouts;
        self.last_bounds = Some(bounds);
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
        let (document, selection, cursor_visible, focused) = {
            let view = self.view.read(context);
            (
                view.notes.rich_editor.document.clone(),
                view.notes.rich_editor.clamped_selection(),
                view.search_text_cursor_visible(),
                view.notes.rich_editor.focus.is_focused(window),
            )
        };
        Some(layout_rich_text_document(
            &document,
            selection,
            self.editable && focused && cursor_visible,
            bounds,
            self.palette,
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
        self.view.update(context, |view, _context| {
            view.notes.rich_editor.store_layout(prepaint.lines, bounds);
        });
    }
}

pub(in crate::app) struct NoteRichTextPrepaint {
    lines: Vec<NoteRichTextLineLayout>,
    decorations: Vec<NoteRichTextDecorationFragment>,
    background_quads: Vec<PaintQuad>,
    selection_quads: Vec<PaintQuad>,
    cursor_quad: Option<PaintQuad>,
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
    window: &mut Window,
) -> NoteRichTextPrepaint {
    let text = document.plain_text();
    let selection = clamp_note_rich_text_range(&text, selection);
    let mut lines = Vec::new();
    let mut decorations = Vec::new();
    let mut background_quads = Vec::new();
    let mut selection_quads = Vec::new();
    let mut cursor_quad = None;
    let mut y = bounds.top() + px(NOTES_RICH_TEXT_VERTICAL_PADDING);
    let mut offset = 0;
    let mut ordered_index = 1;

    for (block_index, block) in document.blocks.iter().enumerate() {
        let line_height = px(rich_text_block_line_height(block));
        let mut x = bounds.left() + px(NOTES_RICH_TEXT_HORIZONTAL_PADDING);
        let prefix_text = match block.kind {
            NoteRichTextBlockKind::Paragraph => None,
            NoteRichTextBlockKind::UnorderedListItem => Some("• ".to_string()),
            NoteRichTextBlockKind::OrderedListItem => {
                let prefix = format!("{ordered_index}. ");
                ordered_index += 1;
                Some(prefix)
            }
        };
        if let Some(prefix_text) = prefix_text {
            let prefix_line = shape_rich_text_fragment(
                &prefix_text,
                &NoteRichTextStyle::default(),
                rgb(palette.muted_text).into(),
                window,
            );
            let width = prefix_line.width;
            decorations.push(NoteRichTextDecorationFragment {
                x,
                y,
                line_height,
                line: prefix_line,
            });
            x += width;
        }
        let content_left = x;
        let block_text = block.plain_text();
        let block_start = offset;
        let block_end = block_start + block_text.len();
        let mut fragments = Vec::new();
        for run in &block.runs {
            if run.text.is_empty() {
                continue;
            }
            let line = shape_rich_text_fragment(
                &run.text,
                &run.style,
                rich_text_color_to_hsla(run.style.color, palette.text),
                window,
            );
            let range = offset..offset + run.text.len();
            if let Some(background_color) = run.style.background_color {
                background_quads.push(fill(
                    Bounds::new(point(x, y), size(line.width, line_height)),
                    rich_text_color_to_hsla(Some(background_color), palette.text),
                ));
            }
            if selection.start < selection.end && ranges_intersect(&range, &selection) {
                let local_start = selection.start.saturating_sub(range.start).min(range.len());
                let local_end = selection.end.saturating_sub(range.start).min(range.len());
                if local_start < local_end {
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
            }
            if show_cursor
                && cursor_quad.is_none()
                && selection.start == selection.end
                && selection.start >= range.start
                && selection.start <= range.end
            {
                let local = selection.start.saturating_sub(range.start).min(range.len());
                cursor_quad = Some(fill(
                    Bounds::new(
                        point(x + line.x_for_index(local), y),
                        size(px(1.5), line_height),
                    ),
                    rgb(palette.accent),
                ));
            }
            let width = line.width;
            fragments.push(NoteRichTextFragmentLayout {
                byte_range: range.clone(),
                x,
                line,
            });
            x += width;
            offset += run.text.len();
        }
        if show_cursor
            && cursor_quad.is_none()
            && selection.start == selection.end
            && selection.start >= block_start
            && selection.start <= block_end
        {
            cursor_quad = Some(fill(
                Bounds::new(point(x, y), size(px(1.5), line_height)),
                rgb(palette.accent),
            ));
        }
        lines.push(NoteRichTextLineLayout {
            byte_range: block_start..block_end,
            bounds: Bounds::new(
                point(bounds.left(), y),
                size(bounds.right() - bounds.left(), line_height),
            ),
            content_left,
            fragments,
        });
        offset = block_end + usize::from(block_index + 1 < document.blocks.len());
        y += line_height;
    }

    NoteRichTextPrepaint {
        lines,
        decorations,
        background_quads,
        selection_quads,
        cursor_quad,
    }
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
    let max_size = block
        .runs
        .iter()
        .map(|run| run.style.font_size_px)
        .max()
        .unwrap_or(NOTE_RICH_TEXT_DEFAULT_FONT_SIZE_PX) as f32;
    (max_size * 1.55).max(NOTES_TEXT_LINE_HEIGHT)
}

fn ranges_intersect(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}
