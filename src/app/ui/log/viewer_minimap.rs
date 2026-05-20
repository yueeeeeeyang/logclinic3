// 日志正文右侧 VS Code 风格局部预览 minimap。
//
// 业务意图：
// - 在日志正文右侧提供类似 VS Code 的局部缩略预览，让用户快速感知当前视口附近的日志结构、搜索命中和当前位置。
// - 局部窗口把真实日志字符和正文同源高亮先绘制到离屏位图；分页日志不显示 minimap，避免右侧预览影响大文件滚动性能。
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

/// minimap 静态文本层的额外缓存行数。
///
/// 业务意图：
/// - VS Code 的 minimap 内容层不是跟随每一个像素滚动都重建，而是缓存当前视口上下文的一段内容。
/// - 这里在当前可见范围上下多缓存一些行，滚轮和拖动时只平移离屏位图，跨过缓存边界后才重新采样日志文本。
///
/// 边界条件：
/// - 该值不能过大，否则打开普通日志时会一次性高亮和绘制过多不可见行；也不能过小，否则慢速滚动会频繁重建静态层。
const LOG_MINIMAP_CACHE_OVERSCAN_LINES: usize = 160;

/// minimap 静态文本层的起始行量化步长。
///
/// 业务意图：
/// - 普通日志每滚动一行都会改变局部窗口的理论起点；如果缓存键直接使用真实起点，仍会出现逐行重建位图的卡顿。
/// - 量化到固定行块后，同一小段滚动只改变图片偏移和视口覆盖层，显著降低滚轮与拖动时的主线程压力。
const LOG_MINIMAP_CACHE_CHUNK_LINES: usize = 192;

/// minimap 离屏位图允许的最大逻辑高度。
///
/// 业务意图：
/// - 极高显示器或异常缩放下，局部窗口位图可能超过常见 GPU 贴图尺寸；设置上限可以保护跨平台渲染稳定性。
/// - 超过上限时只裁剪远离当前视口的 overscan，当前可见区域仍优先保持完整。
const LOG_MINIMAP_MAX_IMAGE_LOGICAL_HEIGHT: f32 = 4096.0;

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

/// minimap 微缩字符图集的源网格宽度。
///
/// 业务意图：
/// - 真实平台字体无法在 GPUI 公开 API 中预渲染到应用私有 atlas；这里使用稳定的微型点阵字形模拟 VS Code minimap 的字符纹理。
/// - 点阵只用于右侧预览，正文仍使用正常字体和完整语法高亮。
const LOG_MINIMAP_GLYPH_GRID_WIDTH: usize = 3;

/// minimap 微缩字符图集的源网格高度。
///
/// 边界条件：
/// - 实际绘制高度会按当前行高和设备缩放裁剪；源网格保持 5 行可以让数字、字母和常见标点保留最基本差异。
const LOG_MINIMAP_GLYPH_GRID_HEIGHT: usize = 5;

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
/// - 分页日志默认不会显示 minimap；保留分页分支只用于测试纯函数和未来可选降级，不在渲染入口触发正文读取。
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
    Paged,
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

/// minimap 离屏静态层的简单 BGRA 位图。
///
/// 业务意图：
/// - GPUI 公开 API 不能直接复用内部字体 atlas；为了避免每帧 `shape_line`，minimap 先把微缩字符写入这块 CPU 位图。
/// - 位图最终直接构造成 GPUI `RenderImage`，跳过异步图片解码；这样滚动热路径只处理一张图片和少量覆盖矩形。
///
/// 边界条件：
/// - 位图始终按不透明背景初始化，避免 BMP 解码器和不同平台对 alpha 通道支持不一致导致右侧栏颜色发黑或透明。
struct LogMinimapBitmap {
    /// 位图宽度，单位为物理像素。
    width: u32,
    /// 位图高度，单位为物理像素。
    height: u32,
    /// BGRA8 像素数据，按从上到下、从左到右排列。
    ///
    /// 业务意图：
    /// - GPUI 底层图片 atlas 期望 BGRA 字节；直接保存 BGRA 可以避免每次缓存重建再做整张图片通道转换。
    pixels: Vec<u8>,
}

impl LogMinimapBitmap {
    /// 创建填充背景色的离屏位图。
    ///
    /// 业务意图：
    /// - minimap 静态层本身包含面板背景，图片平移时不会因为未绘制字符区域露出透明缝隙。
    fn new(width: u32, height: u32, background: gpui::Rgba) -> Self {
        let mut pixels = vec![0; width.saturating_mul(height).saturating_mul(4) as usize];
        let background = Self::rgba_to_bgra_bytes(background, 1.0);
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&background);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    /// 按 alpha 覆盖一个像素。
    ///
    /// 边界条件：
    /// - 字符缩放到极小尺寸时会产生大量亚像素位置；越界像素直接忽略，避免滚动到首尾时写出位图边界。
    fn blend_pixel(&mut self, x: i32, y: i32, color: gpui::Rgba, coverage: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }

        let alpha = (color.a * coverage).clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }
        let index = ((y as u32 * self.width + x as u32) * 4) as usize;
        let source = Self::rgba_to_bgra_bytes(color, alpha);
        for (channel, source_channel) in source.iter().take(3).enumerate() {
            let destination = self.pixels[index + channel] as f32;
            self.pixels[index + channel] =
                (destination * (1.0 - alpha) + *source_channel as f32 * alpha).round() as u8;
        }
        self.pixels[index + 3] = 255;
    }

    /// 绘制一个实心矩形。
    ///
    /// 业务意图：
    /// - 当某个 bucket 没有真实文本时，仍需要用行长轮廓保留日志结构；矩形绘制比字符绘制更便宜。
    fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: gpui::Rgba) {
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let left = x.floor().max(0.0) as i32;
        let top = y.floor().max(0.0) as i32;
        let right = (x + width).ceil().min(self.width as f32) as i32;
        let bottom = (y + height).ceil().min(self.height as f32) as i32;
        for pixel_y in top..bottom {
            for pixel_x in left..right {
                self.blend_pixel(pixel_x, pixel_y, color, 1.0);
            }
        }
    }

    /// 将 RGBA 颜色转换成 GPUI 图片 atlas 使用的 BGRA 字节。
    fn rgba_to_bgra_bytes(color: gpui::Rgba, alpha_override: f32) -> [u8; 4] {
        [
            (color.b.clamp(0.0, 1.0) * 255.0).round() as u8,
            (color.g.clamp(0.0, 1.0) * 255.0).round() as u8,
            (color.r.clamp(0.0, 1.0) * 255.0).round() as u8,
            (alpha_override.clamp(0.0, 1.0) * 255.0).round() as u8,
        ]
    }

    /// 直接构造 GPUI 可绘制的渲染图片。
    ///
    /// 业务意图：
    /// - 旧实现会把位图编码成 BMP，再让 GPUI 资产系统异步解码；滚动时新图片尚未解码会导致 minimap 消失。
    /// - 直接创建 `RenderImage` 后，paint 阶段可以同步提交到 GPU atlas，避免异步解码空窗和额外 CPU 开销。
    fn into_render_image(self) -> Arc<gpui::RenderImage> {
        let buffer = image::RgbaImage::from_raw(self.width, self.height, self.pixels)
            .unwrap_or_else(|| image::RgbaImage::new(1, 1));
        Arc::new(gpui::RenderImage::new(smallvec::SmallVec::from_elem(
            image::Frame::new(buffer),
            1,
        )))
    }
}

/// minimap 后台静态层构建使用的内存日志快照。
///
/// 业务意图：
/// - 分页日志已经在入口关闭 minimap，后台任务只需要携带可安全共享的内存日志数据，避免误把分页文档的 I/O 状态带到预览线程。
/// - 字段均为只读数据或 `Arc`，后台线程只生成派生位图，不修改正文文档、搜索结果或 UI 状态。
#[derive(Clone)]
struct LogMinimapStaticLayerSnapshot {
    /// 完整解码后的日志行集合。
    lines: Arc<Vec<String>>,
    /// 与正文一致的高亮模式。
    highlight_mode: crate::highlighting::HighlightMode,
    /// 小文件预计算高亮；没有预计算时后台任务会按正文同一套规则即时高亮代表行。
    precomputed_highlights: Option<Arc<crate::highlighting::PrecomputedHighlights>>,
}

/// minimap 后台静态层构建请求。
///
/// 业务意图：
/// - canvas prepare 阶段只创建这个轻量请求并立即返回，真正的行采样、高亮和位图写入放到 GPUI 后台执行器。
/// - 请求携带缓存键和序号，回到 UI 线程时可以精确判断是否仍然代表最新视口，避免快速拖动时旧结果覆盖新结果。
struct LogMinimapStaticLayerTaskRequest {
    /// 请求对应的缓存键。
    key: LogMinimapCacheKey,
    /// 请求派发时的任务序号。
    generation: u64,
    /// 只读内存日志快照。
    snapshot: LogMinimapStaticLayerSnapshot,
    /// 文档总行数。
    line_count: usize,
    /// 当前局部窗口布局。
    layout: LogMinimapLayout,
    /// 与正文一致的语法主题。
    syntax_theme: SyntaxTheme,
    /// minimap 逻辑宽度。
    minimap_width: Pixels,
    /// 当前窗口设备缩放因子。
    scale_factor: f32,
    /// 当前主题调色板。
    palette: AppThemePalette,
}

/// minimap 后台静态层构建结果。
///
/// 业务意图：
/// - 结果只包含可绘制图片和几何尺寸，不携带 bucket、高亮临时数据或日志文本，避免后台任务完成后长期占用额外内存。
/// - UI 线程合并时会根据 key 和 generation 决定接收或丢弃。
struct LogMinimapStaticLayerTaskResult {
    /// 结果对应的缓存键。
    key: LogMinimapCacheKey,
    /// 结果对应的任务序号。
    generation: u64,
    /// 已经生成好的静态字符层图片。
    static_image: Option<Arc<gpui::RenderImage>>,
    /// 图片逻辑宽度。
    image_logical_width_px: f32,
    /// 图片逻辑高度。
    image_logical_height_px: f32,
}

impl MainView {
    /// 渲染日志正文右侧 minimap。
    ///
    /// 业务意图：
    /// - 预览栏只在通用设置显式开启且 Ready 日志存在纵向溢出时出现，默认关闭以保护滚动性能。
    /// - 使用 `canvas` 绘制，避免为每条采样线创建 GPUI 子元素；点击、拖动和滚轮由外层 div 处理。
    pub(in crate::app) fn render_log_minimap(
        &self,
        tab: &OpenLogTab,
        document: &LogTabDocument,
        context: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let tab_id = tab.id;
        if !self.settings.log_minimap_enabled || !Self::log_minimap_should_render_for_tab(tab) {
            return div()
                .id(SharedString::from(format!("log-minimap-empty-{tab_id}")))
                .hidden();
        }

        let line_count = document.line_count();
        let palette = self.palette();
        let snapshot = Self::log_minimap_document_snapshot(document);
        let cache_store = self.log.log_minimap_cache.clone();
        let source_key = tab.source_key.clone();
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
        let view_entity = context.weak_entity();

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
                            move |bounds, window, context| {
                                Self::log_minimap_render_cache(
                                    tab_id,
                                    &source_key,
                                    &snapshot,
                                    line_count,
                                    scroll_info,
                                    syntax_theme,
                                    bounds.size.width,
                                    bounds.size.height,
                                    window.scale_factor(),
                                    palette,
                                    &cache_store,
                                    view_entity.clone(),
                                    context,
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
            LogTabDocument::Paged(_) => LogMinimapDocumentSnapshot::Paged,
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
        let desired_start_line = window_start_line_float.floor() as usize;
        let window_start_line = Self::log_minimap_quantized_window_start_line(desired_start_line);
        let visible_window_line_count = visible_window_lines.ceil() as usize;
        let requested_window_line_count = LOG_MINIMAP_CACHE_OVERSCAN_LINES
            .saturating_add(visible_window_line_count)
            .saturating_add(LOG_MINIMAP_CACHE_CHUNK_LINES)
            .saturating_add(LOG_MINIMAP_CACHE_OVERSCAN_LINES)
            .saturating_add(2);
        let window_line_count = requested_window_line_count
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

    /// 返回 minimap 静态文本层的量化起始行。
    ///
    /// 业务意图：
    /// - 离屏文本层必须覆盖当前视口上方一段 overscan，同时起点按固定块对齐，避免每滚动一行就生成新图片。
    /// - 该函数保持纯计算，测试可以直接验证小幅滚动是否复用同一个缓存窗口。
    pub(in crate::app) fn log_minimap_quantized_window_start_line(
        desired_start_line: usize,
    ) -> usize {
        let overscanned_start = desired_start_line.saturating_sub(LOG_MINIMAP_CACHE_OVERSCAN_LINES);
        overscanned_start / LOG_MINIMAP_CACHE_CHUNK_LINES * LOG_MINIMAP_CACHE_CHUNK_LINES
    }

    /// 返回当前 minimap 内容层缓存。
    ///
    /// 业务意图：
    /// - VS Code 类 minimap 的性能关键是把静态内容层缓存起来，像素级滚动时只移动内容偏移和视口覆盖层。
    /// - 这里按 tab 保存当前局部窗口的 bucket；当窗口跨过新的真实行范围时才重新聚合日志文本。
    ///
    /// 边界条件：
    /// - 视口高度为 0 时返回空缓存，避免首帧布局尚未完成时产生除零。
    /// - 缓存只保存静态字符位图，不包含搜索命中、手动标记和当前视口块，这些覆盖层每帧按最新状态绘制。
    #[allow(clippy::too_many_arguments)]
    fn log_minimap_render_cache(
        tab_id: usize,
        source_key: &str,
        snapshot: &LogMinimapDocumentSnapshot,
        line_count: usize,
        scroll_info: Option<LogMinimapScrollInfo>,
        syntax_theme: SyntaxTheme,
        minimap_width: Pixels,
        viewport_height: Pixels,
        scale_factor: f32,
        palette: AppThemePalette,
        cache_store: &Rc<RefCell<HashMap<usize, LogMinimapRenderCache>>>,
        view_entity: gpui::WeakEntity<MainView>,
        context: &mut App,
    ) -> LogMinimapRenderCache {
        let layout = scroll_info.and_then(|scroll_info| {
            Self::log_minimap_layout_for_scroll(
                line_count,
                minimap_width,
                viewport_height,
                scroll_info,
            )
        });
        let scale_factor = scale_factor.clamp(1.0, 4.0);
        let image_size = layout
            .map(|layout| Self::log_minimap_static_image_size(minimap_width, layout, scale_factor));
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
            image_width_px: image_size.map(|size| size.0).unwrap_or(0),
            image_height_px: image_size.map(|size| size.1).unwrap_or(0),
            scale_factor_milli: (scale_factor * 1000.0).round() as u32,
            palette_signature: Self::log_minimap_palette_signature(palette),
            syntax_theme,
            paged: matches!(snapshot, LogMinimapDocumentSnapshot::Paged),
        };
        let static_layer_snapshot = Self::log_minimap_static_layer_snapshot_from_document(snapshot);
        let mut task_request = None;
        let mut images_to_drop = Vec::new();
        let cache = {
            let mut cache_store = cache_store.borrow_mut();
            let cache = cache_store
                .entry(tab_id)
                .or_insert_with(|| LogMinimapRenderCache {
                    key: key.clone(),
                    static_image_key: None,
                    pending_key: None,
                    pending_generation: 0,
                    static_image: None,
                    image_logical_width_px: 0.0,
                    image_logical_height_px: 0.0,
                });

            if cache.key != key {
                cache.key = key.clone();
            }

            if layout.is_none() || static_layer_snapshot.is_none() {
                cache.pending_key = None;
                cache.static_image_key = None;
                cache.image_logical_width_px = 0.0;
                cache.image_logical_height_px = 0.0;
                if let Some(image) = cache.static_image.take() {
                    images_to_drop.push(image);
                }
            } else if cache.static_image_key.as_ref() != Some(&key)
                && Self::log_minimap_should_request_static_layer_build(
                    cache.static_image_key.as_ref(),
                    cache.pending_key.as_ref(),
                    &key,
                )
            {
                cache.pending_generation = cache.pending_generation.saturating_add(1);
                cache.pending_key = Some(key.clone());
                task_request = Some(LogMinimapStaticLayerTaskRequest {
                    key: key.clone(),
                    generation: cache.pending_generation,
                    snapshot: static_layer_snapshot.expect("上方已确认存在内存日志快照"),
                    line_count,
                    layout: layout.expect("上方已确认存在 minimap 布局"),
                    syntax_theme,
                    minimap_width,
                    scale_factor,
                    palette,
                });
            }

            cache.clone()
        };

        for image in images_to_drop {
            // 清理尺寸失效或文档失效的图片；正常跨块滚动时不在这里释放旧图，
            // 这样后台新图生成期间仍能用上一张静态层维持视觉连续性。
            context.drop_image(image, None);
        }
        if let Some(task_request) = task_request {
            Self::spawn_log_minimap_static_layer_task(tab_id, task_request, view_entity, context);
        }
        cache
    }

    /// 从 minimap 缓存中取出需要释放的静态图片。
    ///
    /// 业务意图：
    /// - `LogMinimapRenderCache` 持有的 `RenderImage` 会被 GPUI 上传到窗口 sprite atlas；关闭 tab 或重新解码时只丢弃
    ///   `Arc` 不能释放 atlas 里的纹理，必须集中取出图片并交给 `drop_image`。
    /// - 该函数只取静态图片，不处理 `pending_key`；在途后台任务如果稍后返回，会在合并结果时发现 cache 已不存在并自行释放结果图片。
    ///
    /// 边界条件：
    /// - 缓存可能只有 pending 任务还没有图片，返回 `None` 是正常状态。
    pub(in crate::app) fn log_minimap_take_static_image(
        cache: &mut LogMinimapRenderCache,
    ) -> Option<Arc<gpui::RenderImage>> {
        cache.static_image.take()
    }

    /// 释放一组 minimap 静态图片。
    ///
    /// 业务意图：
    /// - 多个关闭入口都会清理 tab 缓存；集中释放可以避免遗漏 GPUI atlas 清理，减少长时间打开/关闭日志后的 GPU 资源累积。
    fn drop_log_minimap_images(images: Vec<Arc<gpui::RenderImage>>, context: &mut Context<Self>) {
        for image in images {
            context.drop_image(image, None);
        }
    }

    /// 移除指定 tab 的 minimap 缓存并释放静态图片。
    ///
    /// 业务意图：
    /// - 日志重新加载、切换编码或关闭单个 tab 时，旧 minimap 位图已经不再对应任何可见正文，必须同步释放。
    pub(in crate::app) fn drop_log_minimap_cache_for_tab(
        &mut self,
        tab_id: usize,
        context: &mut Context<Self>,
    ) {
        let images = {
            let mut cache_store = self.log.log_minimap_cache.borrow_mut();
            cache_store
                .remove(&tab_id)
                .and_then(|mut cache| Self::log_minimap_take_static_image(&mut cache))
                .into_iter()
                .collect::<Vec<_>>()
        };
        Self::drop_log_minimap_images(images, context);
    }

    /// 仅保留指定 tab 的 minimap 缓存，并释放其它 tab 的静态图片。
    ///
    /// 业务意图：
    /// - “关闭其它 tab”会批量移除多个日志正文；对应 minimap 图片也要一次性从 GPUI atlas 中释放。
    pub(in crate::app) fn retain_log_minimap_cache_for_tab(
        &mut self,
        tab_id: usize,
        context: &mut Context<Self>,
    ) {
        let images = {
            let mut images = Vec::new();
            let mut cache_store = self.log.log_minimap_cache.borrow_mut();
            cache_store.retain(|cached_tab_id, cache| {
                if *cached_tab_id == tab_id {
                    true
                } else {
                    if let Some(image) = Self::log_minimap_take_static_image(cache) {
                        images.push(image);
                    }
                    false
                }
            });
            images
        };
        Self::drop_log_minimap_images(images, context);
    }

    /// 清空所有 minimap 缓存并释放静态图片。
    ///
    /// 业务意图：
    /// - “关闭所有 tab”和重新加载日志源会清空整个工作区；此时所有 minimap 位图都已失效，需要统一释放。
    pub(in crate::app) fn clear_log_minimap_cache(&mut self, context: &mut Context<Self>) {
        let images = {
            let mut cache_store = self.log.log_minimap_cache.borrow_mut();
            cache_store
                .drain()
                .filter_map(|(_, mut cache)| Self::log_minimap_take_static_image(&mut cache))
                .collect::<Vec<_>>()
        };
        Self::drop_log_minimap_images(images, context);
    }

    /// 判断当前请求是否需要派发新的 minimap 静态层后台构建。
    ///
    /// 业务意图：
    /// - 滚动热路径不能同步构建图片，也不能为同一个 key 重复派发任务；该纯函数集中表达去重规则，便于测试。
    /// - 如果当前图片已经匹配请求 key，则无需任务；如果已有任意后台任务，也继续复用旧图等待任务完成，避免快速拖动时堆积大量过期构建。
    #[cfg_attr(not(test), allow(dead_code))]
    pub(in crate::app) fn log_minimap_should_request_static_layer_build(
        static_image_key: Option<&LogMinimapCacheKey>,
        pending_key: Option<&LogMinimapCacheKey>,
        requested_key: &LogMinimapCacheKey,
    ) -> bool {
        static_image_key != Some(requested_key) && pending_key.is_none()
    }

    /// 从 minimap 文档快照提取可后台构建的内存日志数据。
    ///
    /// 业务意图：
    /// - 分页日志不显示 minimap，也不应该把分页读取句柄带入后台预览任务，避免右侧预览影响大文件滚动。
    /// - 返回独立结构可以让后台任务的 `Send` 边界只覆盖内存日志数据，而不是覆盖完整文档枚举。
    fn log_minimap_static_layer_snapshot_from_document(
        snapshot: &LogMinimapDocumentSnapshot,
    ) -> Option<LogMinimapStaticLayerSnapshot> {
        match snapshot {
            LogMinimapDocumentSnapshot::InMemory {
                lines,
                highlight_mode,
                precomputed_highlights,
            } => Some(LogMinimapStaticLayerSnapshot {
                lines: lines.clone(),
                highlight_mode: *highlight_mode,
                precomputed_highlights: precomputed_highlights.clone(),
            }),
            LogMinimapDocumentSnapshot::Paged => None,
        }
    }

    /// 派发 minimap 静态文本层后台任务。
    ///
    /// 业务意图：
    /// - UI 热路径只负责提交请求和绘制已有图片；后台任务完成后再回到主线程合并缓存并触发重绘。
    /// - 任务不持有 `Rc<RefCell<_>>` 或其它 UI 状态，避免跨线程访问 GPUI 视图对象。
    fn spawn_log_minimap_static_layer_task(
        tab_id: usize,
        request: LogMinimapStaticLayerTaskRequest,
        view_entity: gpui::WeakEntity<MainView>,
        context: &mut App,
    ) {
        context
            .spawn(async move |app| {
                let result = app
                    .background_executor()
                    .spawn(async move { Self::build_log_minimap_static_layer(request) })
                    .await;

                view_entity
                    .update(app, |view, context| {
                        view.apply_log_minimap_static_layer_result(tab_id, result, context);
                        context.notify();
                    })
                    .ok();
            })
            .detach();
    }

    /// 在后台线程构建 minimap 静态文本层。
    ///
    /// 业务意图：
    /// - 行采样、正文同源高亮、微缩字符写入位图是 minimap 最重的 CPU 工作，必须离开 UI 线程执行。
    /// - 该函数不访问任何 UI 状态；所有输入都来自任务请求，输出只是一张静态图片和尺寸信息。
    fn build_log_minimap_static_layer(
        request: LogMinimapStaticLayerTaskRequest,
    ) -> LogMinimapStaticLayerTaskResult {
        let buckets = Self::log_minimap_buckets_from_static_snapshot(
            &request.snapshot,
            request.line_count,
            request.layout,
            request.syntax_theme,
        );
        let (static_image, image_logical_width_px, image_logical_height_px) =
            Self::log_minimap_static_render_image(
                buckets.as_slice(),
                request.layout,
                request.minimap_width,
                request.scale_factor,
                request.palette,
            );

        LogMinimapStaticLayerTaskResult {
            key: request.key,
            generation: request.generation,
            static_image,
            image_logical_width_px,
            image_logical_height_px,
        }
    }

    /// 合并 minimap 静态文本层后台结果。
    ///
    /// 业务意图：
    /// - 快速滚动会让多个后台任务交错完成；只有仍匹配当前 pending key 和序号的结果才能进入缓存。
    /// - 被丢弃或被替换的图片需要通知 GPUI 释放资源，避免长时间拖动时积累过期贴图。
    fn apply_log_minimap_static_layer_result(
        &mut self,
        tab_id: usize,
        result: LogMinimapStaticLayerTaskResult,
        context: &mut Context<Self>,
    ) {
        let mut images_to_drop = Vec::new();
        {
            let mut cache_store = self.log.log_minimap_cache.borrow_mut();
            let Some(cache) = cache_store.get_mut(&tab_id) else {
                if let Some(image) = result.static_image {
                    images_to_drop.push(image);
                }
                drop(cache_store);
                for image in images_to_drop {
                    context.drop_image(image, None);
                }
                return;
            };

            let pending_matches = cache.pending_key.as_ref() == Some(&result.key)
                && cache.pending_generation == result.generation;
            let current_request_matches = cache.key == result.key;
            if pending_matches {
                cache.pending_key = None;
            }
            if !pending_matches || !current_request_matches {
                if let Some(image) = result.static_image {
                    images_to_drop.push(image);
                }
            } else {
                cache.static_image_key = Some(result.key);
                cache.image_logical_width_px = result.image_logical_width_px;
                cache.image_logical_height_px = result.image_logical_height_px;
                if let Some(previous_image) = cache.static_image.take() {
                    images_to_drop.push(previous_image);
                }
                cache.static_image = result.static_image;
            }
        }

        for image in images_to_drop {
            context.drop_image(image, None);
        }
    }

    /// 从后台静态层快照构造 minimap bucket。
    ///
    /// 业务意图：
    /// - 后台任务只服务普通内存日志，因此这里去掉分页分支，避免把分页 I/O 降级逻辑带进滚动优化路径。
    /// - 该函数复用原来的内存日志聚合逻辑，确保迁移到后台后视觉结果不变。
    fn log_minimap_buckets_from_static_snapshot(
        snapshot: &LogMinimapStaticLayerSnapshot,
        line_count: usize,
        layout: LogMinimapLayout,
        syntax_theme: SyntaxTheme,
    ) -> Vec<LogMinimapBucket> {
        if line_count == 0 || layout.window_line_count == 0 || layout.bucket_count == 0 {
            return Vec::new();
        }

        (0..layout.bucket_count)
            .map(|bucket_index| {
                let (start_line, end_line) = Self::log_minimap_window_bucket_line_range(
                    layout.window_start_line,
                    layout.window_line_count,
                    layout.bucket_count,
                    bucket_index,
                    line_count,
                );
                Self::log_minimap_bucket_from_in_memory_lines(
                    &snapshot.lines,
                    snapshot.highlight_mode,
                    snapshot.precomputed_highlights.as_deref(),
                    syntax_theme,
                    start_line,
                    end_line,
                )
            })
            .collect()
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
    #[cfg(test)]
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

    /// 返回主题调色板签名。
    ///
    /// 业务意图：
    /// - minimap 静态位图已经把颜色烘焙进像素，不能只依赖 `SyntaxTheme` 判断是否复用缓存。
    /// - 这里只挑选会进入 minimap 静态层或覆盖层边界的颜色，避免无关主题字段导致缓存失效。
    fn log_minimap_palette_signature(palette: AppThemePalette) -> u64 {
        let mut signature = 0xcbf2_9ce4_8422_2325_u64;
        for value in [
            palette.panel,
            palette.text,
            palette.muted_text,
            palette.error,
            palette.accent,
            palette.search_highlight,
            palette.border,
        ] {
            signature ^= value as u64;
            signature = signature.wrapping_mul(0x0000_0100_0000_01b3);
        }
        signature
    }

    /// 计算 minimap 静态文本层的物理像素尺寸。
    ///
    /// 业务意图：
    /// - 离屏图片按设备缩放因子生成，Retina 或 Windows 缩放屏上不会因为逻辑像素拉伸而模糊。
    /// - 高度来自缓存窗口真实覆盖行数，保证图片平移时上下 overscan 不露白。
    pub(in crate::app) fn log_minimap_static_image_size(
        minimap_width: Pixels,
        layout: LogMinimapLayout,
        scale_factor: f32,
    ) -> (u32, u32) {
        let logical_width = f32::from(minimap_width).max(1.0);
        let logical_height = Self::log_minimap_static_image_logical_height(layout);
        (
            (logical_width * scale_factor).ceil().max(1.0) as u32,
            (logical_height * scale_factor).ceil().max(1.0) as u32,
        )
    }

    /// 计算 minimap 静态文本层的逻辑高度。
    ///
    /// 边界条件：
    /// - 如果 overscan 在超高窗口下超过安全上限，只裁剪远离当前视口的部分，避免生成过大的贴图。
    fn log_minimap_static_image_logical_height(layout: LogMinimapLayout) -> f32 {
        (layout.window_line_count as f32 * layout.line_height_px)
            .ceil()
            .clamp(1.0, LOG_MINIMAP_MAX_IMAGE_LOGICAL_HEIGHT)
    }

    /// 构造 minimap 静态文本层图片。
    ///
    /// 业务意图：
    /// - 构建阶段一次性把局部窗口文本、高亮和降级轮廓写入离屏位图；paint 阶段只绘制图片本身。
    /// - 这对应 VS Code minimap 的“静态文本层”和“动态覆盖层”分离思路，可以显著降低滚动时 CPU 消耗。
    fn log_minimap_static_render_image(
        buckets: &[LogMinimapBucket],
        layout: LogMinimapLayout,
        minimap_width: Pixels,
        scale_factor: f32,
        palette: AppThemePalette,
    ) -> (Option<Arc<gpui::RenderImage>>, f32, f32) {
        if buckets.is_empty() {
            return (None, 0.0, 0.0);
        }

        let logical_width = f32::from(minimap_width).max(1.0);
        let logical_height = Self::log_minimap_static_image_logical_height(layout);
        let (image_width, image_height) =
            Self::log_minimap_static_image_size(minimap_width, layout, scale_factor);
        let mut bitmap = LogMinimapBitmap::new(image_width, image_height, rgb(palette.panel));
        let left = LOG_MINIMAP_HORIZONTAL_PADDING * scale_factor;
        let max_width = ((logical_width - LOG_MINIMAP_HORIZONTAL_PADDING * 2.0).max(1.0)
            * scale_factor)
            .max(1.0);
        let line_height_px = layout.line_height_px * scale_factor;
        let column_width_px = LOG_MINIMAP_COLUMN_WIDTH * scale_factor;
        let line_rect_height = line_height_px
            .clamp(
                1.0_f32.min(line_height_px),
                LOG_MINIMAP_LINE_HEIGHT * scale_factor,
            )
            .min(line_height_px.max(1.0));

        for bucket in buckets {
            let local_line = bucket.start_line.saturating_sub(layout.window_start_line);
            let y = local_line as f32 * line_height_px;
            if y > image_height as f32 {
                continue;
            }
            let bucket_height = (bucket.line_count as f32 * line_height_px)
                .max(line_rect_height)
                .min(image_height as f32 - y);
            let default_color = Self::log_minimap_color_for_tone(bucket.tone, palette);
            if let Some(display_text) = bucket.display_text.as_deref()
                && !display_text.is_empty()
            {
                Self::log_minimap_draw_text_line_to_bitmap(
                    &mut bitmap,
                    display_text,
                    &bucket.highlights,
                    left,
                    y,
                    max_width,
                    column_width_px,
                    bucket_height.max(line_height_px),
                    default_color,
                );
                continue;
            }

            let segment_x = left + bucket.start_column as f32 * column_width_px;
            if segment_x >= left + max_width {
                continue;
            }
            let segment_width = (bucket.column_len as f32 * column_width_px).clamp(1.0, max_width);
            let segment_width = segment_width.min(left + max_width - segment_x);
            bitmap.fill_rect(
                segment_x,
                y,
                segment_width,
                line_rect_height.min(bucket_height),
                default_color,
            );
        }

        (
            Some(bitmap.into_render_image()),
            logical_width,
            logical_height,
        )
    }

    /// 将一行真实日志字符绘制到 minimap 离屏位图。
    ///
    /// 业务意图：
    /// - 字符来自真实日志文本，颜色来自正文同源高亮；缩放后的点阵不可读但能形成接近 VS Code 的代码纹理。
    /// - 空格只推进列位置，不写像素，这样缩进、表格列和堆栈层级仍能在右侧预览中显现。
    #[allow(clippy::too_many_arguments)]
    fn log_minimap_draw_text_line_to_bitmap(
        bitmap: &mut LogMinimapBitmap,
        display_text: &str,
        highlights: &[(Range<usize>, gpui::HighlightStyle)],
        left: f32,
        y: f32,
        max_width: f32,
        column_width_px: f32,
        line_height_px: f32,
        default_color: gpui::Rgba,
    ) {
        let glyph_height = line_height_px
            .min(LOG_MINIMAP_GLYPH_GRID_HEIGHT as f32)
            .max(1.0);
        let glyph_width = column_width_px
            .min(LOG_MINIMAP_GLYPH_GRID_WIDTH as f32)
            .max(1.0);
        let glyph_y = y + ((line_height_px - glyph_height) / 2.0).max(0.0);
        let mut column = 0usize;

        for (byte_index, character) in display_text.char_indices() {
            if column >= LOG_MINIMAP_MAX_ANALYZED_COLUMNS {
                break;
            }
            let x = left + column as f32 * column_width_px;
            if x >= left + max_width {
                break;
            }

            if !character.is_whitespace() {
                let color =
                    Self::log_minimap_color_for_highlight(default_color, highlights, byte_index);
                Self::log_minimap_draw_micro_glyph(
                    bitmap,
                    character,
                    x,
                    glyph_y,
                    glyph_width,
                    glyph_height,
                    color,
                );
            }
            column = column.saturating_add(if character == '\t' {
                LOG_VIEWER_TAB_WIDTH
            } else {
                1
            });
        }
    }

    /// 绘制一个 minimap 微缩字符。
    ///
    /// 业务意图：
    /// - 每个字符使用预定义 3x5 点阵，构建静态层时直接写像素，后续滚动不再触发平台字体排版。
    /// - 当设备缩放或行高很小时，点阵会被裁剪为一两个像素；这和 VS Code minimap 的不可读但可扫视目标一致。
    fn log_minimap_draw_micro_glyph(
        bitmap: &mut LogMinimapBitmap,
        character: char,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: gpui::Rgba,
    ) {
        let rows = Self::log_minimap_micro_glyph_rows(character);
        if rows.iter().all(|row| *row == 0) {
            return;
        }

        let pixel_width = width.ceil().max(1.0) as usize;
        let pixel_height = height.ceil().max(1.0) as usize;
        for pixel_y in 0..pixel_height {
            let source_y = pixel_y * LOG_MINIMAP_GLYPH_GRID_HEIGHT / pixel_height;
            let row = rows[source_y.min(LOG_MINIMAP_GLYPH_GRID_HEIGHT - 1)];
            for pixel_x in 0..pixel_width {
                let source_x = pixel_x * LOG_MINIMAP_GLYPH_GRID_WIDTH / pixel_width;
                let bit = 1 << (LOG_MINIMAP_GLYPH_GRID_WIDTH - 1 - source_x);
                if row & bit != 0 {
                    bitmap.blend_pixel(
                        (x + pixel_x as f32).round() as i32,
                        (y + pixel_y as f32).round() as i32,
                        color,
                        1.0,
                    );
                }
            }
        }
    }

    /// 返回字符对应的 minimap 微型点阵。
    ///
    /// 业务意图：
    /// - 该表相当于应用内的极小字符 atlas，避免依赖平台字体栅格化，也避免新增字体或图片依赖。
    /// - 小写字母映射到大写点阵，非 ASCII 使用方块占位；这保留真实字符数量、列位置和高亮颜色，足够服务 minimap 扫视场景。
    pub(in crate::app) fn log_minimap_micro_glyph_rows(character: char) -> [u8; 5] {
        let character = character.to_ascii_uppercase();
        match character {
            ' ' | '\t' => [0, 0, 0, 0, 0],
            '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
            '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
            '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
            '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
            '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
            '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
            '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
            '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
            '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
            '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
            'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
            'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
            'C' => [0b111, 0b100, 0b100, 0b100, 0b111],
            'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
            'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
            'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
            'G' => [0b111, 0b100, 0b101, 0b101, 0b111],
            'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
            'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
            'J' => [0b001, 0b001, 0b001, 0b101, 0b111],
            'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
            'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
            'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
            'N' => [0b101, 0b111, 0b111, 0b111, 0b101],
            'O' => [0b111, 0b101, 0b101, 0b101, 0b111],
            'P' => [0b111, 0b101, 0b111, 0b100, 0b100],
            'Q' => [0b111, 0b101, 0b101, 0b111, 0b001],
            'R' => [0b111, 0b101, 0b111, 0b110, 0b101],
            'S' => [0b111, 0b100, 0b111, 0b001, 0b111],
            'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
            'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
            'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
            'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
            'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
            'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
            'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
            '-' => [0, 0, 0b111, 0, 0],
            '_' => [0, 0, 0, 0, 0b111],
            '.' => [0, 0, 0, 0, 0b010],
            ',' => [0, 0, 0, 0b010, 0b100],
            ':' => [0, 0b010, 0, 0b010, 0],
            ';' => [0, 0b010, 0, 0b010, 0b100],
            '/' => [0b001, 0b001, 0b010, 0b100, 0b100],
            '\\' => [0b100, 0b100, 0b010, 0b001, 0b001],
            '|' => [0b010, 0b010, 0b010, 0b010, 0b010],
            '(' | '[' | '{' => [0b011, 0b010, 0b010, 0b010, 0b011],
            ')' | ']' | '}' => [0b110, 0b010, 0b010, 0b010, 0b110],
            '<' => [0b001, 0b010, 0b100, 0b010, 0b001],
            '>' => [0b100, 0b010, 0b001, 0b010, 0b100],
            '=' => [0, 0b111, 0, 0b111, 0],
            '+' => [0, 0b010, 0b111, 0b010, 0],
            '*' => [0b101, 0b010, 0b111, 0b010, 0b101],
            '\'' | '"' | '`' => [0b010, 0b010, 0, 0, 0],
            '#' => [0b101, 0b111, 0b101, 0b111, 0b101],
            '@' => [0b111, 0b101, 0b111, 0b100, 0b111],
            '&' => [0b110, 0b100, 0b111, 0b101, 0b111],
            '%' => [0b101, 0b001, 0b010, 0b100, 0b101],
            '$' => [0b111, 0b110, 0b111, 0b011, 0b111],
            '!' => [0b010, 0b010, 0b010, 0, 0b010],
            '?' => [0b111, 0b001, 0b010, 0, 0b010],
            _ if character.is_ascii() => [0b111, 0b001, 0b010, 0b100, 0b111],
            _ => [0b111, 0b101, 0b111, 0b101, 0b111],
        }
    }

    /// 返回某个字节位置在 minimap 中应该使用的字符颜色。
    ///
    /// 业务意图：
    /// - 正文高亮范围以 UTF-8 字节偏移保存，minimap 绘制真实显示文本时沿用同一套范围，保证日期、数字、线程名等颜色和正文一致。
    fn log_minimap_color_for_highlight(
        default_color: gpui::Rgba,
        highlights: &[(Range<usize>, gpui::HighlightStyle)],
        byte_index: usize,
    ) -> gpui::Rgba {
        for (range, style) in highlights {
            if range.start <= byte_index && byte_index < range.end {
                let mut color = style
                    .color
                    .map(|color| color.to_rgb())
                    .unwrap_or(default_color);
                let fade = style.fade_out.unwrap_or(0.0).clamp(0.0, 0.85);
                color.a = if style.color.is_some() {
                    (0.88 * (1.0 - fade)).clamp(0.12, 0.95)
                } else {
                    (default_color.a * (1.0 - fade)).clamp(0.08, 0.95)
                };
                return color;
            }
        }
        default_color
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
        Self::paint_log_minimap_static_layer(bounds, cache, layout, window, context);
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

    /// 绘制 minimap 中缓存好的静态文本层。
    ///
    /// 业务意图：
    /// - 字符纹理已经在缓存构建阶段写入离屏图片，滚动时这里只根据浮点窗口起点平移图片。
    /// - 搜索命中、手动标记和当前视口块不包含在图片内，保证这些快速变化的覆盖层不触发图片重建。
    fn paint_log_minimap_static_layer(
        bounds: Bounds<Pixels>,
        cache: &LogMinimapRenderCache,
        layout: Option<LogMinimapLayout>,
        window: &mut Window,
        _context: &mut App,
    ) {
        let Some(layout) = layout else {
            return;
        };
        if cache.key.line_count == 0 || bounds.size.height <= px(0.0) {
            return;
        }
        let Some(static_image_key) = cache.static_image_key.as_ref() else {
            return;
        };
        let Some(image) = cache.static_image.clone() else {
            return;
        };
        if static_image_key.source_key != cache.key.source_key
            || static_image_key.line_count != cache.key.line_count
            || static_image_key.palette_signature != cache.key.palette_signature
            || static_image_key.syntax_theme != cache.key.syntax_theme
            || static_image_key.scale_factor_milli != cache.key.scale_factor_milli
        {
            return;
        }
        let y_offset = Self::log_minimap_static_layer_y_offset(
            layout.window_start_line_float,
            static_image_key.window_start_line,
            layout.line_height_px,
            cache.image_logical_height_px,
            f32::from(bounds.size.height),
            static_image_key == &cache.key,
        );
        let image_bounds = Bounds::new(
            point(bounds.left(), bounds.top() + px(y_offset)),
            size(
                px(cache.image_logical_width_px),
                px(cache.image_logical_height_px),
            ),
        );
        window
            .paint_image(image_bounds, Default::default(), image, 0, false)
            .ok();
    }

    /// 计算 minimap 静态图片在当前可视区内的纵向偏移。
    ///
    /// 业务意图：
    /// - 当前静态图匹配最新缓存 key 时，必须按真实行号精确平移，保证 minimap 文本和视口块对齐。
    /// - 快速拖动很远时，后台新图还没生成完成，旧图的真实偏移可能会落到可视区外；此时把旧图夹在可视区域内作为临时纹理，
    ///   避免右侧栏短暂变空，等后台新图返回后再恢复精确内容。
    ///
    /// 边界条件：
    /// - 图片高度小于可视区时无法完整覆盖右侧栏，直接贴顶显示，避免出现正负偏移来回抖动。
    /// - 行高、图片高度或可视高度异常时返回 0，保证首帧布局不稳定时不会产生 NaN 或无限偏移。
    pub(in crate::app) fn log_minimap_static_layer_y_offset(
        requested_window_start_line_float: f64,
        static_window_start_line: usize,
        line_height_px: f32,
        image_height_px: f32,
        viewport_height_px: f32,
        exact_key_match: bool,
    ) -> f32 {
        if line_height_px <= 0.0 || image_height_px <= 0.0 || viewport_height_px <= 0.0 {
            return 0.0;
        }

        let exact_offset = ((static_window_start_line as f64 - requested_window_start_line_float)
            * line_height_px as f64) as f32;
        if exact_key_match {
            return exact_offset;
        }
        if image_height_px <= viewport_height_px {
            return 0.0;
        }

        exact_offset.clamp(viewport_height_px - image_height_px, 0.0)
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

    /// 把 minimap 当前局部窗口内的 y 坐标换算成目标行号。
    ///
    /// 业务意图：
    /// - minimap 现在只显示当前视口上下文，而不是整份日志压缩图；点击位置必须映射到当前局部窗口里的真实日志行。
    /// - 该函数保持纯计算，测试可以覆盖顶部、底部和越界点击的夹紧规则。
    #[cfg(test)]
    pub(in crate::app) fn log_minimap_target_line_from_y(
        local_y: Pixels,
        layout: LogMinimapLayout,
        line_count: usize,
    ) -> Option<usize> {
        let target_line = Self::log_minimap_target_line_float_from_y(local_y, layout, line_count)?;
        Some(
            target_line
                .floor()
                .clamp(0.0, line_count.saturating_sub(1) as f64) as usize,
        )
    }

    /// 把 minimap 当前局部窗口内的 y 坐标换算成浮点目标行。
    ///
    /// 业务意图：
    /// - 点击跳转需要保留行内浮点位置，避免高 DPI 或小行高下多次点击同一区域全部落到同一整数行。
    /// - 返回值仍会夹紧到真实文件范围内，避免在首尾 overscan 或异常坐标下请求不存在的行。
    fn log_minimap_target_line_float_from_y(
        local_y: Pixels,
        layout: LogMinimapLayout,
        line_count: usize,
    ) -> Option<f64> {
        if line_count == 0 || layout.line_height_px <= 0.0 || layout.window_line_count == 0 {
            return None;
        }
        if line_count == 1 {
            return Some(0.0);
        }

        let local_line = (f64::from(local_y) / layout.line_height_px as f64)
            .clamp(0.0, layout.window_line_count.saturating_sub(1) as f64);
        Some((layout.window_start_line_float + local_line).clamp(0.0, line_count as f64 - 1.0))
    }

    /// 把 minimap 点击位置换算成正文滚动偏移。
    ///
    /// 业务意图：
    /// - 点击视口块外部时，用户点到的是 minimap 里的某一行局部文本，应把这行滚动到正文视口中部。
    /// - 该逻辑不同于拖动视口块；拖动仍按滚动条轨道百分比换算，点击则按当前局部窗口的真实行号换算。
    pub(in crate::app) fn log_minimap_scroll_top_for_click(
        local_y: Pixels,
        layout: LogMinimapLayout,
        scroll_info: LogMinimapScrollInfo,
        line_count: usize,
    ) -> Option<f64> {
        let target_line = Self::log_minimap_target_line_float_from_y(local_y, layout, line_count)?;
        let row_height = f64::from(px(LOG_VIEWER_ROW_HEIGHT));
        if row_height <= 0.0 {
            return Some(0.0);
        }
        let visible_lines = (scroll_info.viewport_height_px / row_height).max(1.0);
        let target_top_line = target_line - visible_lines / 2.0;
        Some((target_top_line * row_height).clamp(0.0, scroll_info.max_scroll_px))
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

    /// 开始拖动 minimap 当前视口块，或点击局部预览文本跳转到对应位置。
    ///
    /// 业务意图：
    /// - 点击视口块内部时进入拖动模式且不立即跳转，保证拖动滚动条手感稳定。
    /// - 点击视口块外部时，把点击处对应的局部日志行滚到正文中部；随后仍记录拖动状态，用户按住继续移动时可以连续拖动。
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
        let clicked_viewport_block =
            local_y >= geometry.viewport_block_top && local_y <= block_bottom;
        let cursor_offset = if clicked_viewport_block {
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
        if clicked_viewport_block {
            self.apply_log_minimap_drag_position(tab_id, event.position.y, cursor_offset, context);
        } else if let Some(scroll_top) =
            self.log_minimap_scroll_top_for_click_on_tab(tab_id, local_y)
        {
            self.set_log_tab_vertical_scroll_top(tab_id, scroll_top);
        }
        context.notify();
    }

    /// 计算当前 tab 在 minimap 点击位置对应的正文滚动偏移。
    ///
    /// 业务意图：
    /// - 点击位置需要结合当前日志行数、正文视口尺寸和局部窗口布局；集中在这里读取 tab 状态，避免事件处理函数重复分散业务规则。
    /// - 分页日志当前不显示 minimap，但该函数仍按文档类型通用读取行数，防止未来恢复分页降级预览时点击逻辑缺失。
    fn log_minimap_scroll_top_for_click_on_tab(
        &self,
        tab_id: usize,
        local_y: Pixels,
    ) -> Option<f64> {
        let tab = self.log.open_tabs.iter().find(|tab| tab.id == tab_id)?;
        let line_count = match &tab.state {
            LogTabState::Ready { document } => document.line_count(),
            LogTabState::Loading { .. } | LogTabState::Failed { .. } => return None,
        };
        let bounds = Self::log_minimap_viewport_bounds_for_tab(tab)?;
        let scroll_info = Self::log_minimap_scroll_info_for_tab(tab)?;
        let layout = Self::log_minimap_layout_for_scroll(
            line_count,
            px(LOG_MINIMAP_WIDTH),
            bounds.size.height,
            scroll_info,
        )?;
        Self::log_minimap_scroll_top_for_click(local_y, layout, scroll_info, line_count)
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
