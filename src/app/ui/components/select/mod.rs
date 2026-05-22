// 通用受控单选 Select 组件。
//
// 业务意图：
// - 日志编码、AI 模型、设置项和后续连接表单都会出现“点击触发按钮后选择一个值”的交互。
// - 过去这些控件在各页面分别自绘，导致按钮尺寸、选中态、禁用态、浮层事件消费和图标布局不一致。
// - 本模块只抽取通用绘制与事件消费规则，具体 open/selected/anchor/options 状态和业务副作用仍由调用方持有。
//
// 关键约束：
// - GPUI 浮层通常画在日志正文、tab 或其它可交互区域上方，因此菜单壳层必须消费左右键按下事件，避免穿透到底层。
// - 第一阶段保持鼠标交互，不新增键盘导航；后续扩展时仍应维持“调用方受控状态、组件只渲染”的边界。
// - 本组件不保存任何业务值，也不负责关闭外层遮罩，避免和现有页面级弹层管理产生重复状态。

use super::*;

/// Select 菜单上下内边距。
///
/// 业务意图：
/// - 现有编码菜单使用 `py_1()`，也就是上下各 4px；把该数值集中到组件内，便于菜单高度计算和渲染保持一致。
/// - 该值只影响菜单内容区域的视觉留白，不改变调用方提供的每项高度。
const SELECT_MENU_VERTICAL_PADDING: f32 = 4.0;

/// 受控 Select 的单个选项。
///
/// 业务意图：
/// - 调用方用 `value` 参与业务状态比较和选择回调，用 `id` 生成稳定 GPUI 元素标识，用 `label` 展示用户可见文案。
/// - `leading_icon` 预留给后续带图标的设置项或连接类型选择，日志编码第一版不使用。
/// - `disabled` 允许菜单展示不可选项；禁用项不会挂载选择回调，点击由菜单壳层消费，避免透传到底层日志正文。
#[derive(Clone, Debug)]
pub(in crate::app) struct SelectOption<T> {
    /// 稳定选项标识，只用于元素 id 和测试断言，不作为用户可见文案。
    pub(in crate::app) id: String,
    /// 用户可见的单行标签。
    pub(in crate::app) label: String,
    /// 调用方持有并在选择后写回业务状态的值。
    pub(in crate::app) value: T,
    /// 可选前置图标，用于后续复用到更复杂选择器。
    pub(in crate::app) leading_icon: Option<Icon>,
    /// 是否禁止选择该项。
    pub(in crate::app) disabled: bool,
}

impl<T> SelectOption<T> {
    /// 创建一个默认可选的 Select 选项。
    ///
    /// 边界条件：
    /// - `id` 应由调用方保证在同一个菜单内稳定且唯一；组件不会为了避免业务语义变化而自动重写 id。
    /// - `label` 只按单行文本渲染，过长文案会在菜单项内裁剪，避免撑开浮层宽度。
    pub(in crate::app) fn new(id: impl Into<String>, label: impl Into<String>, value: T) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            leading_icon: None,
            disabled: false,
        }
    }

    /// 为选项添加前置图标。
    ///
    /// 业务意图：
    /// - 图标是纯视觉辅助，不参与选中态或业务值比较。
    #[allow(dead_code)]
    pub(in crate::app) fn with_leading_icon(mut self, icon: Icon) -> Self {
        self.leading_icon = Some(icon);
        self
    }

    /// 设置选项禁用状态。
    ///
    /// 业务意图：
    /// - 调用方可以把暂不可用的选项展示出来，但禁用项不能触发选择副作用。
    #[allow(dead_code)]
    pub(in crate::app) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// 返回当前选项是否允许被选择。
    ///
    /// 边界条件：
    /// - 该方法是渲染层和测试共享的最小状态判断，避免后续多个菜单各自写反禁用逻辑。
    pub(in crate::app) fn is_selectable(&self) -> bool {
        !self.disabled
    }
}

/// Select 按钮和菜单的尺寸配置。
///
/// 业务意图：
/// - Select 组件只统一交互与视觉结构，不强制所有业务使用相同宽高；调用方通过该结构复用自身已有布局尺寸。
/// - 日志编码迁移时沿用旧常量，确保按钮宽高、菜单宽度和菜单项密度不发生用户可见变化。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::app) struct SelectMetrics {
    /// 触发按钮宽度。
    pub(in crate::app) button_width: f32,
    /// 触发按钮高度。
    pub(in crate::app) button_height: f32,
    /// 弹层菜单宽度。
    pub(in crate::app) menu_width: f32,
    /// 单个菜单项高度。
    pub(in crate::app) item_height: f32,
    /// 菜单最大高度，超出后内部滚动。
    pub(in crate::app) max_menu_height: f32,
    /// 菜单和按钮底边的视觉间隔。
    pub(in crate::app) menu_gap: f32,
}

impl SelectMetrics {
    /// 创建一组 Select 尺寸。
    ///
    /// 边界条件：
    /// - 传入值按像素使用；调用方应避免传入 0 或负数，否则 GPUI 布局会得到不可点击的控件。
    /// - 本组件不做强制纠正，是为了让测试能直接暴露调用方错误尺寸。
    pub(in crate::app) const fn new(
        button_width: f32,
        button_height: f32,
        menu_width: f32,
        item_height: f32,
        max_menu_height: f32,
        menu_gap: f32,
    ) -> Self {
        Self {
            button_width,
            button_height,
            menu_width,
            item_height,
            max_menu_height,
            menu_gap,
        }
    }

    /// 根据选项数量计算菜单实际高度。
    ///
    /// 业务意图：
    /// - 少量选项时菜单高度刚好包住内容，选项较多时受 `max_menu_height` 限制并通过滚动访问剩余项。
    /// - 高度包含上下内边距，和 `render_select_menu` 中的 `py_1()` 保持一致。
    pub(in crate::app) fn menu_height(self, item_count: usize) -> f32 {
        let content_height =
            item_count as f32 * self.item_height + SELECT_MENU_VERTICAL_PADDING * 2.0;
        content_height.min(self.max_menu_height)
    }
}

/// Select 菜单在所属面板内的绝对定位锚点。
///
/// 业务意图：
/// - 不同页面的坐标系不同，例如日志编码菜单定位在右侧工作区内而不是整个窗口内。
/// - 调用方负责把鼠标或按钮位置换算到自身面板坐标，组件只按给定坐标绘制。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::app) struct SelectAnchor {
    /// 菜单左上角相对所属面板的横坐标。
    pub(in crate::app) x: f32,
    /// 菜单左上角相对所属面板的纵坐标。
    pub(in crate::app) y: f32,
}

impl SelectAnchor {
    /// 创建一个绝对定位锚点。
    pub(in crate::app) const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// 受控型单选 Select 组件。
///
/// 业务意图：
/// - 调用方只维护“是否打开、锚点位置、当前选中值和业务回调”，组件内部统一负责触发按钮、菜单壳层和菜单项渲染。
/// - 这样后续日志编码、连接类型、设置项等选择器可以复用同一套结构，而不是继续在页面里手工拼接 dropdown helper。
///
/// 关键约束：
/// - 组件不持有可变 UI 状态，`open_anchor` 只描述调用方当前受控状态，因此关闭遮罩、焦点和业务副作用仍由页面统一管理。
/// - `selected_value` 用于菜单选中态比较；触发按钮的展示文案由 `label` 传入，允许日志编码在自动模式下展示实际检测出的编码。
#[derive(Clone)]
pub(in crate::app) struct Select<T> {
    /// Select 在当前页面内的稳定标识，用于派生触发按钮、菜单和菜单项的 GPUI id。
    id: String,
    /// 触发按钮展示的用户可见文案。
    label: String,
    /// 当前选中的业务值，组件只用它判断菜单项选中态。
    selected_value: T,
    /// 调用方提供的完整选项列表；禁用项会展示但不会触发选择回调。
    options: Vec<SelectOption<T>>,
    /// 当前 Select 的按钮和菜单尺寸配置。
    metrics: SelectMetrics,
    /// 当前主题调色板快照，确保触发按钮和菜单在同一次渲染中颜色一致。
    palette: AppThemePalette,
    /// 是否允许打开和选择；禁用时触发按钮会消费点击但不进入业务逻辑。
    enabled: bool,
    /// 打开状态下的菜单锚点；为 `None` 时 `render_menu` 返回隐藏元素。
    open_anchor: Option<SelectAnchor>,
}

impl<T: Clone + Eq + 'static> Select<T> {
    /// 创建一个受控 Select 组件快照。
    ///
    /// 边界条件：
    /// - `id` 应在同一页面内稳定，避免 GPUI 在菜单打开和关闭时把不同控件识别为同一个元素。
    /// - `options` 可以为空；空菜单会只显示内边距高度，调用方通常应避免提供空选项。
    pub(in crate::app) fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        selected_value: T,
        options: Vec<SelectOption<T>>,
        metrics: SelectMetrics,
        palette: AppThemePalette,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            selected_value,
            options,
            metrics,
            palette,
            enabled: true,
            open_anchor: None,
        }
    }

    /// 设置 Select 是否可用。
    ///
    /// 业务意图：
    /// - 加载中的日志 tab 不能切换编码，但按钮仍要消费点击，避免用户误点后触发下层日志正文或浮层。
    pub(in crate::app) fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// 设置 Select 当前菜单锚点。
    ///
    /// 业务意图：
    /// - `Some` 表示菜单打开并按调用方坐标绘制，`None` 表示关闭。
    /// - 组件不自行计算坐标，是为了兼容日志右侧面板、设置页和弹窗等不同坐标系。
    pub(in crate::app) fn open_anchor(mut self, open_anchor: Option<SelectAnchor>) -> Self {
        self.open_anchor = open_anchor;
        self
    }

    /// 返回当前 Select 的选项数量。
    ///
    /// 业务意图：
    /// - 测试和少量调用方可用该方法断言受控组件收到的业务选项数量，避免重新暴露内部 `options` 可变集合。
    #[cfg(test)]
    pub(in crate::app) fn option_count(&self) -> usize {
        self.options.len()
    }

    /// 渲染 Select 触发按钮。
    ///
    /// 业务意图：
    /// - 调用方提供打开或收起菜单的点击回调，组件负责统一按钮外观、图标和禁用态。
    pub(in crate::app) fn render_trigger(
        &self,
        on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> gpui::Stateful<gpui::Div> {
        let open = self.enabled && self.open_anchor.is_some();
        render_select_trigger(
            format!("{}-trigger", self.id),
            self.label.clone(),
            self.enabled,
            open,
            self.metrics,
            self.palette,
            on_click,
        )
    }

    /// 渲染 Select 菜单。
    ///
    /// 业务意图：
    /// - 调用方只需要按业务值创建选择回调，菜单项的选中态、禁用态、hover 和点击消费全部由组件内部统一处理。
    /// - 使用回调工厂是因为 GPUI 事件监听闭包必须在每个菜单项上独立持有对应的业务值。
    ///
    /// 边界条件：
    /// - `open_anchor=None` 时返回隐藏元素，便于调用方始终把菜单节点挂在浮层区域而不额外分支。
    /// - `enabled=false` 时即使调用方残留了旧锚点，也会隐藏菜单，避免加载中或失败状态下还能选择旧项。
    /// - 禁用项不会调用 `on_select`，点击由菜单壳层消费，不会透传到底层日志行或 tab。
    pub(in crate::app) fn render_menu<F>(&self, mut on_select: F) -> gpui::Stateful<gpui::Div>
    where
        F: FnMut(T) -> Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>,
    {
        if !self.enabled {
            return div()
                .id(SharedString::from(format!("{}-menu-disabled", self.id)))
                .hidden();
        }
        let Some(anchor) = self.open_anchor else {
            return div()
                .id(SharedString::from(format!("{}-menu-empty", self.id)))
                .hidden();
        };
        let items = self
            .options
            .iter()
            .cloned()
            .map(|option| {
                let on_select = if option.is_selectable() {
                    Some(on_select(option.value.clone()))
                } else {
                    None
                };
                render_select_item(
                    &self.id,
                    option,
                    &self.selected_value,
                    self.metrics,
                    self.palette,
                    on_select,
                )
                .into_any_element()
            })
            .collect::<Vec<_>>();

        render_select_menu(
            format!("{}-menu", self.id),
            anchor,
            self.metrics,
            self.palette,
            items,
        )
    }
}

/// 渲染 Select 表单字段式触发区。
///
/// 业务意图：
/// - Select 的视觉语义是“当前值字段”，不是执行命令的按钮，因此外观应接近输入框而不是 dropdown 菜单按钮。
/// - 右侧只保留独立箭头图标作为 affordance，避免出现类似 split button 的箭头槽和分割线。
/// - `enabled=false` 时仍挂载点击监听，调用方监听会消费事件但不执行业务动作，避免禁用字段点击透传到下层控件。
fn render_select_trigger(
    id: impl Into<String>,
    label: impl Into<String>,
    enabled: bool,
    open: bool,
    metrics: SelectMetrics,
    palette: AppThemePalette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let border_color = if open { palette.accent } else { palette.border };
    let text_color = if enabled {
        palette.text
    } else {
        palette.muted_text
    };
    let icon_color = if enabled {
        palette.muted_text
    } else {
        palette.border
    };

    div()
        .id(SharedString::from(id.into()))
        .flex()
        .items_center()
        .justify_between()
        .w(px(metrics.button_width))
        .h(px(metrics.button_height))
        .px_2()
        .rounded(px(6.0))
        .border_1()
        .border_color(rgb(border_color))
        .bg(rgb(palette.panel))
        .text_sm()
        .text_color(rgb(text_color))
        .when(enabled, |button| {
            button.cursor_pointer().hover(move |button| {
                // Select hover 只强化边框，不改变背景，避免表现成普通按钮的 hover 面。
                button.border_color(rgb(if open {
                    palette.accent
                } else {
                    palette.accent_hover
                }))
            })
        })
        .when(!enabled, |button| button.opacity(0.62))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(label.into()),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .w(px(14.0))
                .ml_1()
                .child(MainView::render_lucide_icon(
                    Some(Icon::ChevronDown),
                    12.0,
                    12.0,
                    icon_color,
                )),
        )
        .on_click(on_click)
}

/// 渲染 Select listbox 菜单壳层。
///
/// 业务意图：
/// - Select 展开内容是当前字段的可选值列表，视觉上应是 listbox，而不是操作 dropdown 菜单。
/// - 菜单壳层统一负责绝对定位、宽度、高度、边框、阴影、内部滚动和事件消费。
/// - 点击菜单内边距、滚动条区域或禁用项时，事件不能穿透到下层日志正文、tab、目录树或其它可交互元素。
fn render_select_menu(
    id: impl Into<String>,
    anchor: SelectAnchor,
    metrics: SelectMetrics,
    palette: AppThemePalette,
    items: Vec<gpui::AnyElement>,
) -> gpui::Stateful<gpui::Div> {
    let menu_height = metrics.menu_height(items.len());
    div()
        .id(SharedString::from(id.into()))
        .absolute()
        .left(px(anchor.x))
        .top(px(anchor.y))
        .w(px(metrics.menu_width))
        .h(px(menu_height))
        .py_1()
        .rounded(px(4.0))
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .shadow_md()
        .overflow_y_scroll()
        .scrollbar_width(px(6.0))
        // 菜单是覆盖在业务内容上的浮层，必须阻断鼠标命中继续落到下层输入框、日志正文或 tab。
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            |_event: &MouseDownEvent, _window: &mut Window, context: &mut App| {
                // 菜单壳层要兜住内边距和禁用项区域，避免点击穿透到底层日志正文或 tab。
                context.stop_propagation();
            },
        )
        .on_mouse_down(
            MouseButton::Right,
            |_event: &MouseDownEvent, _window: &mut Window, context: &mut App| {
                // 右键点在菜单上不能打开底层日志正文、目录树或 tab 的右键菜单。
                context.stop_propagation();
            },
        )
        .children(items)
}

/// 渲染 Select 单个 listbox 选项。
///
/// 业务意图：
/// - 选中项使用强调色整行高亮，符合 select/listbox 的“当前值”视觉语义；不使用 Check 图标，避免看起来像命令菜单。
/// - 菜单项在 `mouse_down` 阶段执行选择，和现有编码菜单保持一致，避免浮层关闭后丢失 click 合成事件。
fn render_select_item<T: Clone + Eq + 'static>(
    select_id: impl Into<String>,
    option: SelectOption<T>,
    selected_value: &T,
    metrics: SelectMetrics,
    palette: AppThemePalette,
    on_select: Option<Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>>,
) -> gpui::Stateful<gpui::Div> {
    let selectable = option.is_selectable();
    let SelectOption {
        id,
        label,
        value,
        leading_icon,
        disabled,
    } = option;
    let selected = value == *selected_value;
    let text_color = if disabled {
        palette.muted_text
    } else if selected {
        palette.on_accent
    } else {
        palette.text
    };
    let background = if selected && !disabled {
        palette.accent
    } else {
        palette.surface
    };
    let leading_icon_color = if selected && !disabled {
        palette.on_accent
    } else {
        palette.muted_text
    };

    let item = div()
        .id(SharedString::from(format!(
            "{}-item-{}",
            select_id.into(),
            id
        )))
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .h(px(metrics.item_height))
        .px_2()
        .text_xs()
        .text_color(rgb(text_color))
        .bg(rgb(background))
        .when(selectable && !selected, |item| {
            item.cursor_pointer()
                .hover(move |item| item.bg(rgb(palette.hover)).text_color(rgb(palette.text)))
        })
        .when(selectable && selected, |item| {
            item.cursor_pointer().hover(move |item| {
                item.bg(rgb(palette.accent_hover))
                    .text_color(rgb(palette.on_accent))
            })
        })
        .when(!selectable, |item| item.opacity(0.58))
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .when_some(leading_icon, |row, icon| {
                    row.child(MainView::render_lucide_icon(
                        Some(icon),
                        12.0,
                        12.0,
                        leading_icon_color,
                    ))
                })
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(label),
                ),
        );

    if let Some(on_select) = on_select {
        item.on_mouse_down(MouseButton::Left, on_select)
    } else {
        item
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 验证选项 id 和禁用态不会被构造辅助函数意外改写。
    #[test]
    fn select_option_保留稳定_id_并支持禁用态() {
        let option = SelectOption::new("utf8", "UTF-8", 1_u8).disabled(true);

        assert_eq!(option.id, "utf8");
        assert_eq!(option.label, "UTF-8");
        assert_eq!(option.value, 1);
        assert!(!option.is_selectable());
    }

    /// 验证菜单高度同时包含内边距且不会超过调用方设定的最大高度。
    #[test]
    fn select_menu_height_受最大高度限制() {
        let metrics = SelectMetrics::new(90.0, 24.0, 120.0, 30.0, 80.0, 2.0);

        assert_eq!(metrics.menu_height(2), 68.0);
        assert_eq!(metrics.menu_height(5), 80.0);
    }

    /// 验证 Select 组件只保存调用方传入的受控状态，不自行派生业务选项或打开状态。
    #[test]
    fn select_组件保留受控状态() {
        let metrics = SelectMetrics::new(90.0, 24.0, 120.0, 30.0, 80.0, 2.0);
        let palette = AppThemePalette::for_theme(EffectiveTheme::Light);
        let anchor = SelectAnchor::new(12.0, 34.0);
        let select = Select::new(
            "encoding-select",
            "UTF-8",
            1_u8,
            vec![
                SelectOption::new("utf8", "UTF-8", 1_u8),
                SelectOption::new("gbk", "GBK", 2_u8).disabled(true),
            ],
            metrics,
            palette,
        )
        .enabled(false)
        .open_anchor(Some(anchor));

        assert_eq!(select.id, "encoding-select");
        assert_eq!(select.label, "UTF-8");
        assert_eq!(select.selected_value, 1);
        assert_eq!(select.option_count(), 2);
        assert!(!select.enabled);
        assert_eq!(select.open_anchor, Some(anchor));
    }

    /// 验证禁用项不会提前构造选择回调，避免调用方在回调工厂里记录日志、关闭菜单或执行其它副作用。
    #[test]
    fn select_禁用项不会构造选择回调() {
        let metrics = SelectMetrics::new(90.0, 24.0, 120.0, 30.0, 80.0, 2.0);
        let palette = AppThemePalette::for_theme(EffectiveTheme::Light);
        let select = Select::new(
            "encoding-select",
            "UTF-8",
            1_u8,
            vec![
                SelectOption::new("utf8", "UTF-8", 1_u8),
                SelectOption::new("gbk", "GBK", 2_u8).disabled(true),
            ],
            metrics,
            palette,
        )
        .open_anchor(Some(SelectAnchor::new(12.0, 34.0)));
        let callback_count = std::rc::Rc::new(std::cell::Cell::new(0));
        let callback_count_for_render = std::rc::Rc::clone(&callback_count);

        let _menu = select.render_menu(move |_value| {
            callback_count_for_render.set(callback_count_for_render.get() + 1);
            Box::new(|_event: &MouseDownEvent, _window: &mut Window, _context: &mut App| {})
        });

        assert_eq!(callback_count.get(), 1);
    }

    /// 验证禁用的 Select 即使残留打开锚点，也不会构造任何菜单项选择回调。
    #[test]
    fn select_禁用组件不会构造菜单回调() {
        let metrics = SelectMetrics::new(90.0, 24.0, 120.0, 30.0, 80.0, 2.0);
        let palette = AppThemePalette::for_theme(EffectiveTheme::Light);
        let select = Select::new(
            "encoding-select",
            "UTF-8",
            1_u8,
            vec![SelectOption::new("utf8", "UTF-8", 1_u8)],
            metrics,
            palette,
        )
        .enabled(false)
        .open_anchor(Some(SelectAnchor::new(12.0, 34.0)));
        let callback_count = std::rc::Rc::new(std::cell::Cell::new(0));
        let callback_count_for_render = std::rc::Rc::clone(&callback_count);

        let _menu = select.render_menu(move |_value| {
            callback_count_for_render.set(callback_count_for_render.get() + 1);
            Box::new(|_event: &MouseDownEvent, _window: &mut Window, _context: &mut App| {})
        });

        assert_eq!(callback_count.get(), 0);
    }
}
