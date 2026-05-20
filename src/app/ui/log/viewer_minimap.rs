// 日志正文右侧 VS Code 风格局部预览 minimap。
//
// 业务意图：
// - 在日志正文右侧提供类似 VS Code 的局部缩略预览，让用户快速感知当前视口附近的日志结构、搜索命中和当前位置。
// - 局部窗口优先绘制真实日志字符，并复用正文同源高亮；分页日志只读取当前局部窗口的连续行，读取失败时才退回字节长度轮廓。
//
// 关键约束：
// - minimap 每帧绘制量必须受当前视口高度限制，不能按真实行数创建元素或逐行绘制。
// - 点击和拖动只更新当前 tab 的滚动位置，不改变搜索结果、标记行或日志正文内容。
// - 所有鼠标事件都需要消费，避免预览栏操作穿透到底层正文行触发选区、右键菜单或行号标记。

use super::{viewer_view::LOG_SEARCH_KEYWORD_HIGHLIGHT, *};

/// minimap 每帧最多绘制的局部 bucket 数量。
///
/// 业务意图：
/// - 高分屏或超高窗口下，按固定间距采样仍可能很多；硬上限避免窗口被拉到极端高度时绘制成本失控。
/// - 局部窗口不再压缩整份日志，而是绘制当前视口上下文；上限允许比旧全量压缩更多行，以免宽屏下局部窗口被截断。
const LOG_MINIMAP_MAX_SAMPLE_LINES: usize = 1600;

/// 单行迷你文本最多分析的字符列数。
///
/// 边界条件：
/// - 日志行可能包含超长 JSON 或二进制乱码；minimap 只需要展示可视轮廓，不应为一行扫描任意长文本。
const LOG_MINIMAP_MAX_ANALYZED_COLUMNS: usize = 160;

/// minimap 中一个普通字符列降级轮廓对应的像素宽度。
///
/// 业务意图：
/// - 该值只用于缩略线段宽度，不参与正文真实排版；使用固定宽度可以让 macOS 和 Windows 的 minimap 形态稳定。
const LOG_MINIMAP_COLUMN_WIDTH: f32 = 0.72;

/// minimap 真实字符绘制的最小字号。
///
/// 业务意图：
/// - VS Code minimap 的字符非常小，但如果字号低于 1px，GPUI 和平台文本栅格化会接近不可见。
/// - 这里仅限制字符绘制字号，不改变视口块按正文宽高比计算的高度。
const LOG_MINIMAP_MIN_FONT_SIZE: f32 = 1.6;

/// minimap 真实字符绘制的最大字号。
///
/// 边界条件：
/// - 当窗口很窄导致按比例计算出的行高偏大时，过大的 minimap 字体会挤占右侧栏并显得不像缩略图。
const LOG_MINIMAP_MAX_FONT_SIZE: f32 = 4.0;

/// minimap 中最多绘制的搜索命中标记数。
///
/// 业务意图：
/// - 全文搜索可能返回数万条结果，预览栏只需要展示分布趋势；超过上限后继续绘制会让每帧重绘成本随结果数线性增长。
const LOG_MINIMAP_MAX_SEARCH_MARKERS: usize = 240;

/// minimap 中最多绘制的手动标记行数。
///
/// 边界条件：
/// - 手动标记通常很少；设置上限是为了防止异常状态或后续批量标记功能让预览栏重绘失控。
const LOG_MINIMAP_MAX_MANUAL_MARKERS: usize = 240;

/// minimap 预览数据来源。
///
/// 业务意图：
/// - 内存日志可以廉价读取当前采样行文本，从而绘制更像编辑器缩略图的片段。
/// - 分页日志不能随机读取采样行文本，只能使用行索引里的字节长度近似展示每行轮廓。
#[derive(Clone)]
enum LogMinimapDocumentSnapshot {
    /// 小文件完整行文本。
    InMemory {
        lines: Arc<Vec<String>>,
        /// 与正文相同的高亮模式。
        highlight_mode: crate::highlighting::HighlightMode,
        /// XML/properties 小文件的预计算高亮。
        precomputed_highlights: Option<Arc<crate::highlighting::PrecomputedHighlights>>,
    },
    /// 超大文件分页文档。
    Paged {
        document: log_document::PagedLogDocument,
    },
}

/// minimap 单条迷你文本片段。
///
/// 业务意图：
/// - `start_column` 和 `column_len` 使用缩略列而不是像素，绘制阶段再按 minimap 宽度换算。
/// - 当前每个采样行最多只保留一个轮廓片段，避免表格日志的列分隔被画成竖向条形码并拖慢滚动。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct LogMinimapSegment {
    /// 片段起始缩略列。
    pub(in crate::app) start_column: usize,
    /// 片段长度，单位为缩略列。
    pub(in crate::app) column_len: usize,
}

/// minimap 纵向滚动信息。
///
/// 业务意图：
/// - 普通内存日志和分页日志的真实滚动状态不同，但 minimap 只需要当前滚动距离、最大滚动距离和视口高度。
#[derive(Clone, Copy)]
pub(in crate::app) struct LogMinimapScrollInfo {
    /// 当前正文顶部滚动偏移。
    pub(in crate::app) scroll_top_px: f64,
    /// 正文最大可滚动距离。
    pub(in crate::app) max_scroll_px: f64,
    /// 正文视口宽度。
    pub(in crate::app) viewport_width_px: f64,
    /// 正文视口高度。
    pub(in crate::app) viewport_height_px: f64,
}

/// minimap 当前绘制窗口。
///
/// 业务意图：
/// - VS Code 的 minimap 只绘制当前视口附近的一段内容，而不是把整份文件压缩成一张全局缩略图。
/// - 该结构保存局部窗口和视口块的几何关系，保证正文可见区域在 minimap 中保持与真实正文相同的宽高比。
#[derive(Clone, Copy)]
pub(in crate::app) struct LogMinimapLayout {
    /// minimap 中一行真实日志对应的逻辑像素高度。
    pub(in crate::app) line_height_px: f32,
    /// 当前局部窗口的浮点起始行号，用于像素级滚动时避免内容一行一跳。
    pub(in crate::app) window_start_line_float: f64,
    /// 当前局部窗口缓存使用的整数起始行号。
    pub(in crate::app) window_start_line: usize,
    /// 当前局部窗口最多覆盖的真实行数。
    pub(in crate::app) window_line_count: usize,
    /// 当前局部窗口需要聚合的 bucket 数。
    pub(in crate::app) bucket_count: usize,
    /// 当前视口块顶部在 minimap 内的局部坐标。
    pub(in crate::app) viewport_block_top: Pixels,
    /// 当前视口块高度。
    pub(in crate::app) viewport_block_height: Pixels,
}

/// minimap 当前布局几何信息。
///
/// 业务意图：
/// - 点击和拖动事件使用窗口坐标；该结构把正文视口窗口坐标、minimap 高度和当前视口块位置集中起来，避免多处重复换算。
#[derive(Clone, Copy)]
struct LogMinimapGeometry {
    /// minimap 在窗口坐标中的顶部位置。
    viewport_top: Pixels,
    /// minimap 可绘制高度。
    height: Pixels,
    /// 当前视口块顶部在 minimap 内的局部坐标。
    viewport_block_top: Pixels,
    /// 当前视口块高度。
    viewport_block_height: Pixels,
    /// 正文最大可滚动距离。
    max_scroll_px: f64,
}

impl MainView {
    /// 渲染日志正文右侧 minimap。
    ///
    /// 业务意图：
    /// - 预览栏随存在纵向溢出的 Ready 日志默认出现，短日志不占用右侧固定宽度。
    /// - 使用 `canvas` 绘制，避免为每条采样线创建 GPUI 子元素；点击、拖动和滚轮由外层 div 处理。
    pub(in crate::app) fn render_log_minimap(
        &self,
        tab: &OpenLogTab,
        document: &LogTabDocument,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let tab_id = tab.id;
        if !Self::log_minimap_should_render_for_tab(tab) {
            return div()
                .id(SharedString::from(format!("log-minimap-empty-{tab_id}")))
                .hidden();
        }

        let line_count = document.line_count();
        let palette = self.palette();
        let snapshot = Self::log_minimap_document_snapshot(document);
        let cache_store = self.log.log_minimap_cache.clone();
        let source_key = tab.source_key.clone();
        let paged = matches!(document, LogTabDocument::Paged(_));
        let syntax_theme = self.effective_theme().syntax_theme();
        let search_markers = self.log_minimap_search_marker_lines_for_tab(tab);
        let marked_lines = tab
            .marked_lines
            .iter()
            .copied()
            .take(LOG_MINIMAP_MAX_MANUAL_MARKERS)
            .collect::<Vec<_>>();
        let highlighted_line = tab.highlighted_search_line;
        let scroll_info = Self::log_minimap_scroll_info_for_tab(tab);

        div()
            .id(SharedString::from(format!("log-minimap-area-{tab_id}")))
            .relative()
            .flex()
            .flex_none()
            .h_full()
            .w(px(LOG_MINIMAP_WIDTH))
            .overflow_hidden()
            .bg(rgb(palette.panel))
            .border_l_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .id(SharedString::from(format!("log-minimap-{tab_id}")))
                    .relative()
                    .flex_none()
                    .h_full()
                    .w(px(LOG_MINIMAP_WIDTH))
                    .overflow_hidden()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        context.listener(move |view, event: &MouseDownEvent, _window, context| {
                            view.start_log_minimap_drag(tab_id, event, context);
                            context.stop_propagation();
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        context.listener(|_view, _event: &MouseDownEvent, _window, context| {
                            // minimap 右键暂不提供菜单，但必须消费事件，避免穿透打开正文行右键菜单。
                            context.stop_propagation();
                        }),
                    )
                    .on_scroll_wheel(context.listener(
                        move |view, event: &ScrollWheelEvent, _window, context| {
                            view.handle_log_minimap_scroll_wheel(tab_id, event, context);
                            context.stop_propagation();
                        },
                    ))
                    .child(
                        gpui::canvas(
                            move |bounds, _window, _context| {
                                Self::log_minimap_render_cache(
                                    tab_id,
                                    &source_key,
                                    paged,
                                    &snapshot,
                                    line_count,
                                    scroll_info,
                                    syntax_theme,
                                    bounds.size.width,
                                    bounds.size.height,
                                    &cache_store,
                                )
                            },
                            move |bounds, cache, window, _context| {
                                Self::paint_log_minimap(
                                    bounds,
                                    &cache,
                                    scroll_info,
                                    &search_markers,
                                    &marked_lines,
                                    highlighted_line,
                                    palette,
                                    window,
                                    _context,
                                );
                            },
                        )
                        .size_full(),
                    ),
            )
    }

    /// 判断当前 Ready 日志是否需要显示 minimap。
    ///
    /// 业务意图：
    /// - 一屏内能完整显示的日志不需要右侧局部预览；隐藏 minimap 可以把 110px 宽度还给正文，并避免无效重绘。
    /// - 分页日志通常代表超大文件；minimap 即使只绘制局部窗口，也会在拖动和滚动时增加额外读取、分词和高亮成本，因此分页模式直接关闭。
    /// - 加载首帧如果还没有正文视口测量值，暂时不显示 minimap，等 GPUI 回填真实高度后再按溢出状态决定。
    ///
    /// 边界条件：
    /// - Loading/Failed 状态没有正文滚动信息，必须返回 false。
    /// - Paged 状态使用独立纵向滚动条，不显示右侧预览栏，也不为横向滚动条预留 minimap 宽度。
    /// - 空文件、单行文件和刚好填满视口的文件都没有纵向溢出，也不显示 minimap。
    pub(in crate::app) fn log_minimap_should_render_for_tab(tab: &OpenLogTab) -> bool {
        let LogTabState::Ready { document } = &tab.state else {
            return false;
        };
        if !Self::log_minimap_document_allows_render(document) {
            return false;
        }
        Self::log_minimap_scroll_info_for_tab(tab)
            .is_some_and(|scroll_info| scroll_info.max_scroll_px > 0.5)
    }

    /// 判断 Ready 文档类型是否允许显示 minimap。
    ///
    /// 业务意图：
    /// - 内存日志已经持有完整解码文本，minimap 可以复用这些行生成真实字符纹理和同源高亮。
    /// - 分页日志的核心目标是保护大文件性能；禁用 minimap 可以避免右侧预览在滚动过程中触发额外 I/O、缓存竞争和高亮计算。
    ///
    /// 边界条件：
    /// - 该判断只处理文档类型，不检查视口尺寸；是否真的有纵向溢出仍由 `log_minimap_should_render_for_tab` 统一决定。
    pub(in crate::app) fn log_minimap_document_allows_render(document: &LogTabDocument) -> bool {
        matches!(document, LogTabDocument::InMemory(_))
    }

    /// 为当前日志文档创建 minimap 绘制快照。
    ///
    /// 业务意图：
    /// - 快照只克隆 `Arc` 或行索引句柄，不复制整份日志正文。
    fn log_minimap_document_snapshot(document: &LogTabDocument) -> LogMinimapDocumentSnapshot {
        match document {
            LogTabDocument::InMemory(document) => LogMinimapDocumentSnapshot::InMemory {
                lines: document.lines.clone(),
                highlight_mode: document.highlight_mode,
                precomputed_highlights: document.precomputed_highlights.clone(),
            },
            LogTabDocument::Paged(document) => LogMinimapDocumentSnapshot::Paged {
                document: document.clone(),
            },
        }
    }

    /// 返回当前 tab 在 minimap 中需要展示的搜索命中行。
    ///
    /// 业务意图：
    /// - 底部搜索结果面板可能保存多次搜索历史；minimap 只绘制与当前 tab 来源一致的命中，避免不同文件结果互相污染。
    /// - 当前跳转高亮行额外加入集合，即使底部结果面板被关闭，用户仍能在预览栏看到最近定位位置。
    fn log_minimap_search_marker_lines_for_tab(&self, tab: &OpenLogTab) -> Vec<usize> {
        let records = self
            .search
            .search_results_panel
            .as_ref()
            .map(|panel| panel.records.as_slice())
            .unwrap_or(&[]);
        Self::log_minimap_search_marker_lines_for_records(
            &tab.source_key,
            tab.highlighted_search_line,
            records,
        )
    }

    /// 从搜索历史中提取当前来源对应的 minimap 命中行。
    ///
    /// 业务意图：
    /// - 该函数保持为纯数据转换，便于测试“只绘制当前 tab 来源”这一约束。
    pub(in crate::app) fn log_minimap_search_marker_lines_for_records(
        source_key: &str,
        highlighted_line: Option<usize>,
        records: &[SearchHistoryRecord],
    ) -> Vec<usize> {
        let mut marker_lines = BTreeSet::new();
        if let Some(highlighted_line) = highlighted_line {
            marker_lines.insert(highlighted_line);
        }

        for record in records.iter().rev() {
            for result in &record.results {
                if result.source_key == source_key {
                    marker_lines.insert(result.line_index);
                    if marker_lines.len() >= LOG_MINIMAP_MAX_SEARCH_MARKERS {
                        return marker_lines.into_iter().collect();
                    }
                }
            }
        }

        marker_lines.into_iter().collect()
    }

    /// 按当前视口高度计算 minimap 采样容量。
    ///
    /// 业务意图：
    /// - minimap 信息密度应跟可见高度相关，而不是跟日志真实行数相关；这样 1 千行和 1 千万行都能保持稳定绘制成本。
    #[cfg(test)]
    pub(in crate::app) fn log_minimap_sample_capacity(viewport_height: Pixels) -> usize {
        if viewport_height <= px(0.0) {
            return 0;
        }

        ((f32::from(viewport_height) / LOG_MINIMAP_SAMPLE_ROW_HEIGHT).ceil() as usize)
            .clamp(1, LOG_MINIMAP_MAX_SAMPLE_LINES)
    }

    /// 按本帧采样数量计算单条缩略线高度。
    ///
    /// 业务意图：
    /// - 当日志行数远大于 minimap 像素高度时，多行会被压缩到同一像素附近；缩略线高度必须跟随采样间距缩小，避免视觉重叠。
    /// - 当日志较短但仍需要滚动时，线高保持上限，让用户能看清字段长短轮廓。
    #[cfg(test)]
    pub(in crate::app) fn log_minimap_sample_line_height(
        sample_count: usize,
        viewport_height: Pixels,
    ) -> f32 {
        if sample_count == 0 || viewport_height <= px(0.0) {
            return 0.0;
        }

        let sample_pitch = f32::from(viewport_height) / sample_count as f32;
        sample_pitch
            .clamp(0.5_f32.min(sample_pitch), LOG_MINIMAP_LINE_HEIGHT)
            .min(sample_pitch)
    }

    /// 返回 minimap 本帧应该采样的真实行号。
    ///
    /// 边界条件：
    /// - 空文档返回空集合；单行文档只采样第 0 行。
    /// - 采样数量永远不超过 `log_minimap_sample_capacity`，避免超大日志按真实行数绘制。
    #[cfg(test)]
    pub(in crate::app) fn log_minimap_sample_line_indices(
        line_count: usize,
        viewport_height: Pixels,
    ) -> Vec<usize> {
        if line_count == 0 {
            return Vec::new();
        }
        let sample_count = Self::log_minimap_sample_capacity(viewport_height).min(line_count);
        if sample_count == 0 {
            return Vec::new();
        }
        if sample_count == 1 {
            return vec![0];
        }

        (0..sample_count)
            .map(|sample_index| {
                sample_index * line_count.saturating_sub(1) / sample_count.saturating_sub(1)
            })
            .collect()
    }

    /// 计算 VS Code 风格 minimap 的局部窗口布局。
    ///
    /// 业务意图：
    /// - minimap 视口块不再按“可见内容占全文比例”决定高度，而是按正文视口宽高比决定高度，使它看起来像真实正文窗口的缩小版。
    /// - 内容层只绘制当前视口上下文窗口；窗口起点由“正文当前行”和“视口块在 minimap 内的位置”共同决定，保证当前可见行落在视口块内部。
    ///
    /// 边界条件：
    /// - 空日志、无尺寸视口或一屏内日志不会进入有效布局。
    /// - 局部窗口起点同时夹紧文件首尾，滚到顶部或底部时不会请求负行号或超过真实行数。
    pub(in crate::app) fn log_minimap_layout_for_scroll(
        line_count: usize,
        minimap_width: Pixels,
        minimap_height: Pixels,
        scroll_info: LogMinimapScrollInfo,
    ) -> Option<LogMinimapLayout> {
        if line_count == 0
            || minimap_width <= px(0.0)
            || minimap_height <= px(0.0)
            || scroll_info.viewport_width_px <= 0.0
            || scroll_info.viewport_height_px <= 0.0
        {
            return None;
        }

        let block_height =
            Self::log_minimap_viewport_block_height(minimap_width, minimap_height, scroll_info);
        if block_height <= px(0.0) {
            return None;
        }

        let visible_line_count =
            (scroll_info.viewport_height_px / f64::from(px(LOG_VIEWER_ROW_HEIGHT))).max(1.0);
        let line_height_px = (f64::from(block_height) / visible_line_count).max(0.25) as f32;
        let visible_window_lines = (f64::from(minimap_height) / line_height_px as f64)
            .max(1.0)
            .min(line_count as f64);
        let block_top =
            Self::log_minimap_viewport_block_top(minimap_height, block_height, scroll_info);
        let top_line_float =
            (scroll_info.scroll_top_px / f64::from(px(LOG_VIEWER_ROW_HEIGHT))).max(0.0);
        let unclamped_start_line = top_line_float - f64::from(block_top) / line_height_px as f64;
        let max_start_line = (line_count as f64 - visible_window_lines).max(0.0);
        let window_start_line_float = unclamped_start_line.clamp(0.0, max_start_line);
        let window_start_line = window_start_line_float.floor() as usize;
        let window_line_count = ((visible_window_lines.ceil() as usize).saturating_add(2))
            .min(line_count.saturating_sub(window_start_line))
            .max(1);
        let bucket_count = window_line_count.min(LOG_MINIMAP_MAX_SAMPLE_LINES).max(1);

        Some(LogMinimapLayout {
            line_height_px,
            window_start_line_float,
            window_start_line,
            window_line_count,
            bucket_count,
            viewport_block_top: block_top,
            viewport_block_height: block_height,
        })
    }

    /// 返回当前 minimap 内容层缓存。
    ///
    /// 业务意图：
    /// - VS Code 类 minimap 的性能关键是把静态内容层缓存起来，像素级滚动时只移动内容偏移和视口覆盖层。
    /// - 这里按 tab 保存当前局部窗口的 bucket；当窗口跨过新的真实行范围时才重新聚合日志文本。
    ///
    /// 边界条件：
    /// - 视口高度为 0 时返回空缓存，避免首帧布局尚未完成时产生除零。
    /// - 缓存只保存内容轮廓，不包含搜索命中、手动标记和当前视口块，这些覆盖层每帧按最新状态绘制。
    #[allow(clippy::too_many_arguments)]
    fn log_minimap_render_cache(
        tab_id: usize,
        source_key: &str,
        paged: bool,
        snapshot: &LogMinimapDocumentSnapshot,
        line_count: usize,
        scroll_info: Option<LogMinimapScrollInfo>,
        syntax_theme: SyntaxTheme,
        minimap_width: Pixels,
        viewport_height: Pixels,
        cache_store: &Rc<RefCell<HashMap<usize, LogMinimapRenderCache>>>,
    ) -> LogMinimapRenderCache {
        let layout = scroll_info.and_then(|scroll_info| {
            Self::log_minimap_layout_for_scroll(
                line_count,
                minimap_width,
                viewport_height,
                scroll_info,
            )
        });
        let key = LogMinimapCacheKey {
            source_key: source_key.to_string(),
            line_count,
            viewport_width_px: scroll_info
                .map(|scroll_info| scroll_info.viewport_width_px.max(0.0).round() as u32)
                .unwrap_or(0),
            viewport_height_px: f32::from(viewport_height).max(0.0).round() as u32,
            window_start_line: layout.map(|layout| layout.window_start_line).unwrap_or(0),
            window_line_count: layout.map(|layout| layout.window_line_count).unwrap_or(0),
            bucket_count: layout.map(|layout| layout.bucket_count).unwrap_or(0),
            syntax_theme,
            paged,
        };
        if let Some(cache) = cache_store.borrow().get(&tab_id)
            && cache.key == key
        {
            return cache.clone();
        }

        let cache = LogMinimapRenderCache {
            buckets: Arc::new(layout.map_or_else(Vec::new, |layout| {
                Self::log_minimap_buckets(snapshot, line_count, layout, syntax_theme)
            })),
            key,
        };
        cache_store.borrow_mut().insert(tab_id, cache.clone());
        cache
    }

    /// 构造 minimap 像素 bucket。
    ///
    /// 业务意图：
    /// - 小文件使用当前局部窗口内的真实文本聚合缩进、长度和最高优先级色调，形成类似缩小文本块的密度变化。
    /// - 分页日志只读取行索引字节长度，避免因为右侧预览栏引入随机 seek/read。
    fn log_minimap_buckets(
        snapshot: &LogMinimapDocumentSnapshot,
        line_count: usize,
        layout: LogMinimapLayout,
        syntax_theme: SyntaxTheme,
    ) -> Vec<LogMinimapBucket> {
        if line_count == 0 || layout.window_line_count == 0 || layout.bucket_count == 0 {
            return Vec::new();
        }

        match snapshot {
            LogMinimapDocumentSnapshot::InMemory {
                lines,
                highlight_mode,
                precomputed_highlights,
            } => (0..layout.bucket_count)
                .map(|bucket_index| {
                    let (start_line, end_line) = Self::log_minimap_window_bucket_line_range(
                        layout.window_start_line,
                        layout.window_line_count,
                        layout.bucket_count,
                        bucket_index,
                        line_count,
                    );
                    Self::log_minimap_bucket_from_in_memory_lines(
                        lines,
                        *highlight_mode,
                        precomputed_highlights.as_deref(),
                        syntax_theme,
                        start_line,
                        end_line,
                    )
                })
                .collect(),
            LogMinimapDocumentSnapshot::Paged { document } => {
                let visible_lines = document
                    .read_visible_lines(layout.window_start_line, layout.window_line_count)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|line| (line.line_number, line.text))
                    .collect::<HashMap<_, _>>();
                (0..layout.bucket_count)
                    .map(|bucket_index| {
                        let (start_line, end_line) = Self::log_minimap_window_bucket_line_range(
                            layout.window_start_line,
                            layout.window_line_count,
                            layout.bucket_count,
                            bucket_index,
                            line_count,
                        );
                        Self::log_minimap_bucket_from_paged_document(
                            document,
                            &visible_lines,
                            syntax_theme,
                            start_line,
                            end_line,
                        )
                    })
                    .collect()
            }
        }
    }

    /// 返回某个 minimap bucket 覆盖的真实日志行范围。
    ///
    /// 业务意图：
    /// - 以 bucket 数量而非真实行数为布局基础，保证不同大小文件都压缩到稳定的预览高度。
    /// - 使用半开区间 `[start, end)`，便于内存日志和分页索引复用同一套聚合逻辑。
    pub(in crate::app) fn log_minimap_bucket_line_range(
        line_count: usize,
        bucket_count: usize,
        bucket_index: usize,
    ) -> (usize, usize) {
        if line_count == 0 || bucket_count == 0 {
            return (0, 0);
        }

        let safe_bucket_index = bucket_index.min(bucket_count - 1);
        let start = safe_bucket_index * line_count / bucket_count;
        let end = ((safe_bucket_index + 1) * line_count).div_ceil(bucket_count);
        (start.min(line_count), end.max(start + 1).min(line_count))
    }

    /// 返回局部 minimap 窗口中某个 bucket 覆盖的真实日志行范围。
    ///
    /// 业务意图：
    /// - VS Code 风格局部窗口只覆盖当前视口上下文的一段日志，该函数先在局部窗口内按 bucket 平均分段，再映射回真实行号。
    /// - 当窗口行数超过绘制上限时，一个 bucket 会聚合多行；窗口较小时则基本保持一行一个 bucket，预览形态更接近真实文本。
    ///
    /// 边界条件：
    /// - 起始行和结束行都会夹紧到真实日志行数，避免底部窗口或超大日志行数计算时越界。
    pub(in crate::app) fn log_minimap_window_bucket_line_range(
        window_start_line: usize,
        window_line_count: usize,
        bucket_count: usize,
        bucket_index: usize,
        total_line_count: usize,
    ) -> (usize, usize) {
        if window_line_count == 0 || bucket_count == 0 || total_line_count == 0 {
            return (0, 0);
        }

        let (local_start, local_end) =
            Self::log_minimap_bucket_line_range(window_line_count, bucket_count, bucket_index);
        let start = window_start_line
            .saturating_add(local_start)
            .min(total_line_count);
        let end = window_start_line
            .saturating_add(local_end)
            .max(start.saturating_add(1))
            .min(total_line_count);
        (start, end)
    }

    /// 从内存日志行聚合一个 minimap bucket。
    ///
    /// 业务意图：
    /// - 每个 bucket 只抽样少量真实行，避免 30MB 内存日志在首次显示或调整窗口高度时扫描全部文本。
    /// - 聚合结果使用最大行长和最小缩进，让表格日志形成稳定的整块轮廓，而不是每列画成竖条。
    fn log_minimap_bucket_from_in_memory_lines(
        lines: &[String],
        highlight_mode: crate::highlighting::HighlightMode,
        precomputed_highlights: Option<&crate::highlighting::PrecomputedHighlights>,
        syntax_theme: SyntaxTheme,
        start_line: usize,
        end_line: usize,
    ) -> LogMinimapBucket {
        let mut start_column = LOG_MINIMAP_MAX_ANALYZED_COLUMNS;
        let mut column_len = 1_usize;
        let mut tone = LogMinimapLineTone::Text;
        let mut display_text = None;
        let mut highlights = Vec::new();

        for line_index in Self::log_minimap_bucket_sample_lines(start_line, end_line) {
            let Some(line) = lines.get(line_index) else {
                continue;
            };
            if display_text.is_none() {
                let precomputed = precomputed_highlights
                    .filter(|_| syntax_theme == SyntaxTheme::Light)
                    .and_then(|highlights| highlights.lines.get(line_index));
                let (line_display_text, line_highlights) =
                    Self::log_minimap_display_text_and_highlights(
                        line,
                        highlight_mode,
                        precomputed,
                        syntax_theme,
                    );
                display_text = Some(line_display_text);
                highlights = line_highlights;
            }
            let Some(segment) = Self::log_minimap_segments_for_text(line).into_iter().next() else {
                continue;
            };
            start_column = start_column.min(segment.start_column);
            column_len = column_len.max(segment.start_column.saturating_add(segment.column_len));
            tone = Self::log_minimap_merge_tone(tone, Self::log_minimap_tone_for_line(line));
        }

        if start_column == LOG_MINIMAP_MAX_ANALYZED_COLUMNS {
            start_column = 0;
        }
        LogMinimapBucket {
            start_line,
            line_count: end_line.saturating_sub(start_line).max(1),
            start_column,
            column_len: column_len.saturating_sub(start_column).max(1),
            tone,
            display_text,
            highlights,
        }
    }

    /// 从分页日志局部窗口聚合一个 minimap bucket。
    ///
    /// 业务意图：
    /// - 分页日志只读取当前 minimap 局部窗口的连续行；这里优先使用已读取到的真实行文本，保证小窗口里也能看到真实字符。
    /// - 如果分页读取失败或某行缺失，再退回行索引字节长度轮廓，避免右侧预览影响正文可用性。
    fn log_minimap_bucket_from_paged_document(
        document: &log_document::PagedLogDocument,
        visible_lines: &HashMap<usize, String>,
        syntax_theme: SyntaxTheme,
        start_line: usize,
        end_line: usize,
    ) -> LogMinimapBucket {
        let mut column_len = 1_usize;
        let mut display_text = None;
        let mut highlights = Vec::new();
        for line_index in Self::log_minimap_bucket_sample_lines(start_line, end_line) {
            if let Some(line) = visible_lines.get(&line_index)
                && display_text.is_none()
            {
                let (line_display_text, line_highlights) =
                    Self::log_minimap_display_text_and_highlights(
                        line,
                        document.highlight_mode,
                        None,
                        syntax_theme,
                    );
                display_text = Some(line_display_text);
                highlights = line_highlights;
            }
            if let Some(entry) = document.line_index.get(line_index) {
                if let Some(segment) = Self::log_minimap_segments_for_byte_len(entry.byte_len)
                    .into_iter()
                    .next()
                {
                    column_len = column_len.max(segment.column_len);
                }
            }
        }

        LogMinimapBucket {
            start_line,
            line_count: end_line.saturating_sub(start_line).max(1),
            start_column: 0,
            column_len,
            tone: LogMinimapLineTone::Text,
            display_text,
            highlights,
        }
    }

    /// 构造 minimap 真实字符显示文本和正文同源高亮。
    ///
    /// 业务意图：
    /// - 正文渲染会先展开制表符，再把原始日志高亮映射到显示文本；minimap 复用相同流程，保证颜色和列位置一致。
    /// - minimap 只展示右侧栏宽度内有意义的前缀，超长行会同步截断文本和高亮范围，避免为不可见字符排版。
    pub(in crate::app) fn log_minimap_display_text_and_highlights(
        line: &str,
        highlight_mode: crate::highlighting::HighlightMode,
        precomputed: Option<&crate::highlighting::LineHighlights>,
        syntax_theme: SyntaxTheme,
    ) -> (String, Vec<(Range<usize>, gpui::HighlightStyle)>) {
        let line_highlights = highlight_line(highlight_mode, line, precomputed, syntax_theme);
        let expanded_line = Self::expanded_log_line_for_display(line);
        let display_highlights =
            Self::map_log_highlights_to_display(line_highlights, &expanded_line);
        let display_text = Self::log_minimap_truncate_display_text(&expanded_line.text);
        let display_highlights =
            Self::log_minimap_clamp_highlights_to_text(&display_text, display_highlights);
        (display_text, display_highlights)
    }

    /// 截断 minimap 中用于排版的显示文本。
    ///
    /// 边界条件：
    /// - 截断必须落在 UTF-8 字符边界；中文日志和包含 emoji 的日志不能产生非法字符串切片。
    fn log_minimap_truncate_display_text(text: &str) -> String {
        let end = text
            .char_indices()
            .map(|(index, _)| index)
            .nth(LOG_MINIMAP_MAX_ANALYZED_COLUMNS)
            .unwrap_or(text.len());
        text[..end].to_string()
    }

    /// 将高亮范围夹紧到 minimap 截断后的显示文本。
    fn log_minimap_clamp_highlights_to_text(
        text: &str,
        highlights: Vec<(Range<usize>, gpui::HighlightStyle)>,
    ) -> Vec<(Range<usize>, gpui::HighlightStyle)> {
        highlights
            .into_iter()
            .filter_map(|(range, style)| {
                let range = Self::clamp_search_text_range(text, range);
                (range.start < range.end).then_some((range, style))
            })
            .collect()
    }

    /// 返回一个 bucket 内用于聚合的代表行。
    ///
    /// 业务意图：
    /// - 每个 bucket 最多检查首行、中间两行和末行，既能捕捉日志形态变化，又能把缓存构建成本限制在 O(bucket 数)。
    /// - 对短范围去重，避免同一行被重复分析。
    fn log_minimap_bucket_sample_lines(start_line: usize, end_line: usize) -> Vec<usize> {
        if start_line >= end_line {
            return Vec::new();
        }
        let last_line = end_line - 1;
        let span = end_line - start_line;
        let mut lines = vec![
            start_line,
            start_line + span / 3,
            start_line + span / 2,
            last_line,
        ];
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// 合并 bucket 内多行的日志色调。
    ///
    /// 业务意图：
    /// - 错误优先于警告，警告优先于普通文本；一个 bucket 中只要出现高优先级日志，预览就应显示更醒目的色调。
    fn log_minimap_merge_tone(
        current: LogMinimapLineTone,
        next: LogMinimapLineTone,
    ) -> LogMinimapLineTone {
        if Self::log_minimap_tone_rank(next) > Self::log_minimap_tone_rank(current) {
            next
        } else {
            current
        }
    }

    /// 返回日志色调优先级。
    fn log_minimap_tone_rank(tone: LogMinimapLineTone) -> u8 {
        match tone {
            LogMinimapLineTone::Text => 0,
            LogMinimapLineTone::Muted => 1,
            LogMinimapLineTone::Warning => 2,
            LogMinimapLineTone::Error => 3,
        }
    }

    /// 将一行文本拆成 minimap 迷你文本片段。
    ///
    /// 业务意图：
    /// - 为了保证滚动流畅，每个采样行只画一条连续轮廓线，而不是按空白拆成多个矩形。
    /// - 连续轮廓仍保留缩进和行长差异；对表格日志尤其重要，否则重复列会形成不自然的竖向条纹。
    ///
    /// 边界条件：
    /// - 最多分析固定列数，超长行只展示开头轮廓。
    /// - Unicode 宽字符在 minimap 中按 1 列处理；这里不做真实排版，只提供粗略空间分布。
    pub(in crate::app) fn log_minimap_segments_for_text(line: &str) -> Vec<LogMinimapSegment> {
        let mut leading_columns = 0_usize;
        let mut visible_columns = 0_usize;
        let mut seen_text = false;

        for character in line.chars() {
            if leading_columns.saturating_add(visible_columns) >= LOG_MINIMAP_MAX_ANALYZED_COLUMNS {
                break;
            }

            let column_width = if character == '\t' {
                LOG_VIEWER_TAB_WIDTH
            } else {
                1
            };
            if !seen_text && character.is_whitespace() {
                leading_columns = leading_columns.saturating_add(column_width);
                continue;
            }

            seen_text = true;
            visible_columns = visible_columns.saturating_add(column_width);
        }

        vec![LogMinimapSegment {
            start_column: leading_columns.min(LOG_MINIMAP_MAX_ANALYZED_COLUMNS.saturating_sub(1)),
            column_len: visible_columns.max(1).min(LOG_MINIMAP_MAX_ANALYZED_COLUMNS),
        }]
    }

    /// 根据分页日志行字节长度生成单条近似轮廓。
    ///
    /// 业务意图：
    /// - 分页日志没有可用文本快照，使用字节长度可以展示长短行分布，同时保持 O(采样行数) 且不访问文件正文。
    pub(in crate::app) fn log_minimap_segments_for_byte_len(
        byte_len: u32,
    ) -> Vec<LogMinimapSegment> {
        vec![LogMinimapSegment {
            start_column: 0,
            column_len: (byte_len as usize)
                .max(1)
                .min(LOG_MINIMAP_MAX_ANALYZED_COLUMNS),
        }]
    }

    /// 粗略识别 minimap 行色调。
    ///
    /// 业务意图：
    /// - minimap 不做完整语法高亮，但错误和警告是日志排查最常用的视觉锚点，应在缩略图中保留。
    /// - 识别范围限制在行首固定列内，避免超长 JSON 或异常堆栈在滚动时因为色调判断扫描整行。
    pub(in crate::app) fn log_minimap_tone_for_line(line: &str) -> LogMinimapLineTone {
        let line = Self::log_minimap_prefix_for_analysis(line);
        if Self::log_minimap_contains_ascii_word(line, b"error")
            || Self::log_minimap_contains_ascii_word(line, b"exception")
            || Self::log_minimap_contains_ascii_word(line, b"fatal")
            || line.contains("失败")
            || line.contains("异常")
        {
            LogMinimapLineTone::Error
        } else if Self::log_minimap_contains_ascii_word(line, b"warn") || line.contains("警告") {
            LogMinimapLineTone::Warning
        } else if Self::log_minimap_contains_ascii_word(line, b"debug")
            || Self::log_minimap_contains_ascii_word(line, b"trace")
        {
            LogMinimapLineTone::Muted
        } else {
            LogMinimapLineTone::Text
        }
    }

    /// 在行前缀中执行无分配 ASCII 忽略大小写匹配。
    ///
    /// 业务意图：
    /// - minimap 在滚动时会频繁处理采样行，不能为每行调用 `to_ascii_lowercase` 分配临时字符串。
    /// - 日志级别关键字均为 ASCII，中文错误词由调用方继续使用普通 `contains` 处理。
    fn log_minimap_contains_ascii_word(text: &str, needle: &[u8]) -> bool {
        if needle.is_empty() || text.len() < needle.len() {
            return false;
        }

        text.as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
    }

    /// 返回 minimap 单行分析时允许读取的文本前缀。
    ///
    /// 业务意图：
    /// - minimap 只提供轮廓和异常分布提示，不能因为右侧预览栏对超长日志行做全文扫描。
    /// - 通过 UTF-8 字符边界截断，保证中文错误词和其它非 ASCII 文本不会被切出非法切片。
    fn log_minimap_prefix_for_analysis(line: &str) -> &str {
        if line.is_empty() {
            return line;
        }

        let end = line
            .char_indices()
            .map(|(index, _)| index)
            .nth(LOG_MINIMAP_MAX_ANALYZED_COLUMNS)
            .unwrap_or(line.len());
        &line[..end]
    }

    /// 绘制 minimap。
    ///
    /// 业务意图：
    /// - 绘制顺序从背景、迷你文本、搜索/标记线到视口块，保证当前位置始终可见。
    #[allow(clippy::too_many_arguments)]
    fn paint_log_minimap(
        bounds: Bounds<Pixels>,
        cache: &LogMinimapRenderCache,
        scroll_info: Option<LogMinimapScrollInfo>,
        search_markers: &[usize],
        marked_lines: &[usize],
        highlighted_line: Option<usize>,
        palette: AppThemePalette,
        window: &mut Window,
        context: &mut App,
    ) {
        window.paint_quad(fill(bounds, rgb(palette.panel)));
        let layout = scroll_info.and_then(|scroll_info| {
            Self::log_minimap_layout_for_scroll(
                cache.key.line_count,
                bounds.size.width,
                bounds.size.height,
                scroll_info,
            )
        });
        Self::paint_log_minimap_buckets(bounds, cache, layout, palette, window, context);
        Self::paint_log_minimap_markers(
            bounds,
            layout,
            search_markers,
            palette.search_highlight,
            0.78,
            window,
        );
        Self::paint_log_minimap_markers(bounds, layout, marked_lines, palette.accent, 0.86, window);
        if let Some(line_index) = highlighted_line {
            Self::paint_log_minimap_markers(
                bounds,
                layout,
                &[line_index],
                LOG_SEARCH_KEYWORD_HIGHLIGHT,
                0.95,
                window,
            );
        }
        Self::paint_log_minimap_viewport(bounds, layout, palette, window);
    }

    /// 绘制 minimap 中缓存好的内容 bucket。
    fn paint_log_minimap_buckets(
        bounds: Bounds<Pixels>,
        cache: &LogMinimapRenderCache,
        layout: Option<LogMinimapLayout>,
        palette: AppThemePalette,
        window: &mut Window,
        context: &mut App,
    ) {
        let Some(layout) = layout else {
            return;
        };
        if cache.key.line_count == 0 || cache.buckets.is_empty() || bounds.size.height <= px(0.0) {
            return;
        }

        let left = f32::from(bounds.left()) + LOG_MINIMAP_HORIZONTAL_PADDING;
        let max_width =
            (f32::from(bounds.size.width) - LOG_MINIMAP_HORIZONTAL_PADDING * 2.0).max(1.0);
        let line_height = Self::log_minimap_segment_height_for_line_height(layout.line_height_px);
        if line_height <= 0.0 {
            return;
        }
        for bucket in cache.buckets.iter() {
            let y = Self::log_minimap_y_for_line_in_window(
                bucket.start_line,
                layout.window_start_line_float,
                layout.line_height_px,
                bounds,
            );
            let bucket_height = (bucket.line_count as f32 * layout.line_height_px)
                .max(line_height)
                .min(f32::from(bounds.size.height));
            if y + bucket_height < f32::from(bounds.top()) || y > f32::from(bounds.bottom()) {
                continue;
            }
            let y = y.clamp(
                f32::from(bounds.top()) - bucket_height,
                f32::from(bounds.bottom()),
            );
            let color = Self::log_minimap_color_for_tone(bucket.tone, palette);
            if let Some(display_text) = bucket.display_text.as_deref()
                && !display_text.is_empty()
            {
                Self::paint_log_minimap_text_line(
                    display_text,
                    &bucket.highlights,
                    point(px(left), px(y)),
                    px(bucket_height.max(layout.line_height_px)),
                    layout.line_height_px,
                    palette,
                    window,
                    context,
                );
                continue;
            }
            let segment_x = left + bucket.start_column as f32 * LOG_MINIMAP_COLUMN_WIDTH;
            if segment_x >= left + max_width {
                continue;
            }
            let segment_width =
                (bucket.column_len as f32 * LOG_MINIMAP_COLUMN_WIDTH).clamp(1.0, max_width);
            let segment_width = segment_width.min(left + max_width - segment_x);
            window.paint_quad(fill(
                Bounds::new(
                    point(px(segment_x), px(y)),
                    size(px(segment_width), px(line_height.min(bucket_height))),
                ),
                color,
            ));
        }
    }

    /// 绘制搜索命中或手动标记行。
    fn paint_log_minimap_markers(
        bounds: Bounds<Pixels>,
        layout: Option<LogMinimapLayout>,
        lines: &[usize],
        color: u32,
        alpha: f32,
        window: &mut Window,
    ) {
        let Some(layout) = layout else {
            return;
        };
        if lines.is_empty() || bounds.size.height <= px(0.0) {
            return;
        }

        let mut marker_color = rgb(color);
        marker_color.a = alpha;
        let left = f32::from(bounds.left()) + 2.0;
        let width = (f32::from(bounds.size.width) - 4.0).max(1.0);
        for line_index in lines {
            let Some(y) = Self::log_minimap_marker_y_for_line_in_window(
                *line_index,
                layout.window_start_line_float,
                layout.line_height_px,
                bounds,
            ) else {
                continue;
            };
            window.paint_quad(fill(
                Bounds::new(point(px(left), px(y)), size(px(width), px(2.0))),
                marker_color,
            ));
        }
    }

    /// 绘制当前正文视口块。
    fn paint_log_minimap_viewport(
        bounds: Bounds<Pixels>,
        layout: Option<LogMinimapLayout>,
        palette: AppThemePalette,
        window: &mut Window,
    ) {
        let Some(layout) = layout else {
            return;
        };
        if bounds.size.height <= px(0.0) {
            return;
        }

        let block_height = layout.viewport_block_height;
        let block_top = layout.viewport_block_top;
        let mut background = rgb(palette.accent);
        background.a = 0.18;
        let mut border = rgb(palette.accent);
        border.a = 0.78;
        window.paint_quad(
            fill(
                Bounds::new(
                    point(bounds.left(), bounds.top() + block_top),
                    size(bounds.size.width, block_height),
                ),
                background,
            )
            .border_widths(px(1.0))
            .border_color(border),
        );
    }

    /// 返回局部窗口中某一真实行对应的 y 坐标。
    ///
    /// 业务意图：
    /// - minimap 当前只绘制正文视口上下文，因此行号不能再按全文百分比映射到高度，而要按局部窗口起点和迷你行高定位。
    fn log_minimap_y_for_line_in_window(
        line_index: usize,
        window_start_line_float: f64,
        line_height_px: f32,
        bounds: Bounds<Pixels>,
    ) -> f32 {
        f32::from(bounds.top())
            + ((line_index as f64 - window_start_line_float) * line_height_px as f64) as f32
    }

    /// 返回当前迷你行应该绘制的矩形高度。
    ///
    /// 业务意图：
    /// - 行距由正文宽高比推导，矩形本身保持较细，避免局部窗口密集行在视觉上粘成整块。
    fn log_minimap_segment_height_for_line_height(line_height_px: f32) -> f32 {
        line_height_px
            .clamp(0.5_f32.min(line_height_px), LOG_MINIMAP_LINE_HEIGHT)
            .min(line_height_px)
    }

    /// 绘制 minimap 中一行真实日志字符。
    ///
    /// 业务意图：
    /// - 用户需要看到接近 VS Code 的真实字符纹理，而不是简单行长横线；这里使用 GPUI 文本系统按极小字号排版代表行。
    /// - 文本 run 来自正文同源高亮，因此时间戳、数字、日志级别、线程名等颜色与正文保持一致。
    ///
    /// 边界条件：
    /// - 字号只影响字符栅格化，不改变视口块按正文宽高比计算出的几何高度。
    /// - 如果平台文本系统拒绝极小字号绘制，`paint` 失败会被忽略，正文查看器仍然可用。
    #[allow(clippy::too_many_arguments)]
    fn paint_log_minimap_text_line(
        display_text: &str,
        highlights: &[(Range<usize>, gpui::HighlightStyle)],
        origin: Point<Pixels>,
        line_height: Pixels,
        layout_line_height_px: f32,
        palette: AppThemePalette,
        window: &mut Window,
        context: &mut App,
    ) {
        let mut text_style = window.text_style();
        text_style.font_family = LOG_VIEWER_FONT_FAMILY.into();
        text_style.font_size = px(Self::log_minimap_font_size(layout_line_height_px)).into();
        text_style.color = rgb(palette.text).into();
        let runs = Self::log_minimap_text_runs(display_text, &text_style, highlights);
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let shaped_line = window.text_system().shape_line(
            SharedString::from(display_text.to_string()),
            font_size,
            &runs,
            None,
        );
        shaped_line.paint(origin, line_height, window, context).ok();
    }

    /// 返回 minimap 真实字符绘制字号。
    ///
    /// 业务意图：
    /// - 理论缩放字号来自局部行高，但 GPUI 走平台字体栅格化，过小字号会直接消失；设置下限保证用户能看到字符纹理。
    /// - 上限保证短日志或窄正文窗口中 minimap 仍保持“缩略图”外观，不变成第二个正文编辑器。
    fn log_minimap_font_size(line_height_px: f32) -> f32 {
        (line_height_px * 1.15).clamp(LOG_MINIMAP_MIN_FONT_SIZE, LOG_MINIMAP_MAX_FONT_SIZE)
    }

    /// 将正文高亮转换为 minimap 文本 run。
    ///
    /// 边界条件：
    /// - 高亮范围已经映射到 `display_text`，这里仍再次夹紧，避免截断文本或旧缓存导致排版越界。
    fn log_minimap_text_runs(
        display_text: &str,
        default_style: &gpui::TextStyle,
        highlights: &[(Range<usize>, gpui::HighlightStyle)],
    ) -> Vec<TextRun> {
        let mut runs = Vec::new();
        let mut cursor = 0usize;
        for (range, highlight) in highlights {
            let range = Self::clamp_search_text_range(display_text, range.clone());
            if range.start > cursor {
                runs.push(default_style.clone().to_run(range.start - cursor));
            }
            if range.start < range.end {
                runs.push(
                    default_style
                        .clone()
                        .highlight(highlight.clone())
                        .to_run(range.end - range.start),
                );
            }
            cursor = cursor.max(range.end);
        }
        if cursor < display_text.len() {
            runs.push(default_style.to_run(display_text.len() - cursor));
        }
        if runs.is_empty() {
            runs.push(default_style.to_run(display_text.len()));
        }
        runs
    }

    /// 返回局部窗口中搜索命中或手动标记行的 y 坐标。
    ///
    /// 边界条件：
    /// - 不在当前局部窗口内的全局标记不绘制，避免 minimap 看起来又退回“整份日志压缩图”。
    fn log_minimap_marker_y_for_line_in_window(
        line_index: usize,
        window_start_line_float: f64,
        line_height_px: f32,
        bounds: Bounds<Pixels>,
    ) -> Option<f32> {
        let y = Self::log_minimap_y_for_line_in_window(
            line_index,
            window_start_line_float,
            line_height_px,
            bounds,
        );
        if y < f32::from(bounds.top()) - 2.0 || y > f32::from(bounds.bottom()) {
            return None;
        }
        Some(y.clamp(f32::from(bounds.top()), f32::from(bounds.bottom()) - 2.0))
    }

    /// 把 minimap 内的 y 坐标换算成目标行号。
    ///
    /// 业务意图：
    /// - 点击预览栏空白处时，需要把用户点击位置映射到整份日志中的相对行号，再让正文滚动到该行附近。
    #[cfg(test)]
    pub(in crate::app) fn log_minimap_target_line_from_y(
        local_y: Pixels,
        viewport_height: Pixels,
        line_count: usize,
    ) -> Option<usize> {
        if line_count == 0 || viewport_height <= px(0.0) {
            return None;
        }
        if line_count == 1 {
            return Some(0);
        }

        let ratio = f64::from((local_y / viewport_height).clamp(0.0, 1.0));
        let line_index = ((line_count as f64) * ratio).floor() as usize;
        Some(line_index.min(line_count - 1))
    }

    /// 返回 minimap 当前视口块高度。
    ///
    /// 业务意图：
    /// - VS Code 的 minimap 视口块看起来像真实编辑器窗口的缩小版，因此高度由 minimap 宽度和正文视口宽高比共同决定。
    /// - 该高度不再按“当前可见内容占全文比例”计算，否则大日志会得到极薄滑块，和 VS Code 的局部窗口模型不一致。
    pub(in crate::app) fn log_minimap_viewport_block_height(
        minimap_width: Pixels,
        minimap_height: Pixels,
        scroll_info: LogMinimapScrollInfo,
    ) -> Pixels {
        if minimap_width <= px(0.0)
            || minimap_height <= px(0.0)
            || scroll_info.viewport_width_px <= 0.0
        {
            return px(0.0);
        }

        let aspect_height = f64::from(minimap_width)
            * (scroll_info.viewport_height_px / scroll_info.viewport_width_px);
        px((aspect_height as f32).clamp(1.0, f32::from(minimap_height)))
    }

    /// 返回 minimap 当前视口块顶部。
    fn log_minimap_viewport_block_top(
        minimap_height: Pixels,
        block_height: Pixels,
        scroll_info: LogMinimapScrollInfo,
    ) -> Pixels {
        let movable_height = (minimap_height - block_height).max(px(0.0));
        if movable_height <= px(0.0) || scroll_info.max_scroll_px <= 0.0 {
            return px(0.0);
        }

        let ratio = (scroll_info.scroll_top_px / scroll_info.max_scroll_px).clamp(0.0, 1.0);
        movable_height * ratio as f32
    }

    /// 取得当前 tab 的 minimap 几何信息。
    fn log_minimap_geometry_for_tab(&self, tab_id: usize) -> Option<LogMinimapGeometry> {
        let tab = self.log.open_tabs.iter().find(|tab| tab.id == tab_id)?;
        let bounds = Self::log_minimap_viewport_bounds_for_tab(tab)?;
        let scroll_info = Self::log_minimap_scroll_info_for_tab(tab)?;
        let block_height = Self::log_minimap_viewport_block_height(
            px(LOG_MINIMAP_WIDTH),
            bounds.size.height,
            scroll_info,
        );
        let block_top =
            Self::log_minimap_viewport_block_top(bounds.size.height, block_height, scroll_info);

        Some(LogMinimapGeometry {
            viewport_top: bounds.top(),
            height: bounds.size.height,
            viewport_block_top: block_top,
            viewport_block_height: block_height,
            max_scroll_px: scroll_info.max_scroll_px,
        })
    }

    /// 取得 minimap 对齐的正文视口边界。
    ///
    /// 业务意图：
    /// - minimap 与正文内容区等高，因此点击换算可以复用正文滚动视口的窗口坐标，不需要为 minimap 额外保存测量句柄。
    fn log_minimap_viewport_bounds_for_tab(tab: &OpenLogTab) -> Option<Bounds<Pixels>> {
        let bounds = match &tab.state {
            LogTabState::Ready { document } => match document.as_ref() {
                LogTabDocument::InMemory(_) => tab.scroll_handle.0.borrow().base_handle.bounds(),
                LogTabDocument::Paged(_) => tab.paged_viewport_handle.bounds(),
            },
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => return None,
        };
        (bounds.size.height > px(0.0)).then_some(bounds)
    }

    /// 取得 minimap 需要的纵向滚动信息。
    fn log_minimap_scroll_info_for_tab(tab: &OpenLogTab) -> Option<LogMinimapScrollInfo> {
        match &tab.state {
            LogTabState::Ready { document } => match document.as_ref() {
                LogTabDocument::InMemory(document) => {
                    let state = tab.scroll_handle.0.borrow();
                    let bounds = state.base_handle.bounds();
                    let viewport_height = f64::from(bounds.size.height);
                    if viewport_height <= 0.0 {
                        return None;
                    }
                    let content_height =
                        document.line_count() as f64 * f64::from(px(LOG_VIEWER_ROW_HEIGHT));
                    let max_scroll = (content_height - viewport_height).max(0.0);
                    let scroll_top = f64::from((-state.base_handle.offset().y).max(px(0.0)))
                        .clamp(0.0, max_scroll);
                    Some(LogMinimapScrollInfo {
                        scroll_top_px: scroll_top,
                        max_scroll_px: max_scroll,
                        viewport_width_px: f64::from(bounds.size.width),
                        viewport_height_px: viewport_height,
                    })
                }
                LogTabDocument::Paged(document) => {
                    let bounds = tab.paged_viewport_handle.bounds();
                    let viewport_height = bounds.size.height;
                    if viewport_height <= px(0.0) {
                        return None;
                    }
                    let max_scroll = Self::paged_log_vertical_max_scroll_px(
                        document.line_count(),
                        viewport_height,
                    );
                    Some(LogMinimapScrollInfo {
                        scroll_top_px: tab.paged_scroll.top_px.clamp(0.0, max_scroll),
                        max_scroll_px: max_scroll,
                        viewport_width_px: f64::from(bounds.size.width),
                        viewport_height_px: f64::from(viewport_height),
                    })
                }
            },
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => None,
        }
    }

    /// 开始拖动 minimap 当前视口块，或点击跳转到对应位置。
    ///
    /// 业务意图：
    /// - 点击视口块内部时进入拖动模式且不立即跳转；点击空白区域时先把视口块中心移动到点击位置，再进入拖动模式。
    pub(in crate::app) fn start_log_minimap_drag(
        &mut self,
        tab_id: usize,
        event: &MouseDownEvent,
        context: &mut Context<Self>,
    ) {
        self.note_keyboard_scroll_region(KeyboardScrollRegion::LogContent);
        let Some(geometry) = self.log_minimap_geometry_for_tab(tab_id) else {
            return;
        };
        let local_y = (event.position.y - geometry.viewport_top).clamp(px(0.0), geometry.height);
        let block_bottom = geometry.viewport_block_top + geometry.viewport_block_height;
        let cursor_offset = if local_y >= geometry.viewport_block_top && local_y <= block_bottom {
            local_y - geometry.viewport_block_top
        } else {
            geometry.viewport_block_height / 2.0
        };

        self.log.log_minimap_drag = Some(LogMinimapDrag {
            tab_id,
            cursor_offset,
        });
        self.log.log_scrollbar_drag = None;
        self.log.tab_context_menu = None;
        self.log.encoding_dropdown_menu = None;
        self.log.log_viewer_context_menu = None;
        self.stop_log_text_selection(context);
        self.apply_log_minimap_drag_position(tab_id, event.position.y, cursor_offset, context);
        context.notify();
    }

    /// 根据鼠标移动更新 minimap 拖动位置。
    pub(in crate::app) fn update_log_minimap_drag(
        &mut self,
        event: &MouseMoveEvent,
        context: &mut Context<Self>,
    ) {
        let Some(drag) = self.log.log_minimap_drag else {
            return;
        };
        if !event.dragging() {
            self.log.log_minimap_drag = None;
            context.notify();
            return;
        }

        self.apply_log_minimap_drag_position(
            drag.tab_id,
            event.position.y,
            drag.cursor_offset,
            context,
        );
    }

    /// 结束 minimap 拖动状态。
    pub(in crate::app) fn stop_log_minimap_drag(&mut self, context: &mut Context<Self>) {
        if self.log.log_minimap_drag.is_some() {
            self.log.log_minimap_drag = None;
            context.notify();
        }
    }

    /// 把 minimap 中的拖动位置写回正文滚动状态。
    fn apply_log_minimap_drag_position(
        &mut self,
        tab_id: usize,
        pointer_y: Pixels,
        cursor_offset: Pixels,
        context: &mut Context<Self>,
    ) {
        let Some(geometry) = self.log_minimap_geometry_for_tab(tab_id) else {
            self.log.log_minimap_drag = None;
            context.notify();
            return;
        };
        let scroll_top = Self::log_minimap_scroll_top_for_drag(
            pointer_y - geometry.viewport_top,
            cursor_offset,
            geometry.height,
            geometry.viewport_block_height,
            geometry.max_scroll_px,
        );
        self.set_log_tab_vertical_scroll_top(tab_id, scroll_top);
        context.notify();
    }

    /// 将 minimap 拖动位置换算为正文纵向滚动偏移。
    ///
    /// 边界条件：
    /// - 当前视口块因最小高度被放大时，仍按可移动轨道长度反推滚动比例，确保顶部和底部都能准确到达。
    pub(in crate::app) fn log_minimap_scroll_top_for_drag(
        local_y: Pixels,
        cursor_offset: Pixels,
        minimap_height: Pixels,
        block_height: Pixels,
        max_scroll_px: f64,
    ) -> f64 {
        if max_scroll_px <= 0.0 || minimap_height <= px(0.0) {
            return 0.0;
        }
        let movable_height = (minimap_height - block_height).max(px(0.0));
        if movable_height <= px(0.0) {
            return 0.0;
        }

        let block_top = (local_y - cursor_offset).clamp(px(0.0), movable_height);
        max_scroll_px * f64::from(block_top / movable_height)
    }

    /// 设置指定 tab 的纵向滚动偏移。
    ///
    /// 业务意图：
    /// - minimap 点击、拖动和滚轮都需要写入正文滚动位置；集中处理可以保证内存模式和分页模式夹紧规则一致。
    fn set_log_tab_vertical_scroll_top(&mut self, tab_id: usize, scroll_top_px: f64) {
        let Some(tab) = self.log.open_tabs.iter_mut().find(|tab| tab.id == tab_id) else {
            return;
        };

        match &tab.state {
            LogTabState::Ready { document } => match document.as_ref() {
                LogTabDocument::InMemory(document) => {
                    let bounds = tab.scroll_handle.0.borrow().base_handle.bounds();
                    let viewport_height = f64::from(bounds.size.height).max(1.0);
                    let content_height =
                        document.line_count() as f64 * f64::from(px(LOG_VIEWER_ROW_HEIGHT));
                    let max_scroll = (content_height - viewport_height).max(0.0);
                    let next_scroll = scroll_top_px.clamp(0.0, max_scroll);
                    let base_scroll_handle = { tab.scroll_handle.0.borrow().base_handle.clone() };
                    let current_offset = base_scroll_handle.offset();
                    base_scroll_handle
                        .set_offset(point(current_offset.x, px(-(next_scroll as f32))));
                }
                LogTabDocument::Paged(document) => {
                    let viewport_height = tab.paged_viewport_handle.bounds().size.height;
                    let max_scroll = Self::paged_log_vertical_max_scroll_px(
                        document.line_count(),
                        viewport_height,
                    );
                    tab.paged_scroll.top_px = scroll_top_px.clamp(0.0, max_scroll);
                }
            },
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => {}
        }
    }

    /// 处理 minimap 上的滚轮滚动。
    ///
    /// 业务意图：
    /// - minimap 位于正文右侧但仍属于日志阅读区域；鼠标停在预览栏上滚轮时应滚动当前正文，而不是没有响应。
    pub(in crate::app) fn handle_log_minimap_scroll_wheel(
        &mut self,
        tab_id: usize,
        event: &ScrollWheelEvent,
        context: &mut Context<Self>,
    ) {
        self.note_keyboard_scroll_region(KeyboardScrollRegion::LogContent);
        let pixel_delta = event.delta.pixel_delta(px(20.0));
        let Some(tab) = self.log.open_tabs.iter().find(|tab| tab.id == tab_id) else {
            return;
        };
        let Some(scroll_info) = Self::log_minimap_scroll_info_for_tab(tab) else {
            return;
        };
        let next_scroll_top = (scroll_info.scroll_top_px - f64::from(pixel_delta.y))
            .clamp(0.0, scroll_info.max_scroll_px);
        self.set_log_tab_vertical_scroll_top(tab_id, next_scroll_top);
        self.log.log_viewer_context_menu = None;
        context.notify();
    }

    /// 返回 minimap 采样行对应的绘制颜色。
    fn log_minimap_color_for_tone(
        tone: LogMinimapLineTone,
        palette: AppThemePalette,
    ) -> gpui::Rgba {
        let (color, alpha) = match tone {
            LogMinimapLineTone::Text => (palette.text, 0.36),
            LogMinimapLineTone::Muted => (palette.muted_text, 0.24),
            LogMinimapLineTone::Warning => (0xd29922, 0.78),
            LogMinimapLineTone::Error => (palette.error, 0.86),
        };
        let mut rgba = rgb(color);
        rgba.a = alpha;
        rgba
    }
}
