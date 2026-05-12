// 笔记富文本文档模型。
//
// 业务意图：
// - 笔记正文升级为自研富文本后，SQLite 仍只保存一个 `content` 字段，因此这里定义稳定 JSON v1 文档结构。
// - 本模块不依赖 GPUI，负责文本、块、样式、选区变换和序列化；UI 只消费这些纯状态，避免渲染代码直接改 JSON。
//
// 边界条件：
// - 所有选区位置都使用线性 UTF-8 字节下标，段落之间用一个 `\n` 映射，确保中文和 emoji 不会被截断。
// - 旧纯文本和旧 Markdown 不解析语法，只按原始文本拆成富文本段落，保存后才写回富文本 JSON。

use std::ops::Range;

use serde::{Deserialize, Serialize};

use super::*;

/// 富文本 JSON 版本号。
///
/// 业务意图：
/// - 当前不提升 SQLite schema，只在 JSON 内部保存版本；未来增加表格、对齐、链接等能力时可按文档版本迁移。
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
    },
    Break {
        next_kind: NoteRichTextBlockKind,
    },
}

impl NoteRichTextDocument {
    /// 创建空富文本文档。
    pub(crate) fn empty() -> Self {
        Self {
            version: NOTE_RICH_TEXT_JSON_VERSION,
            blocks: vec![NoteRichTextBlock {
                kind: NoteRichTextBlockKind::Paragraph,
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
        let mut blocks = text
            .split('\n')
            .map(|line| NoteRichTextBlock {
                kind: NoteRichTextBlockKind::Paragraph,
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
                kind: NoteRichTextBlockKind::Paragraph,
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

    /// 从 SQLite 保存的 JSON 解析富文本文档。
    pub(crate) fn from_json(raw: &str) -> Result<Self, String> {
        let mut document: Self =
            serde_json::from_str(raw).map_err(|error| format!("解析富文本笔记失败：{error}"))?;
        if document.version != NOTE_RICH_TEXT_JSON_VERSION {
            return Err(format!("富文本笔记版本 {} 暂不支持", document.version));
        }
        document.normalize();
        Ok(document)
    }

    /// 序列化为 SQLite `notes.content` 字段。
    pub(crate) fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| format!("序列化富文本笔记失败：{error}"))
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
        }
        if self.blocks.is_empty() {
            self.blocks.push(NoteRichTextBlock {
                kind: NoteRichTextBlockKind::Paragraph,
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
        let units = self.units();
        let fragment_units = fragment.units();
        let mut next_units = Vec::new();
        append_units_in_range(&units, 0..range.start, &mut next_units);
        next_units.extend(fragment_units.into_iter().map(|(_, unit)| unit));
        append_units_in_range(&units, range.end..text.len(), &mut next_units);
        let first_kind = first_kind_for_rebuild(&self.blocks, &next_units);
        *self = Self::from_units(first_kind, next_units);
    }

    /// 使用普通文本替换指定范围，插入文本继承调用方传入的样式。
    pub(crate) fn replace_range_with_text(
        &mut self,
        range: Range<usize>,
        text: &str,
        style: NoteRichTextStyle,
    ) {
        let fragment = Self::from_plain_text_with_style(text, style);
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
        Self::from_units(first_kind, selected_units)
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
                } if ranges_intersect(&unit_range, &range) => {
                    style.apply_patch(patch);
                    next_units.push(RichTextUnit::Text { character, style });
                }
                other => next_units.push(other),
            }
        }
        let first_kind = self
            .blocks
            .first()
            .map(|block| block.kind)
            .unwrap_or(NoteRichTextBlockKind::Paragraph);
        *self = Self::from_units(first_kind, next_units);
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
        }
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
    fn kind_at_position(&self, position: usize) -> NoteRichTextBlockKind {
        let text = self.plain_text();
        let position = clamp_note_rich_text_boundary(&text, position);
        let mut offset = 0;
        for (index, block) in self.blocks.iter().enumerate() {
            let end = offset + block.plain_text().len();
            if position <= end {
                return block.kind;
            }
            offset = end + usize::from(index + 1 < self.blocks.len());
        }
        self.blocks
            .last()
            .map(|block| block.kind)
            .unwrap_or(NoteRichTextBlockKind::Paragraph)
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
                    },
                ));
                offset += 1;
            }
        }
        units
    }

    /// 从线性单元重建文档。
    fn from_units(first_kind: NoteRichTextBlockKind, units: Vec<RichTextUnit>) -> Self {
        let mut blocks = Vec::new();
        let mut current = NoteRichTextBlock {
            kind: first_kind,
            runs: Vec::new(),
        };
        for unit in units {
            match unit {
                RichTextUnit::Text { character, style } => {
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
                RichTextUnit::Break { next_kind } => {
                    blocks.push(current);
                    current = NoteRichTextBlock {
                        kind: next_kind,
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
        NoteContentFormat::PlainText | NoteContentFormat::Markdown => {
            Ok(NoteRichTextDocument::from_plain_text(&note.content))
        }
    }
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

fn first_kind_for_rebuild(
    blocks: &[NoteRichTextBlock],
    _units: &[RichTextUnit],
) -> NoteRichTextBlockKind {
    blocks
        .first()
        .map(|block| block.kind)
        .unwrap_or(NoteRichTextBlockKind::Paragraph)
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
}
