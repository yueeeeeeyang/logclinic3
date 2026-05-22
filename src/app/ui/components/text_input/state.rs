// 通用单行输入框状态类型。
//
// 业务意图：
// - 输入框组件需要在绘制阶段读取不可变快照，同时在业务状态中保存真实文本、选区和 IME 组合范围。
// - 真实文本与展示文本分离，后续密码、API Key 等字段可以使用掩码模式而不泄露真实内容。

use super::*;

/// 输入框在主窗口状态中的绑定位置。
///
/// 业务意图：
/// - 第一阶段只接入搜索关键字，后续迁移其它输入框时在这里追加绑定即可复用同一个元素实现。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum TextInputBinding {
    /// 搜索弹窗的关键字输入框。
    SearchQuery,
    /// 连接页左侧连接名称过滤输入框。
    ConnectionTreeSearch,
    /// SSH 连接表单中的单行输入框。
    ConnectionForm(ConnectionFormField),
    /// SMB 连接表单中的单行输入框。
    SmbConnectionForm(SmbConnectionFormField),
    /// 连接分类弹窗中的名称输入框。
    ConnectionCategoryName,
    /// 连接文件管理窗口的路径地址栏。
    ///
    /// 业务意图：
    /// - 文件管理窗口不是 `MainView` 的子状态，但地址栏仍需要复用通用输入框的裁剪、IME、选区和水平滚动行为。
    /// - 该绑定由 `ConnectionFileManagerWindowView` 实现，`MainView` 遇到时会安全返回空结果。
    FileManagerPath,
}

/// 输入框展示模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum TextInputDisplayMode {
    /// 明文展示，展示下标与真实文本 UTF-8 下标一致。
    Plain,
    /// 掩码展示，真实文本只用于保存、IME 和剪贴板，绘制时按字符数量显示掩码字符。
    ///
    /// 业务意图：
    /// - 第一阶段只迁移搜索关键字输入框，因此运行路径暂时只使用明文模式。
    /// - 保留掩码模式是为了后续密码、API Key 和 SSH 连接密码字段迁移到同一组件时无需再改动核心结构。
    #[allow(dead_code)]
    Masked { mask_char: char },
}

impl TextInputDisplayMode {
    /// 根据真实文本生成展示文本。
    pub(in crate::app) fn display_text(&self, text: &str) -> String {
        match self {
            Self::Plain => text.to_string(),
            Self::Masked { mask_char } => std::iter::repeat(*mask_char)
                .take(text.chars().count())
                .collect::<String>(),
        }
    }

    /// 将真实文本 UTF-8 字节下标映射到展示文本 UTF-8 字节下标。
    ///
    /// 边界条件：
    /// - 掩码模式下真实字符和掩码字符的 UTF-8 宽度可能不同，必须按字符数量转换，不能直接复用真实字节下标。
    pub(in crate::app) fn display_index_for_text_index(&self, text: &str, index: usize) -> usize {
        match self {
            Self::Plain => text_input_clamp_byte_index(text, index),
            Self::Masked { mask_char } => {
                let clamped = text_input_clamp_byte_index(text, index);
                let character_count = text[..clamped].chars().count();
                character_count * mask_char.len_utf8()
            }
        }
    }

    /// 将真实文本范围映射为展示文本范围。
    pub(in crate::app) fn display_range_for_text_range(
        &self,
        text: &str,
        range: Range<usize>,
    ) -> Range<usize> {
        let range = text_input_clamp_range(text, range);
        let start = self.display_index_for_text_index(text, range.start);
        let end = self.display_index_for_text_index(text, range.end);
        start.min(end)..start.max(end)
    }

    /// 将展示文本 UTF-8 字节下标映射回真实文本 UTF-8 字节下标。
    ///
    /// 业务意图：
    /// - 鼠标命中基于展示文本布局返回下标；掩码模式需要再换算到真实文本字符边界。
    ///
    /// 边界条件：
    /// - 搜索关键字当前使用明文模式，掩码命中会在后续密码类输入框迁移时接入；先保留该方法可以让映射规则被单元测试锁定。
    #[allow(dead_code)]
    pub(in crate::app) fn text_index_for_display_index(
        &self,
        text: &str,
        display_index: usize,
    ) -> usize {
        match self {
            Self::Plain => text_input_clamp_byte_index(text, display_index),
            Self::Masked { mask_char } => {
                let character_index = display_index / mask_char.len_utf8();
                text.char_indices()
                    .nth(character_index)
                    .map(|(index, _)| index)
                    .unwrap_or(text.len())
            }
        }
    }
}

/// 输入框绘制阶段读取的快照。
#[derive(Clone)]
pub(in crate::app) struct TextInputSnapshot {
    /// 真实文本，供光标、选区、IME 和剪贴板使用。
    pub(in crate::app) text: String,
    /// 当前选择范围，按真实文本 UTF-8 字节下标保存。
    pub(in crate::app) selection_range: Range<usize>,
    /// 输入法组合文本范围，按真实文本 UTF-8 字节下标保存。
    pub(in crate::app) marked_range: Option<Range<usize>>,
    /// 当前水平滚动偏移。
    pub(in crate::app) horizontal_scroll_px: f32,
    /// 展示模式。
    pub(in crate::app) display_mode: TextInputDisplayMode,
}

impl TextInputSnapshot {
    /// 从现有单行输入快照构造通用组件快照。
    pub(in crate::app) fn from_single_line(
        snapshot: SingleLineTextInputSnapshot,
        display_mode: TextInputDisplayMode,
    ) -> Self {
        Self {
            text: snapshot.text,
            selection_range: snapshot.selection_range,
            marked_range: snapshot.marked_range,
            horizontal_scroll_px: snapshot.horizontal_scroll_px,
            display_mode,
        }
    }
}

impl MainView {
    /// 读取通用输入框绑定的当前快照。
    pub(in crate::app) fn text_input_snapshot(
        &self,
        binding: TextInputBinding,
    ) -> Option<TextInputSnapshot> {
        match binding {
            TextInputBinding::SearchQuery => self
                .search_text_snapshot(SearchTextInputKind::Query)
                .map(|snapshot| {
                    TextInputSnapshot::from_single_line(snapshot, TextInputDisplayMode::Plain)
                }),
            TextInputBinding::ConnectionTreeSearch => {
                self.connection_tree_search_text_snapshot().map(|snapshot| {
                    TextInputSnapshot::from_single_line(snapshot, TextInputDisplayMode::Plain)
                })
            }
            TextInputBinding::ConnectionForm(field) => {
                let display_mode = if field == ConnectionFormField::Password {
                    TextInputDisplayMode::Masked { mask_char: '*' }
                } else {
                    TextInputDisplayMode::Plain
                };
                self.connection_form_text_snapshot(field)
                    .map(|snapshot| TextInputSnapshot::from_single_line(snapshot, display_mode))
            }
            TextInputBinding::SmbConnectionForm(field) => {
                let display_mode = if field == SmbConnectionFormField::Password {
                    TextInputDisplayMode::Masked { mask_char: '*' }
                } else {
                    TextInputDisplayMode::Plain
                };
                self.smb_connection_form_text_snapshot(field)
                    .map(|snapshot| TextInputSnapshot::from_single_line(snapshot, display_mode))
            }
            TextInputBinding::ConnectionCategoryName => {
                self.connection_category_text_snapshot().map(|snapshot| {
                    TextInputSnapshot::from_single_line(snapshot, TextInputDisplayMode::Plain)
                })
            }
            TextInputBinding::FileManagerPath => None,
        }
    }

    /// 保存通用输入框最近一次绘制布局。
    pub(in crate::app) fn store_text_input_layout(
        &mut self,
        binding: TextInputBinding,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
        horizontal_scroll_px: f32,
    ) {
        match binding {
            TextInputBinding::SearchQuery => self.store_search_text_layout(
                SearchTextInputKind::Query,
                line,
                bounds,
                horizontal_scroll_px,
            ),
            TextInputBinding::ConnectionTreeSearch => {
                self.store_connection_tree_search_text_layout(line, bounds, horizontal_scroll_px)
            }
            TextInputBinding::ConnectionForm(field) => {
                self.store_connection_form_text_layout(field, line, bounds, horizontal_scroll_px)
            }
            TextInputBinding::SmbConnectionForm(field) => self
                .store_smb_connection_form_text_layout(field, line, bounds, horizontal_scroll_px),
            TextInputBinding::ConnectionCategoryName => {
                self.store_connection_category_text_layout(line, bounds, horizontal_scroll_px)
            }
            TextInputBinding::FileManagerPath => {}
        }
    }

    /// 返回通用输入框绑定的可变文本状态。
    ///
    /// 业务意图：
    /// - 鼠标选区、键盘编辑和平台输入法都应修改同一份 `SingleLineTextInputState`，这样拖拽锚点、选区和组合文本不会分散在具体功能页。
    /// - 调用方仍保留焦点句柄和布局缓存，因为这些数据依赖具体窗口位置和页面生命周期。
    ///
    /// 边界条件：
    /// - 搜索窗口或连接弹窗已经关闭时返回 `None`，低层事件监听在下一帧失效前可能仍收到一次释放事件，必须安全忽略。
    fn text_input_state_mut(
        &mut self,
        binding: TextInputBinding,
    ) -> Option<&mut SingleLineTextInputState> {
        match binding {
            TextInputBinding::SearchQuery => self
                .search
                .search_dialog
                .as_mut()
                .map(|dialog| &mut dialog.query_input),
            TextInputBinding::ConnectionTreeSearch => Some(&mut self.connections.tree_search.input),
            TextInputBinding::ConnectionForm(field) => self
                .connections
                .dialog
                .as_mut()
                .map(|dialog| &mut dialog.field_mut(field).input),
            TextInputBinding::SmbConnectionForm(field) => self
                .connections
                .smb_dialog
                .as_mut()
                .map(|dialog| &mut dialog.field_mut(field).input),
            TextInputBinding::ConnectionCategoryName => self
                .connections
                .category_dialog
                .as_mut()
                .map(|dialog| &mut dialog.name.input),
            TextInputBinding::FileManagerPath => None,
        }
    }

    /// 返回通用输入框绑定的只读文本状态。
    ///
    /// 业务意图：
    /// - 鼠标释放阶段只需要判断是否存在拖拽锚点，使用只读入口避免为了查询状态而提前获取可变借用。
    fn text_input_state(&self, binding: TextInputBinding) -> Option<&SingleLineTextInputState> {
        match binding {
            TextInputBinding::SearchQuery => self
                .search
                .search_dialog
                .as_ref()
                .map(|dialog| &dialog.query_input),
            TextInputBinding::ConnectionTreeSearch => Some(&self.connections.tree_search.input),
            TextInputBinding::ConnectionForm(field) => self
                .connections
                .dialog
                .as_ref()
                .map(|dialog| &dialog.field(field).input),
            TextInputBinding::SmbConnectionForm(field) => self
                .connections
                .smb_dialog
                .as_ref()
                .map(|dialog| &dialog.field(field).input),
            TextInputBinding::ConnectionCategoryName => self
                .connections
                .category_dialog
                .as_ref()
                .map(|dialog| &dialog.name.input),
            TextInputBinding::FileManagerPath => None,
        }
    }

    /// 根据窗口坐标换算通用输入框中的 UTF-8 字节下标。
    ///
    /// 边界条件：
    /// - 组件刚挂载但尚未回写布局时，各功能现有命中函数会回退到文本末尾。
    /// - 密码掩码字段通过连接表单命中函数把展示下标换回真实文本下标，避免掩码字符宽度和真实字符宽度不一致时切坏中文。
    fn text_input_index_for_point(
        &self,
        binding: TextInputBinding,
        position: Point<Pixels>,
    ) -> usize {
        match binding {
            TextInputBinding::SearchQuery => {
                self.search_text_index_for_point(SearchTextInputKind::Query, position)
            }
            TextInputBinding::ConnectionTreeSearch => {
                self.connection_tree_search_text_index_for_point(position)
            }
            TextInputBinding::ConnectionForm(field) => {
                self.connection_form_text_index_for_point(field, position)
            }
            TextInputBinding::SmbConnectionForm(field) => {
                self.smb_connection_form_text_index_for_point(field, position)
            }
            TextInputBinding::ConnectionCategoryName => {
                self.connection_category_text_index_for_point(position)
            }
            TextInputBinding::FileManagerPath => 0,
        }
    }

    /// 处理通用输入框鼠标按下，完成定位、选词、全选或开始拖拽选区。
    ///
    /// 业务意图：
    /// - 搜索关键字和连接表单都由 `TextInputElement` 自己处理鼠标编辑行为，具体页面不再保存拖拽锚点。
    /// - 搜索关键字保持旧交互：第一次获得焦点时全选，方便用户直接替换关键字；已经聚焦后才按普通输入框规则定位或拖拽。
    ///
    /// 边界条件：
    /// - 鼠标按下时会清除 IME 组合范围，避免组合中的临时文本继续提交到新的光标位置。
    /// - 双击和三击只改变选区，不进入拖拽生命周期，释放事件不会留下悬挂状态。
    pub(in crate::app) fn begin_text_input_mouse_interaction(
        &mut self,
        binding: TextInputBinding,
        event: &MouseDownEvent,
        was_focused: bool,
        context: &mut Context<Self>,
    ) -> bool {
        if matches!(
            binding,
            TextInputBinding::ConnectionForm(_) | TextInputBinding::SmbConnectionForm(_)
        ) {
            // 连接表单中的分类 Select 与文本输入框处于同一个弹窗层级；点击任意文本字段时应先收起
            // Select，避免浮层继续覆盖后续输入区域或底部按钮。
            self.close_connection_dialog_category_select();
            self.close_smb_connection_dialog_category_select();
        }
        if !was_focused {
            match binding {
                TextInputBinding::SearchQuery => {
                    if let Some(dialog) = self.search.search_dialog.as_mut() {
                        Self::select_all_search_query(dialog);
                        self.search.search_text_selection_drag = None;
                        self.touch_search_text_cursor_activity();
                        context.notify();
                        return true;
                    }
                    return false;
                }
                TextInputBinding::ConnectionTreeSearch => {
                    select_all_text_input(&mut self.connections.tree_search.input);
                    self.touch_search_text_cursor_activity();
                    context.notify();
                    return true;
                }
                _ => {}
            }
        }

        let index = self.text_input_index_for_point(binding, event.position);
        let handled = if let Some(input) = self.text_input_state_mut(binding) {
            begin_text_input_mouse_selection(
                input,
                index,
                event.click_count,
                event.modifiers.shift,
            );
            true
        } else {
            false
        };
        if handled {
            if binding == TextInputBinding::SearchQuery {
                self.search.search_text_selection_drag = None;
            }
            self.touch_search_text_cursor_activity();
            context.notify();
        }
        handled
    }

    /// 拖动鼠标时扩展通用输入框选区。
    ///
    /// 业务意图：
    /// - 释放点可能在输入框或弹窗外，因此移动监听不能依赖具体功能页容器命中；只要输入状态中存在拖拽锚点就继续更新。
    /// - 命中下标仍通过各输入框最后一次布局换算，保证长文本水平滚动和掩码展示都按同一规则工作。
    pub(in crate::app) fn update_text_input_mouse_drag(
        &mut self,
        binding: TextInputBinding,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) -> bool {
        let Some(anchor) = self
            .text_input_state(binding)
            .and_then(|input| input.selection_drag)
        else {
            return false;
        };
        let index = self.text_input_index_for_point(binding, position);
        let handled = if let Some(input) = self.text_input_state_mut(binding) {
            update_text_input_mouse_selection(input, anchor, index);
            true
        } else {
            false
        };
        if handled {
            self.touch_search_text_cursor_activity();
            context.notify();
        }
        handled
    }

    /// 结束通用输入框鼠标拖拽选区。
    ///
    /// 业务意图：
    /// - 鼠标释放由组件级监听统一清理，即使释放发生在弹窗空白、遮罩或其它控件上，也不会让输入框一直处于“按下”状态。
    pub(in crate::app) fn finish_text_input_mouse_drag(
        &mut self,
        binding: TextInputBinding,
        context: &mut Context<Self>,
    ) -> bool {
        let handled = self
            .text_input_state_mut(binding)
            .and_then(|input| input.selection_drag.take())
            .is_some();
        if handled {
            context.notify();
        }
        handled
    }
}

impl TextInputElementHost for MainView {
    /// 将主窗口已有的通用输入框状态暴露给 `TextInputElement`。
    fn text_input_snapshot(&self, binding: TextInputBinding) -> Option<TextInputSnapshot> {
        MainView::text_input_snapshot(self, binding)
    }

    /// 保存主窗口输入框最近一次布局。
    fn store_text_input_layout(
        &mut self,
        binding: TextInputBinding,
        line: ShapedLine,
        bounds: Bounds<Pixels>,
        horizontal_scroll_px: f32,
    ) {
        MainView::store_text_input_layout(self, binding, line, bounds, horizontal_scroll_px);
    }

    /// 委托主窗口处理输入框鼠标按下行为。
    fn begin_text_input_mouse_interaction(
        &mut self,
        binding: TextInputBinding,
        event: &MouseDownEvent,
        was_focused: bool,
        context: &mut Context<Self>,
    ) -> bool {
        MainView::begin_text_input_mouse_interaction(self, binding, event, was_focused, context)
    }

    /// 委托主窗口处理输入框鼠标拖拽行为。
    fn update_text_input_mouse_drag(
        &mut self,
        binding: TextInputBinding,
        position: Point<Pixels>,
        context: &mut Context<Self>,
    ) -> bool {
        MainView::update_text_input_mouse_drag(self, binding, position, context)
    }

    /// 委托主窗口清理输入框拖拽状态。
    fn finish_text_input_mouse_drag(
        &mut self,
        binding: TextInputBinding,
        context: &mut Context<Self>,
    ) -> bool {
        MainView::finish_text_input_mouse_drag(self, binding, context)
    }

    /// 主窗口输入框沿用现有光标闪烁节奏。
    fn text_input_cursor_visible(&self) -> bool {
        self.search_text_cursor_visible()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证掩码展示不会泄露真实文本。
    #[test]
    fn 掩码模式只按字符数展示掩码() {
        let mode = TextInputDisplayMode::Masked { mask_char: '*' };
        assert_eq!(mode.display_text("a中🙂"), "***");
    }

    /// 验证掩码模式下真实下标和展示下标按字符数量映射。
    #[test]
    fn 掩码模式下标按字符数量映射() {
        let mode = TextInputDisplayMode::Masked { mask_char: '*' };
        let text = "a中🙂";

        assert_eq!(mode.display_index_for_text_index(text, 0), 0);
        assert_eq!(mode.display_index_for_text_index(text, 1), 1);
        assert_eq!(mode.display_index_for_text_index(text, 4), 2);
        assert_eq!(mode.display_index_for_text_index(text, text.len()), 3);
        assert_eq!(mode.text_index_for_display_index(text, 2), 4);
    }
}
