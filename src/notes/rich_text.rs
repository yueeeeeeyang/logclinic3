// 笔记富文本文档模型。
//
// 业务意图：
// - 笔记正文仍由自研富文本编辑器承载，但持久化文件改为 Markdown，因此这里同时负责富文本 JSON 兼容和 Markdown 双向转换。
// - 本模块不依赖 GPUI，负责文本、块、样式、选区变换和序列化；UI 只消费这些纯状态，避免渲染代码直接改持久化文本。
//
// 边界条件：
// - 所有选区位置都使用线性 UTF-8 字节下标，段落之间用一个 `\n` 映射，确保中文和 emoji 不会被截断。
// - Markdown 只做基础语义映射；颜色、字号、背景色、下划线等 Markdown 不支持的样式在导出时会丢弃。

use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};

use super::*;

/// 富文本 JSON 版本号。
///
/// 业务意图：
/// - 富文本 JSON 只作为旧笔记迁移和编辑器内部兼容格式；未来增加表格、对齐、链接等能力时可按文档版本迁移。
pub(crate) const NOTE_RICH_TEXT_JSON_VERSION: u32 = 1;

/// 富文本默认字号。
pub(crate) const NOTE_RICH_TEXT_DEFAULT_FONT_SIZE_PX: u32 = 14;

/// 富文本撤销/重做最多保存的快照数。
///
/// 业务意图：
/// - 单篇笔记最大 1 MiB，撤销栈必须有上限，避免连续输入或批量粘贴导致 UI 进程内存无限增长。
pub(crate) const NOTE_RICH_TEXT_HISTORY_LIMIT: usize = 100;

/// 富文本块类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoteRichTextBlockKind {
    /// 普通段落。
    Paragraph,
    /// 无序列表项。
    UnorderedListItem,
    /// 有序列表项。
    OrderedListItem,
    /// 代码块。
    ///
    /// 业务意图：
    /// - 代码块以块级语义保存，编辑和查看时使用等宽字体、语法高亮和独立横向滚动。
    /// - 代码块内部不解析 Markdown，也不支持局部富文本样式，避免代码内容被普通正文工具栏改坏。
    CodeBlock,
}

/// 富文本颜色。
///
/// 业务意图：
/// - 使用 RGB 三元组而不是主题色名称保存，保证用户设置的局部文字颜色跨主题和跨平台稳定恢复。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NoteRichTextColor {
    /// 红色通道。
    pub(crate) r: u8,
    /// 绿色通道。
    pub(crate) g: u8,
    /// 蓝色通道。
    pub(crate) b: u8,
}

impl NoteRichTextColor {
    /// 创建 RGB 颜色。
    pub(crate) const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// 富文本 run 样式。
///
/// 业务意图：
/// - run 样式只保存第一版工具栏支持的内联属性；未设置文字或背景颜色时分别跟随主题正文色和透明背景，
///   避免暗色/亮色主题切换后旧笔记出现不可读的固定色块。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NoteRichTextStyle {
    /// 字号，单位为 GPUI 逻辑像素。
    pub(crate) font_size_px: u32,
    /// 是否加粗。
    pub(crate) bold: bool,
    /// 是否斜体。
    pub(crate) italic: bool,
    /// 是否下划线。
    pub(crate) underline: bool,
    /// 是否删除线。
    pub(crate) strikethrough: bool,
    /// 文字颜色；`None` 表示跟随主题正文颜色。
    pub(crate) color: Option<NoteRichTextColor>,
    /// 背景颜色；`None` 表示透明背景。
    ///
    /// 边界条件：
    /// - 该字段是在富文本 JSON v1 已落库后补充的能力，因此必须允许旧 JSON 缺失字段并回退为透明。
    #[serde(default)]
    pub(crate) background_color: Option<NoteRichTextColor>,
}

impl Default for NoteRichTextStyle {
    fn default() -> Self {
        Self {
            font_size_px: NOTE_RICH_TEXT_DEFAULT_FONT_SIZE_PX,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            color: None,
            background_color: None,
        }
    }
}

/// 样式修改补丁。
///
/// 业务意图：
/// - 工具栏命令可能只改一个属性，例如字号或加粗；使用补丁可以避免调用方为了一个属性重写整套样式。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct NoteRichTextStylePatch {
    /// 待设置字号。
    pub(crate) font_size_px: Option<u32>,
    /// 待设置加粗状态。
    pub(crate) bold: Option<bool>,
    /// 待设置斜体状态。
    pub(crate) italic: Option<bool>,
    /// 待设置下划线状态。
    pub(crate) underline: Option<bool>,
    /// 待设置删除线状态。
    pub(crate) strikethrough: Option<bool>,
    /// 待设置颜色；外层 `None` 表示不改，内层 `None` 表示恢复主题色。
    pub(crate) color: Option<Option<NoteRichTextColor>>,
    /// 待设置背景颜色；外层 `None` 表示不改，内层 `None` 表示清除背景色。
    pub(crate) background_color: Option<Option<NoteRichTextColor>>,
}

impl NoteRichTextStyle {
    /// 应用一个局部样式补丁。
    pub(crate) fn apply_patch(&mut self, patch: &NoteRichTextStylePatch) {
        if let Some(font_size_px) = patch.font_size_px {
            self.font_size_px = font_size_px;
        }
        if let Some(bold) = patch.bold {
            self.bold = bold;
        }
        if let Some(italic) = patch.italic {
            self.italic = italic;
        }
        if let Some(underline) = patch.underline {
            self.underline = underline;
        }
        if let Some(strikethrough) = patch.strikethrough {
            self.strikethrough = strikethrough;
        }
        if let Some(color) = patch.color {
            self.color = color;
        }
        if let Some(background_color) = patch.background_color {
            self.background_color = background_color;
        }
    }
}

/// 富文本 run。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NoteRichTextRun {
    /// run 内连续文本。
    pub(crate) text: String,
    /// run 样式。
    pub(crate) style: NoteRichTextStyle,
}

/// 富文本块。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NoteRichTextBlock {
    /// 块类型。
    pub(crate) kind: NoteRichTextBlockKind,
    /// 代码块语言标识。
    ///
    /// 边界条件：
    /// - 该字段只对 `CodeBlock` 生效；普通段落和列表会在 `normalize` 中清空，保证旧 JSON 缺失字段也能读取。
    /// - 第一版语言来自 UI 常用下拉，未知语言由高亮层降级为纯文本。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) code_language: Option<String>,
    /// 块内样式 run。
    pub(crate) runs: Vec<NoteRichTextRun>,
}

impl NoteRichTextBlock {
    /// 返回块内纯文本。
    pub(crate) fn plain_text(&self) -> String {
        self.runs
            .iter()
            .map(|run| run.text.as_str())
            .collect::<String>()
    }
}

/// 富文本 JSON v1 根文档。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NoteRichTextDocument {
    /// JSON 文档版本。
    pub(crate) version: u32,
    /// 文档块列表。
    pub(crate) blocks: Vec<NoteRichTextBlock>,
}

#[derive(Clone)]
enum RichTextUnit {
    Text {
        character: char,
        style: NoteRichTextStyle,
        block_kind: NoteRichTextBlockKind,
        code_language: Option<String>,
    },
    Break {
        next_kind: NoteRichTextBlockKind,
        next_code_language: Option<String>,
    },
}

impl NoteRichTextDocument {
    /// 创建空富文本文档。
    pub(crate) fn empty() -> Self {
        Self {
            version: NOTE_RICH_TEXT_JSON_VERSION,
            blocks: vec![NoteRichTextBlock {
                kind: NoteRichTextBlockKind::Paragraph,
                code_language: None,
                runs: Vec::new(),
            }],
        }
    }

    /// 按默认样式把普通文本转换成富文本文档。
    pub(crate) fn from_plain_text(text: &str) -> Self {
        Self::from_plain_text_with_style(text, NoteRichTextStyle::default())
    }

    /// 按指定样式把普通文本转换成富文本文档。
    pub(crate) fn from_plain_text_with_style(text: &str, style: NoteRichTextStyle) -> Self {
        Self::from_plain_text_with_style_and_block(
            text,
            style,
            NoteRichTextBlockKind::Paragraph,
            None,
        )
    }

    /// 按指定样式和块类型把普通文本转换成富文本文档。
    ///
    /// 业务意图：
    /// - 编辑器在列表项或代码块中按 Enter / 粘贴多行文本时，新产生的块应继承当前位置的块语义。
    /// - 代码块语言也需要继承，否则在代码块中插入换行会丢失语法高亮上下文。
    fn from_plain_text_with_style_and_block(
        text: &str,
        style: NoteRichTextStyle,
        kind: NoteRichTextBlockKind,
        code_language: Option<String>,
    ) -> Self {
        let mut blocks = text
            .split('\n')
            .map(|line| NoteRichTextBlock {
                kind,
                code_language: (kind == NoteRichTextBlockKind::CodeBlock)
                    .then(|| code_language.clone())
                    .flatten(),
                runs: if line.is_empty() {
                    Vec::new()
                } else {
                    vec![NoteRichTextRun {
                        text: line.to_string(),
                        style: style.clone(),
                    }]
                },
            })
            .collect::<Vec<_>>();
        if blocks.is_empty() {
            blocks.push(NoteRichTextBlock {
                kind,
                code_language: (kind == NoteRichTextBlockKind::CodeBlock)
                    .then_some(code_language)
                    .flatten(),
                runs: Vec::new(),
            });
        }
        let mut document = Self {
            version: NOTE_RICH_TEXT_JSON_VERSION,
            blocks,
        };
        document.normalize();
        document
    }

    /// 从旧存储保存的 JSON 解析富文本文档。
    pub(crate) fn from_json(raw: &str) -> Result<Self, String> {
        let mut document: Self =
            serde_json::from_str(raw).map_err(|error| format!("解析富文本笔记失败：{error}"))?;
        if document.version != NOTE_RICH_TEXT_JSON_VERSION {
            return Err(format!("富文本笔记版本 {} 暂不支持", document.version));
        }
        document.normalize();
        Ok(document)
    }

    /// 序列化为旧富文本 JSON 字段。
    #[cfg(test)]
    pub(crate) fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| format!("序列化富文本笔记失败：{error}"))
    }

    /// 从 Markdown 文本构建富文本文档。
    ///
    /// 业务意图：
    /// - 物理文件存储以 Markdown 为真实正文格式，但编辑器仍使用富文本模型；加载文件时需要把常用 Markdown 语义映射到现有块和 run。
    /// - 第一版只做基础映射，确保外部 `.md` 文件可读可编辑；表格、引用、链接等复杂结构会降级为普通可见文本。
    pub(crate) fn from_markdown(markdown: &str) -> Self {
        note_rich_text_document_from_markdown(markdown)
    }

    /// 导出为 Markdown 文本。
    ///
    /// 业务意图：
    /// - 保存笔记时写入真实 `.md` 文件，外部编辑器应能直接打开；因此富文本需要导出为可读的 Markdown。
    /// - Markdown 不支持的局部颜色、字号、背景色和下划线会被丢弃，避免写入私有扩展污染用户文件。
    pub(crate) fn to_markdown(&self) -> String {
        note_rich_text_document_to_markdown(self)
    }

    /// 返回线性纯文本，块之间用换行连接。
    pub(crate) fn plain_text(&self) -> String {
        self.blocks
            .iter()
            .map(NoteRichTextBlock::plain_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 返回指定 UTF-8 范围对应的纯文本。
    pub(crate) fn plain_text_range(&self, range: Range<usize>) -> String {
        let text = self.plain_text();
        let range = clamp_note_rich_text_range(&text, range);
        text[range].to_string()
    }

    /// 返回线性文本总字节数。
    pub(crate) fn len(&self) -> usize {
        self.plain_text().len()
    }

    /// 判断文档是否为空。
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.plain_text().is_empty()
    }

    /// 合并相邻同样式 run，并保证文档至少有一个段落。
    pub(crate) fn normalize(&mut self) {
        if self.version == 0 {
            self.version = NOTE_RICH_TEXT_JSON_VERSION;
        }
        for block in &mut self.blocks {
            let mut merged: Vec<NoteRichTextRun> = Vec::new();
            for run in block.runs.drain(..) {
                if run.text.is_empty() {
                    continue;
                }
                if let Some(previous) = merged.last_mut()
                    && previous.style == run.style
                {
                    previous.text.push_str(&run.text);
                    continue;
                }
                merged.push(run);
            }
            block.runs = merged;
            if block.kind != NoteRichTextBlockKind::CodeBlock {
                block.code_language = None;
            } else if block
                .code_language
                .as_ref()
                .is_some_and(|language| language.trim().is_empty())
            {
                block.code_language = None;
            }
        }
        if self.blocks.is_empty() {
            self.blocks.push(NoteRichTextBlock {
                kind: NoteRichTextBlockKind::Paragraph,
                code_language: None,
                runs: Vec::new(),
            });
        }
    }

    /// 使用片段替换指定线性范围。
    ///
    /// 实现原因：
    /// - 将文档拆成“字符单元 + 换行单元”后再重组，可以同时支持中文/emoji 字符边界、跨段落删除和富文本粘贴。
    pub(crate) fn replace_range_with_document(
        &mut self,
        range: Range<usize>,
        fragment: &NoteRichTextDocument,
    ) {
        let text = self.plain_text();
        let range = clamp_note_rich_text_range(&text, range);
        let (kind, code_language) = self.block_meta_at_position(range.start);
        let units = self.units();
        let fragment_units = fragment.units();
        let mut next_units = Vec::new();
        append_units_in_range(&units, 0..range.start, &mut next_units);
        next_units.extend(fragment_units.into_iter().map(|(_, unit)| unit));
        append_units_in_range(&units, range.end..text.len(), &mut next_units);
        let (first_kind, first_code_language) =
            first_meta_for_rebuild(&self.blocks, &next_units).unwrap_or((kind, code_language));
        *self = Self::from_units(first_kind, first_code_language, next_units);
    }

    /// 使用普通文本替换指定范围，插入文本继承调用方传入的样式。
    pub(crate) fn replace_range_with_text(
        &mut self,
        range: Range<usize>,
        text: &str,
        style: NoteRichTextStyle,
    ) {
        let current = self.plain_text();
        let range = clamp_note_rich_text_range(&current, range);
        let (kind, code_language) = self.block_meta_at_position(range.start);
        let fragment = Self::from_plain_text_with_style_and_block(text, style, kind, code_language);
        self.replace_range_with_document(range, &fragment);
    }

    /// 删除指定范围。
    pub(crate) fn delete_range(&mut self, range: Range<usize>) {
        self.replace_range_with_text(range, "", NoteRichTextStyle::default());
    }

    /// 截取一段富文本片段，供应用内复制粘贴保留样式。
    pub(crate) fn fragment_for_range(&self, range: Range<usize>) -> Self {
        let text = self.plain_text();
        let range = clamp_note_rich_text_range(&text, range);
        let units = self.units();
        let mut selected_units = Vec::new();
        append_units_in_range(&units, range.clone(), &mut selected_units);
        let first_kind = self.kind_at_position(range.start);
        let (_, first_code_language) = self.block_meta_at_position(range.start);
        Self::from_units(first_kind, first_code_language, selected_units)
    }

    /// 对选区应用内联样式。
    pub(crate) fn apply_style_patch(
        &mut self,
        range: Range<usize>,
        patch: &NoteRichTextStylePatch,
    ) {
        let text = self.plain_text();
        let range = clamp_note_rich_text_range(&text, range);
        if range.start >= range.end {
            return;
        }
        let mut next_units = Vec::new();
        for (unit_range, unit) in self.units() {
            match unit {
                RichTextUnit::Text {
                    character,
                    mut style,
                    block_kind,
                    code_language,
                } if block_kind != NoteRichTextBlockKind::CodeBlock
                    && ranges_intersect(&unit_range, &range) =>
                {
                    style.apply_patch(patch);
                    next_units.push(RichTextUnit::Text {
                        character,
                        style,
                        block_kind,
                        code_language,
                    });
                }
                other => next_units.push(other),
            }
        }
        let (first_kind, first_code_language) = first_meta_for_rebuild(&self.blocks, &next_units)
            .unwrap_or((NoteRichTextBlockKind::Paragraph, None));
        *self = Self::from_units(first_kind, first_code_language, next_units);
    }

    /// 切换选区覆盖块的列表类型。
    pub(crate) fn toggle_blocks_kind(
        &mut self,
        range: Range<usize>,
        target_kind: NoteRichTextBlockKind,
    ) {
        let covered = self.block_indices_for_range(range);
        if covered.is_empty() {
            return;
        }
        let should_clear = covered
            .iter()
            .all(|index| self.blocks[*index].kind == target_kind);
        for index in covered {
            self.blocks[index].kind = if should_clear {
                NoteRichTextBlockKind::Paragraph
            } else {
                target_kind
            };
            if self.blocks[index].kind != NoteRichTextBlockKind::CodeBlock {
                self.blocks[index].code_language = None;
            } else if self.blocks[index].code_language.is_none() {
                self.blocks[index].code_language = Some("text".to_string());
            }
        }
    }

    /// 设置选区覆盖代码块的语言。
    ///
    /// 业务意图：
    /// - 语言下拉只影响代码块，普通正文选择时不应该悄悄写入无效字段。
    pub(crate) fn set_code_language_for_range(&mut self, range: Range<usize>, language: &str) {
        let covered = self.block_indices_for_range(range);
        for index in covered {
            if self.blocks[index].kind == NoteRichTextBlockKind::CodeBlock {
                self.blocks[index].code_language = Some(language.to_string());
            }
        }
        self.normalize();
    }

    /// 确保指定代码块组后面存在普通段落，并返回段落起始光标位置。
    ///
    /// 业务意图：
    /// - 代码块是块级编辑区域，用户点击代码块下方空白处时应能把光标移出代码块继续输入普通正文。
    /// - 如果代码块已经后接普通段落，直接定位到后续段落开头；如果代码块位于文档末尾或后面仍是代码块，
    ///   则插入一个空普通段落作为可编辑落点，避免后续输入继续落在代码块语义里。
    ///
    /// 边界条件：
    /// - `position` 使用线性 UTF-8 字节下标，方法内部会按当前文档纯文本长度夹紧。
    /// - 连续同语言代码行在 UI 中作为同一个视觉代码块组处理，因此插入位置必须落在整组代码块之后。
    /// - 返回值中的布尔值表示是否实际修改了文档，调用方据此决定是否压入撤销栈。
    pub(crate) fn ensure_paragraph_after_code_block_at_position(
        &mut self,
        position: usize,
    ) -> Option<(usize, bool)> {
        let text = self.plain_text();
        let position = clamp_note_rich_text_boundary(&text, position);
        let mut offset = 0;
        let mut block_index = 0;
        while block_index < self.blocks.len() {
            let block = &self.blocks[block_index];
            let block_len = block.plain_text().len();
            let block_start = offset;
            let block_end = block_start + block_len;
            let next_offset = block_end + usize::from(block_index + 1 < self.blocks.len());
            if position >= block_start
                && position <= block_end
                && block.kind == NoteRichTextBlockKind::CodeBlock
            {
                let language = block.code_language.clone();
                let mut group_end_index = block_index;
                let mut group_end_offset = block_end;
                while let Some(next) = self.blocks.get(group_end_index + 1) {
                    if next.kind != NoteRichTextBlockKind::CodeBlock
                        || next.code_language != language
                    {
                        break;
                    }
                    let next_start = group_end_offset + 1;
                    group_end_offset = next_start + next.plain_text().len();
                    group_end_index += 1;
                }
                let cursor_after_group = group_end_offset + 1;
                if let Some(next) = self.blocks.get(group_end_index + 1)
                    && next.kind != NoteRichTextBlockKind::CodeBlock
                {
                    return Some((cursor_after_group, false));
                }
                self.blocks.insert(
                    group_end_index + 1,
                    NoteRichTextBlock {
                        kind: NoteRichTextBlockKind::Paragraph,
                        code_language: None,
                        runs: Vec::new(),
                    },
                );
                self.normalize();
                return Some((cursor_after_group, true));
            }
            offset = next_offset;
            block_index += 1;
        }
        None
    }

    /// 返回光标所在或选区覆盖的块下标。
    fn block_indices_for_range(&self, range: Range<usize>) -> Vec<usize> {
        let text = self.plain_text();
        let range = clamp_note_rich_text_range(&text, range);
        let start = range.start;
        let end = range.end.max(start);
        let mut result = Vec::new();
        let mut offset = 0;
        for (index, block) in self.blocks.iter().enumerate() {
            let block_len = block.plain_text().len();
            let block_start = offset;
            let block_end = offset + block_len;
            let covered = if start == end {
                start >= block_start && start <= block_end
            } else {
                start <= block_end && end >= block_start
            };
            if covered {
                result.push(index);
            }
            offset = block_end + usize::from(index + 1 < self.blocks.len());
        }
        result
    }

    /// 返回指定位置前后可继承的样式。
    pub(crate) fn style_at_position(&self, position: usize) -> NoteRichTextStyle {
        let text = self.plain_text();
        let position = clamp_note_rich_text_boundary(&text, position);
        let mut previous_style: Option<NoteRichTextStyle> = None;
        for (range, unit) in self.units() {
            if let RichTextUnit::Text { style, .. } = unit {
                // 光标位于两个 run 的边界时，富文本编辑器应继承左侧刚输入文本的样式。
                // 这能保证“无选区打开加粗/改字号后连续输入”不会在第一个字符后丢失待输入样式；
                // 如果光标在文档开头，则退回到右侧首字符样式，符合打开已有文本时的工具栏状态。
                if position <= range.start {
                    return previous_style.unwrap_or(style);
                }
                if position < range.end {
                    return style;
                }
                previous_style = Some(style);
            }
        }
        previous_style.unwrap_or_default()
    }

    /// 返回指定位置所在块类型。
    pub(crate) fn block_kind_at_position(&self, position: usize) -> NoteRichTextBlockKind {
        self.block_meta_at_position(position).0
    }

    /// 返回指定位置所在块类型。
    fn kind_at_position(&self, position: usize) -> NoteRichTextBlockKind {
        self.block_meta_at_position(position).0
    }

    /// 返回指定位置所在块的类型和代码语言。
    fn block_meta_at_position(&self, position: usize) -> (NoteRichTextBlockKind, Option<String>) {
        let text = self.plain_text();
        let position = clamp_note_rich_text_boundary(&text, position);
        let mut offset = 0;
        for (index, block) in self.blocks.iter().enumerate() {
            let end = offset + block.plain_text().len();
            if position <= end {
                return (block.kind, block.code_language.clone());
            }
            offset = end + usize::from(index + 1 < self.blocks.len());
        }
        self.blocks
            .last()
            .map(|block| (block.kind, block.code_language.clone()))
            .unwrap_or((NoteRichTextBlockKind::Paragraph, None))
    }

    /// 将文档转换成带字节范围的线性单元。
    fn units(&self) -> Vec<(Range<usize>, RichTextUnit)> {
        let mut units = Vec::new();
        let mut offset = 0;
        for (block_index, block) in self.blocks.iter().enumerate() {
            for run in &block.runs {
                for character in run.text.chars() {
                    let len = character.len_utf8();
                    units.push((
                        offset..offset + len,
                        RichTextUnit::Text {
                            character,
                            style: run.style.clone(),
                            block_kind: block.kind,
                            code_language: block.code_language.clone(),
                        },
                    ));
                    offset += len;
                }
            }
            if let Some(next_block) = self.blocks.get(block_index + 1) {
                units.push((
                    offset..offset + 1,
                    RichTextUnit::Break {
                        next_kind: next_block.kind,
                        next_code_language: next_block.code_language.clone(),
                    },
                ));
                offset += 1;
            }
        }
        units
    }

    /// 从线性单元重建文档。
    fn from_units(
        first_kind: NoteRichTextBlockKind,
        first_code_language: Option<String>,
        units: Vec<RichTextUnit>,
    ) -> Self {
        let mut blocks = Vec::new();
        let mut current = NoteRichTextBlock {
            kind: first_kind,
            code_language: first_code_language,
            runs: Vec::new(),
        };
        for unit in units {
            match unit {
                RichTextUnit::Text {
                    character, style, ..
                } => {
                    if let Some(previous) = current.runs.last_mut()
                        && previous.style == style
                    {
                        previous.text.push(character);
                        continue;
                    }
                    current.runs.push(NoteRichTextRun {
                        text: character.to_string(),
                        style,
                    });
                }
                RichTextUnit::Break {
                    next_kind,
                    next_code_language,
                } => {
                    blocks.push(current);
                    current = NoteRichTextBlock {
                        kind: next_kind,
                        code_language: next_code_language,
                        runs: Vec::new(),
                    };
                }
            }
        }
        blocks.push(current);
        let mut document = Self {
            version: NOTE_RICH_TEXT_JSON_VERSION,
            blocks,
        };
        document.normalize();
        document
    }
}

/// 应用内富文本剪贴板元数据。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NoteRichTextClipboardPayload {
    /// 元数据版本。
    pub(crate) version: u32,
    /// 被复制的富文本片段。
    pub(crate) document: NoteRichTextDocument,
}

impl NoteRichTextClipboardPayload {
    /// 根据选区创建剪贴板元数据。
    pub(crate) fn new(document: NoteRichTextDocument) -> Self {
        Self {
            version: NOTE_RICH_TEXT_JSON_VERSION,
            document,
        }
    }

    /// 校验并取出富文本片段。
    pub(crate) fn into_document(self) -> Option<NoteRichTextDocument> {
        (self.version == NOTE_RICH_TEXT_JSON_VERSION).then_some(self.document)
    }
}

/// 根据笔记格式懒转换为富文本文档。
///
/// 业务意图：
/// - 旧纯文本和 Markdown 数据仍可打开；只有用户保存时才写入 `RichText` JSON，避免启动时迁移失败影响整库可用性。
pub(crate) fn rich_text_document_from_note(note: &Note) -> Result<NoteRichTextDocument, String> {
    match note.content_format {
        NoteContentFormat::RichText => NoteRichTextDocument::from_json(&note.content),
        NoteContentFormat::PlainText => Ok(NoteRichTextDocument::from_plain_text(&note.content)),
        NoteContentFormat::Markdown => Ok(NoteRichTextDocument::from_markdown(&note.content)),
    }
}

/// 返回笔记 Markdown 解析使用的扩展集合。
fn note_markdown_options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_GFM);
    options
}

/// 把 Markdown 基础语义转换为富文本文档。
fn note_rich_text_document_from_markdown(markdown: &str) -> NoteRichTextDocument {
    let markdown = markdown.strip_prefix('\u{feff}').unwrap_or(markdown);
    let mut blocks = Vec::new();
    let mut runs = Vec::new();
    let mut block_kind: Option<NoteRichTextBlockKind> = None;
    let mut code_language: Option<String> = None;
    let mut style = NoteRichTextStyle::default();
    let mut list_stack: Vec<NoteRichTextBlockKind> = Vec::new();

    for event in Parser::new_ext(markdown, note_markdown_options()) {
        match event {
            Event::Start(Tag::Paragraph) | Event::Start(Tag::Heading { .. }) => {
                begin_markdown_block(
                    &mut blocks,
                    &mut runs,
                    &mut block_kind,
                    &mut code_language,
                    NoteRichTextBlockKind::Paragraph,
                    None,
                );
            }
            Event::End(TagEnd::Paragraph) | Event::End(TagEnd::Heading(_)) => {
                finish_markdown_block(&mut blocks, &mut runs, &mut block_kind, &mut code_language);
            }
            Event::Start(Tag::List(start)) => {
                list_stack.push(if start.is_some() {
                    NoteRichTextBlockKind::OrderedListItem
                } else {
                    NoteRichTextBlockKind::UnorderedListItem
                });
            }
            Event::End(TagEnd::List(_)) => {
                list_stack.pop();
            }
            Event::Start(Tag::Item) => {
                let kind = list_stack
                    .last()
                    .copied()
                    .unwrap_or(NoteRichTextBlockKind::UnorderedListItem);
                begin_markdown_block(
                    &mut blocks,
                    &mut runs,
                    &mut block_kind,
                    &mut code_language,
                    kind,
                    None,
                );
            }
            Event::End(TagEnd::Item) => {
                finish_markdown_block(&mut blocks, &mut runs, &mut block_kind, &mut code_language);
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split_whitespace()
                        .next()
                        .filter(|value| !value.is_empty())
                        .map(ToString::to_string),
                    CodeBlockKind::Indented => None,
                };
                begin_markdown_block(
                    &mut blocks,
                    &mut runs,
                    &mut block_kind,
                    &mut code_language,
                    NoteRichTextBlockKind::CodeBlock,
                    language,
                );
            }
            Event::End(TagEnd::CodeBlock) => {
                finish_markdown_code_block(
                    &mut blocks,
                    &mut runs,
                    &mut block_kind,
                    &mut code_language,
                );
            }
            Event::Start(Tag::Strong) => style.bold = true,
            Event::End(TagEnd::Strong) => style.bold = false,
            Event::Start(Tag::Emphasis) => style.italic = true,
            Event::End(TagEnd::Emphasis) => style.italic = false,
            Event::Start(Tag::Strikethrough) => style.strikethrough = true,
            Event::End(TagEnd::Strikethrough) => style.strikethrough = false,
            Event::Text(text) | Event::Code(text) => {
                append_markdown_text(
                    &mut blocks,
                    &mut runs,
                    &mut block_kind,
                    &mut code_language,
                    &style,
                    &text,
                );
            }
            Event::SoftBreak | Event::HardBreak => {
                finish_markdown_block(&mut blocks, &mut runs, &mut block_kind, &mut code_language);
                begin_markdown_block(
                    &mut blocks,
                    &mut runs,
                    &mut block_kind,
                    &mut code_language,
                    NoteRichTextBlockKind::Paragraph,
                    None,
                );
            }
            Event::Rule => {
                begin_markdown_block(
                    &mut blocks,
                    &mut runs,
                    &mut block_kind,
                    &mut code_language,
                    NoteRichTextBlockKind::Paragraph,
                    None,
                );
                append_markdown_text(
                    &mut blocks,
                    &mut runs,
                    &mut block_kind,
                    &mut code_language,
                    &style,
                    "---",
                );
                finish_markdown_block(&mut blocks, &mut runs, &mut block_kind, &mut code_language);
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                append_markdown_text(
                    &mut blocks,
                    &mut runs,
                    &mut block_kind,
                    &mut code_language,
                    &style,
                    &html,
                );
            }
            _ => {}
        }
    }
    finish_markdown_block(&mut blocks, &mut runs, &mut block_kind, &mut code_language);

    if blocks.is_empty() {
        NoteRichTextDocument::empty()
    } else {
        let mut document = NoteRichTextDocument {
            version: NOTE_RICH_TEXT_JSON_VERSION,
            blocks,
        };
        document.normalize();
        document
    }
}

/// 开始一个 Markdown 导入块。
fn begin_markdown_block(
    blocks: &mut Vec<NoteRichTextBlock>,
    runs: &mut Vec<NoteRichTextRun>,
    block_kind: &mut Option<NoteRichTextBlockKind>,
    code_language: &mut Option<String>,
    next_kind: NoteRichTextBlockKind,
    next_language: Option<String>,
) {
    finish_markdown_block(blocks, runs, block_kind, code_language);
    *block_kind = Some(next_kind);
    *code_language = next_language;
}

/// 结束当前 Markdown 导入块。
fn finish_markdown_block(
    blocks: &mut Vec<NoteRichTextBlock>,
    runs: &mut Vec<NoteRichTextRun>,
    block_kind: &mut Option<NoteRichTextBlockKind>,
    code_language: &mut Option<String>,
) {
    let Some(kind) = block_kind.take() else {
        return;
    };
    blocks.push(NoteRichTextBlock {
        kind,
        code_language: (kind == NoteRichTextBlockKind::CodeBlock)
            .then(|| code_language.take())
            .flatten(),
        runs: std::mem::take(runs),
    });
}

/// 结束当前 Markdown 代码块，并丢弃 fenced code 末尾协议性换行产生的空块。
///
/// 业务意图：
/// - `pulldown-cmark` 会把围栏代码块闭合前的换行作为代码文本的一部分上报。
/// - 编辑器内部用“多个代码块行”表示多行代码，如果把最后这个协议性空行也保存下来，笔记每次保存再读取都会多出一行空白。
///
/// 边界条件：
/// - 用户真实输入的空白代码行必须保留；只有“当前块为空，并且前一个块已经是同语言代码块”时才丢弃末尾占位块。
/// - 损坏或未闭合的 Markdown 在文件末尾仍走普通 `finish_markdown_block`，避免丢失实际输入内容。
fn finish_markdown_code_block(
    blocks: &mut Vec<NoteRichTextBlock>,
    runs: &mut Vec<NoteRichTextRun>,
    block_kind: &mut Option<NoteRichTextBlockKind>,
    code_language: &mut Option<String>,
) {
    let language = code_language.clone();
    if block_kind
        .as_ref()
        .is_some_and(|kind| *kind == NoteRichTextBlockKind::CodeBlock)
        && runs.is_empty()
        && blocks.last().is_some_and(|block| {
            block.kind == NoteRichTextBlockKind::CodeBlock && block.code_language == language
        })
    {
        *block_kind = None;
        *code_language = None;
        return;
    }
    finish_markdown_block(blocks, runs, block_kind, code_language);
}

/// 把 Markdown 文本追加到当前块；文本内换行会拆成多个富文本块。
fn append_markdown_text(
    blocks: &mut Vec<NoteRichTextBlock>,
    runs: &mut Vec<NoteRichTextRun>,
    block_kind: &mut Option<NoteRichTextBlockKind>,
    code_language: &mut Option<String>,
    style: &NoteRichTextStyle,
    text: &str,
) {
    if block_kind.is_none() {
        *block_kind = Some(NoteRichTextBlockKind::Paragraph);
    }
    let mut parts = text.split('\n').peekable();
    while let Some(part) = parts.next() {
        if !part.is_empty() {
            let in_code_block = block_kind
                .as_ref()
                .is_some_and(|kind| *kind == NoteRichTextBlockKind::CodeBlock);
            let mut run_style = if in_code_block {
                NoteRichTextStyle::default()
            } else {
                style.clone()
            };
            if in_code_block {
                run_style.bold = false;
                run_style.italic = false;
                run_style.strikethrough = false;
            }
            runs.push(NoteRichTextRun {
                text: part.to_string(),
                style: run_style,
            });
        }
        if parts.peek().is_some() {
            let kind = block_kind.unwrap_or(NoteRichTextBlockKind::Paragraph);
            let language = code_language.clone();
            finish_markdown_block(blocks, runs, block_kind, code_language);
            *block_kind = Some(kind);
            *code_language = language;
        }
    }
}

/// 把富文本文档导出为 Markdown。
fn note_rich_text_document_to_markdown(document: &NoteRichTextDocument) -> String {
    let mut output = String::new();
    let mut index = 0;
    while index < document.blocks.len() {
        if document.blocks[index].kind == NoteRichTextBlockKind::CodeBlock {
            let language = document.blocks[index].code_language.clone();
            if !output.is_empty() {
                output.push_str("\n\n");
            }
            output.push_str("```");
            if let Some(language) = language.as_deref().filter(|value| !value.is_empty()) {
                output.push_str(language);
            }
            output.push('\n');
            while index < document.blocks.len()
                && document.blocks[index].kind == NoteRichTextBlockKind::CodeBlock
                && document.blocks[index].code_language == language
            {
                output.push_str(&document.blocks[index].plain_text());
                output.push('\n');
                index += 1;
            }
            output.push_str("```");
            continue;
        }

        if !output.is_empty() {
            output.push_str("\n\n");
        }
        match document.blocks[index].kind {
            NoteRichTextBlockKind::Paragraph => {
                output.push_str(&note_rich_text_runs_to_markdown(
                    &document.blocks[index].runs,
                ));
            }
            NoteRichTextBlockKind::UnorderedListItem => {
                output.push_str("- ");
                output.push_str(&note_rich_text_runs_to_markdown(
                    &document.blocks[index].runs,
                ));
            }
            NoteRichTextBlockKind::OrderedListItem => {
                output.push_str("1. ");
                output.push_str(&note_rich_text_runs_to_markdown(
                    &document.blocks[index].runs,
                ));
            }
            NoteRichTextBlockKind::CodeBlock => {}
        }
        index += 1;
    }
    output
}

/// 把一组富文本 run 导出为 Markdown 行内文本。
fn note_rich_text_runs_to_markdown(runs: &[NoteRichTextRun]) -> String {
    let mut output = String::new();
    for run in runs {
        output.push_str(&note_rich_text_run_to_markdown(run));
    }
    output
}

/// 把单个 run 导出为 Markdown 行内文本。
fn note_rich_text_run_to_markdown(run: &NoteRichTextRun) -> String {
    let mut text = escape_note_markdown_text(&run.text);
    if run.style.strikethrough && !text.is_empty() {
        text = format!("~~{text}~~");
    }
    if run.style.italic && !text.is_empty() {
        text = format!("*{text}*");
    }
    if run.style.bold && !text.is_empty() {
        text = format!("**{text}**");
    }
    text
}

/// 转义 Markdown 行内文本中的基础控制字符。
fn escape_note_markdown_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(
            character,
            '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '#'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// 限制富文本字节位置到 UTF-8 字符边界。
pub(crate) fn clamp_note_rich_text_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// 限制富文本范围到 UTF-8 字符边界。
pub(crate) fn clamp_note_rich_text_range(text: &str, range: Range<usize>) -> Range<usize> {
    let start = clamp_note_rich_text_boundary(text, range.start);
    let end = clamp_note_rich_text_boundary(text, range.end);
    start.min(end)..start.max(end)
}

fn append_units_in_range(
    units: &[(Range<usize>, RichTextUnit)],
    range: Range<usize>,
    output: &mut Vec<RichTextUnit>,
) {
    for (unit_range, unit) in units {
        if range.start <= unit_range.start && unit_range.end <= range.end {
            output.push(unit.clone());
        }
    }
}

fn first_meta_for_rebuild(
    blocks: &[NoteRichTextBlock],
    units: &[RichTextUnit],
) -> Option<(NoteRichTextBlockKind, Option<String>)> {
    if units.is_empty() {
        return blocks
            .first()
            .map(|block| (block.kind, block.code_language.clone()));
    }
    if let Some(RichTextUnit::Text {
        block_kind,
        code_language,
        ..
    }) = units.first()
    {
        return Some((*block_kind, code_language.clone()));
    }
    blocks
        .first()
        .map(|block| (block.kind, block.code_language.clone()))
}

fn ranges_intersect(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证富文本 JSON 能往返，并保留局部样式。
    #[test]
    fn 富文本_json_序列化往返保留样式() {
        let mut document = NoteRichTextDocument::from_plain_text("你好");
        document.apply_style_patch(
            0..3,
            &NoteRichTextStylePatch {
                bold: Some(true),
                font_size_px: Some(20),
                background_color: Some(Some(NoteRichTextColor::rgb(254, 240, 138))),
                ..Default::default()
            },
        );

        let json = document.to_json().unwrap();
        let restored = NoteRichTextDocument::from_json(&json).unwrap();
        assert_eq!(restored, document);
    }

    /// 验证新增背景色字段兼容旧富文本 JSON。
    #[test]
    fn 富文本旧_json_缺少背景色会按透明处理() {
        let json = r#"{"version":1,"blocks":[{"kind":"paragraph","runs":[{"text":"旧笔记","style":{"font_size_px":14,"bold":false,"italic":false,"underline":false,"strikethrough":false,"color":null}}]}]}"#;
        let restored = NoteRichTextDocument::from_json(json).unwrap();
        assert_eq!(restored.blocks[0].runs[0].style.background_color, None);
    }

    /// 验证中文和 emoji 插入删除都按 UTF-8 字符边界处理。
    #[test]
    fn 富文本编辑不会截断中文和_emoji() {
        let mut document = NoteRichTextDocument::from_plain_text("A中文😀B");
        let start = "A".len();
        let end = "A中文😀".len();
        document.delete_range(start..end);
        assert_eq!(document.plain_text(), "AB");

        document.replace_range_with_text(1..1, "再😀", NoteRichTextStyle::default());
        assert_eq!(document.plain_text(), "A再😀B");
    }

    /// 验证选区样式会拆分 run，且相邻同样式 run 会重新合并。
    #[test]
    fn 富文本选区样式会拆分并合并_run() {
        let mut document = NoteRichTextDocument::from_plain_text("abcd");
        document.apply_style_patch(
            1..3,
            &NoteRichTextStylePatch {
                italic: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(document.blocks[0].runs.len(), 3);
        document.apply_style_patch(
            0..4,
            &NoteRichTextStylePatch {
                italic: Some(false),
                ..Default::default()
            },
        );
        assert_eq!(document.blocks[0].runs.len(), 1);
    }

    /// 验证光标位于 run 边界时继承左侧样式，保证连续输入不会丢失工具栏待输入样式。
    #[test]
    fn 富文本边界样式优先继承左侧_run() {
        let mut document = NoteRichTextDocument::from_plain_text("ab");
        document.apply_style_patch(
            0..1,
            &NoteRichTextStylePatch {
                bold: Some(true),
                font_size_px: Some(20),
                ..Default::default()
            },
        );

        let boundary_style = document.style_at_position(1);
        assert!(boundary_style.bold);
        assert_eq!(boundary_style.font_size_px, 20);
        assert!(!document.style_at_position(2).bold);
    }

    /// 验证列表切换只修改覆盖到的块类型。
    #[test]
    fn 富文本列表块切换可恢复为段落() {
        let mut document = NoteRichTextDocument::from_plain_text("一\n二");
        document.toggle_blocks_kind(0..document.len(), NoteRichTextBlockKind::UnorderedListItem);
        assert!(
            document
                .blocks
                .iter()
                .all(|block| block.kind == NoteRichTextBlockKind::UnorderedListItem)
        );
        document.toggle_blocks_kind(0..document.len(), NoteRichTextBlockKind::UnorderedListItem);
        assert!(
            document
                .blocks
                .iter()
                .all(|block| block.kind == NoteRichTextBlockKind::Paragraph)
        );
    }

    /// 验证代码块 JSON 能往返，并保留语言字段。
    ///
    /// 业务意图：
    /// - 笔记代码块只扩展富文本 JSON，不改变 Markdown 文件身份；该测试锁定旧字段兼容和新语言字段的保存语义。
    #[test]
    fn 富文本代码块_json_保留语言字段() {
        let mut document = NoteRichTextDocument::from_plain_text("fn main() {}\nprintln!();");
        document.toggle_blocks_kind(0..document.len(), NoteRichTextBlockKind::CodeBlock);
        document.set_code_language_for_range(0..document.len(), "rust");

        let restored = NoteRichTextDocument::from_json(&document.to_json().unwrap()).unwrap();
        assert_eq!(restored.blocks[0].kind, NoteRichTextBlockKind::CodeBlock);
        assert_eq!(restored.blocks[0].code_language.as_deref(), Some("rust"));
        assert_eq!(restored.blocks[1].code_language.as_deref(), Some("rust"));
    }

    /// 验证 Markdown 代码块保存往返不会重复追加空白行。
    ///
    /// 业务意图：
    /// - 用户保存笔记时 Markdown 会先解析成富文本再序列化；围栏代码块末尾的换行是 Markdown 语法边界，不应变成用户内容。
    /// - 如果这里回归，代码块每保存一次都会多出一个空行，属于可见数据损坏。
    #[test]
    fn 富文本代码块_markdown_往返不追加空行() {
        let markdown = "```rust\nlet a = 1;\n```";
        let once = NoteRichTextDocument::from_markdown(markdown).to_markdown();
        let twice = NoteRichTextDocument::from_markdown(&once).to_markdown();

        assert_eq!(once, markdown);
        assert_eq!(twice, markdown);
    }

    /// 验证旧富文本 JSON 缺少代码语言字段时仍可读取。
    #[test]
    fn 富文本旧_json_缺少代码语言字段可读取() {
        let json = r#"{"version":1,"blocks":[{"kind":"paragraph","runs":[{"text":"旧笔记","style":{"font_size_px":14,"bold":false,"italic":false,"underline":false,"strikethrough":false,"color":null}}]}]}"#;
        let restored = NoteRichTextDocument::from_json(json).unwrap();
        assert_eq!(restored.blocks[0].kind, NoteRichTextBlockKind::Paragraph);
        assert_eq!(restored.blocks[0].code_language, None);
    }

    /// 验证代码块中插入换行会继承代码块类型和语言。
    ///
    /// 业务意图：
    /// - 编辑器按 Enter 会调用普通文本替换路径；如果这里不继承块语义，代码块会被拆成普通段落。
    #[test]
    fn 富文本代码块插入换行会继承语言() {
        let mut document = NoteRichTextDocument::from_plain_text("let a = 1;");
        document.toggle_blocks_kind(0..document.len(), NoteRichTextBlockKind::CodeBlock);
        document.set_code_language_for_range(0..document.len(), "rust");

        document.replace_range_with_text(4..4, "\n", NoteRichTextStyle::default());

        assert_eq!(document.blocks.len(), 2);
        assert!(
            document
                .blocks
                .iter()
                .all(|block| block.kind == NoteRichTextBlockKind::CodeBlock)
        );
        assert!(
            document
                .blocks
                .iter()
                .all(|block| block.code_language.as_deref() == Some("rust"))
        );
    }

    /// 验证代码块不写入普通富文本样式。
    ///
    /// 业务意图：
    /// - 代码块渲染固定使用等宽字体和语法高亮，不读取 run 上的字号、颜色、粗斜体等富文本样式。
    /// - 如果模型层仍保存这些隐藏样式，代码块切回普通段落或复制到正文后会出现用户不可预期的样式。
    #[test]
    fn 富文本代码块忽略内联样式补丁() {
        let mut document = NoteRichTextDocument::from_plain_text("let a = 1;");
        document.toggle_blocks_kind(0..document.len(), NoteRichTextBlockKind::CodeBlock);
        document.apply_style_patch(
            0..document.len(),
            &NoteRichTextStylePatch {
                bold: Some(true),
                color: Some(Some(NoteRichTextColor::rgb(220, 38, 38))),
                font_size_px: Some(24),
                ..Default::default()
            },
        );

        assert_eq!(
            document.blocks[0].runs[0].style,
            NoteRichTextStyle::default()
        );
    }

    /// 验证点击代码块下方时可懒创建普通段落作为光标落点。
    ///
    /// 业务意图：
    /// - 用户需要在代码块后继续输入正文；如果最后一个块仍是代码块，必须能创建一个普通段落脱离代码编辑语义。
    #[test]
    fn 富文本代码块后可创建普通段落落点() {
        let mut document = NoteRichTextDocument::from_plain_text("let a = 1;");
        document.toggle_blocks_kind(0..document.len(), NoteRichTextBlockKind::CodeBlock);
        document.set_code_language_for_range(0..document.len(), "rust");

        let (cursor, changed) = document
            .ensure_paragraph_after_code_block_at_position(document.len())
            .unwrap();

        assert!(changed);
        assert_eq!(cursor, "let a = 1;\n".len());
        assert_eq!(
            document.blocks.last().map(|block| block.kind),
            Some(NoteRichTextBlockKind::Paragraph)
        );
        assert_eq!(
            document.block_kind_at_position(cursor),
            NoteRichTextBlockKind::Paragraph
        );
    }

    /// 验证代码块后已有普通正文时仅移动光标，不产生内容修改。
    #[test]
    fn 富文本代码块后已有段落时不重复创建段落() {
        let mut document = NoteRichTextDocument::from_plain_text("code\n正文");
        document.toggle_blocks_kind(0..4, NoteRichTextBlockKind::CodeBlock);
        document.set_code_language_for_range(0..4, "rust");

        let (cursor, changed) = document
            .ensure_paragraph_after_code_block_at_position(2)
            .unwrap();

        assert!(!changed);
        assert_eq!(cursor, "code\n".len());
        assert_eq!(document.blocks.len(), 2);
        assert_eq!(
            document.block_kind_at_position(cursor),
            NoteRichTextBlockKind::Paragraph
        );
    }

    /// 验证复制粘贴代码块片段时保留首行代码块类型。
    #[test]
    fn 富文本代码块片段保留首行类型() {
        let mut document = NoteRichTextDocument::from_plain_text("a\nb");
        document.toggle_blocks_kind(0..document.len(), NoteRichTextBlockKind::CodeBlock);
        document.set_code_language_for_range(0..document.len(), "rust");
        let fragment = document.fragment_for_range(0..document.len());

        let mut target = NoteRichTextDocument::empty();
        target.replace_range_with_document(0..0, &fragment);

        assert_eq!(target.blocks[0].kind, NoteRichTextBlockKind::CodeBlock);
        assert_eq!(target.blocks[0].code_language.as_deref(), Some("rust"));
        assert_eq!(target.plain_text(), "a\nb");
    }
}
