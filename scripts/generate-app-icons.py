#!/usr/bin/env python3
"""LogClinic 应用图标生成脚本。

业务意图：
- 将用户提供的正方形 PNG 转换为仓库内稳定的 macOS `.icns` 和 Windows `.ico` 图标资源。
- 图标资源放在 `assets/icons`，供 macOS 打包脚本和 Windows 构建脚本复用，避免每次打包都依赖 Downloads 中的原始文件。

关键约束：
- 尺寸缩放依赖 macOS 自带的 `sips`，因此该脚本完整功能需要在 macOS 上执行。
- `.icns` 和 `.ico` 都使用 PNG 容器格式，由 Python 标准库直接写入，不依赖 Pillow、ImageMagick 或 iconutil。
- 原图必须是可被 `sips` 读取的正方形 PNG；脚本会按 macOS 常见图标外观添加透明圆角和安全边距，避免 Dock/Finder 中显示生硬且过大的方形。
"""

from __future__ import annotations

import argparse
import binascii
import math
import shutil
import struct
import subprocess
import sys
import zlib
from pathlib import Path


# 业务意图：这些尺寸覆盖 macOS iconset 和 Windows shell 常用图标尺寸，保证 Dock、Finder、任务栏和资源管理器缩放时都清晰。
ICON_SIZES = (16, 32, 48, 64, 128, 256, 512)

# macOS 基准图标画布尺寸。
MACOS_ICON_CANVAS_SIZE = 1024

# 业务意图：macOS Dock 中同一行图标通常不会让主体贴满整个 1024 画布；缩进到约 82% 可以让视觉尺寸接近系统常见图标。
MACOS_CONTENT_SCALE = 0.82

# 业务意图：macOS 图标通常是圆角方形；0.22 接近 1024px 图标约 220px 圆角的视觉比例，既保留主体也去掉生硬直角。
MACOS_CORNER_RADIUS_RATIO = 0.22

# 业务意图：AI 生成图标常把主体画在白色画布上；这里仅抠除从图片边缘连通进来的近白背景，保留图标内部文档纸张等白色主体。
EDGE_BACKGROUND_MIN_CHANNEL = 240

# 业务意图：背景可能不是纯白，允许少量 RGB 波动；阈值过大可能误伤蓝色主体的高光，因此保持保守。
EDGE_BACKGROUND_MAX_CHANNEL_DELTA = 24

# 业务意图：已有透明像素应被视为背景，避免后续边缘搜索被半透明空像素阻断。
EDGE_BACKGROUND_ALPHA_THRESHOLD = 8

# PNG 文件签名。
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


class PngImage:
    """脚本内部使用的 RGBA PNG 像素模型。

    业务意图：
    - 标准库没有直接的图片处理 API；这里仅实现当前图标生成所需的 8-bit RGB/RGBA 非交错 PNG 读写。
    - 转成 RGBA 后可以直接调整透明度，为 macOS 圆角图标写出透明角。
    """

    def __init__(self, width: int, height: int, rgba: bytearray) -> None:
        self.width = width
        self.height = height
        self.rgba = rgba


def run_command(command: list[str]) -> None:
    """执行外部图像处理命令，并在失败时给出可理解的错误。

    边界条件：
    - `sips` 或 `iconutil` 不存在时会直接抛出 `FileNotFoundError`，这里转换为中文提示，方便在非 macOS 环境定位原因。
    - 外部命令失败通常表示源图片损坏、格式不受支持或目标路径不可写，错误码会保留给调用方。
    """

    try:
        subprocess.run(command, check=True)
    except FileNotFoundError as error:
        missing = command[0]
        raise RuntimeError(f"缺少命令 {missing}，无法生成应用图标") from error
    except subprocess.CalledProcessError as error:
        raise RuntimeError(f"命令执行失败：{' '.join(command)}") from error


def resize_png(
    source: Path,
    target: Path,
    size: int,
    *,
    rounded: bool,
    remove_edge_background: bool = False,
) -> None:
    """使用 `sips` 生成指定尺寸的 PNG，并按需写入圆角透明蒙版。

    业务意图：
    - `sips` 是 macOS 系统自带工具，不需要为一次性图标缩放新增项目依赖。
    - 目标路径先删除再生成，避免旧尺寸文件残留导致 `.icns` 或 `.ico` 混入过期图标。
    - 圆角蒙版在缩放后按目标尺寸重新计算，避免小尺寸图标出现锯齿或圆角比例失真。
    - 部分源图自带白色画布，按需先把边缘连通的近白背景改成透明，避免最终 Dock 图标出现白边。
    """

    if target.exists():
        target.unlink()

    raw_target = target.with_name(f"{target.stem}.raw{target.suffix}")
    if raw_target.exists():
        raw_target.unlink()

    run_command(["sips", "-z", str(size), str(size), str(source), "--out", str(raw_target)])
    if rounded or remove_edge_background:
        image = read_png_rgba(raw_target)
        if remove_edge_background:
            remove_edge_connected_light_background(image)
        if rounded:
            apply_rounded_corners(image, MACOS_CORNER_RADIUS_RATIO)
        write_png_rgba(image, target)
        raw_target.unlink()
    else:
        raw_target.replace(target)


def build_macos_standard_source_icon(source: Path, target: Path) -> None:
    """生成带 macOS 安全边距的 1024px 源图标。

    业务意图：
    - 用户反馈 Dock 中图标比其它程序明显大一圈；根因是原图主体填满了整个 1024 画布。
    - macOS 常见图标会在画布边缘保留视觉安全区，这里把圆角主体缩放到 82% 后居中放在透明画布上。

    边界条件：
    - 透明画布保留完整 1024x1024 尺寸，确保 `.icns` 和 `.ico` 的标准尺寸不变，只调整视觉占比。
    - 主体圆角先按主体尺寸计算，再居中合成，避免外层透明画布被误当成图标实体。
    """

    inset_size = round(MACOS_ICON_CANVAS_SIZE * MACOS_CONTENT_SCALE)
    inset_path = target.with_name(f"{target.stem}.inset{target.suffix}")
    resize_png(source, inset_path, inset_size, rounded=True, remove_edge_background=True)

    inset_image = read_png_rgba(inset_path)
    canvas = PngImage(
        MACOS_ICON_CANVAS_SIZE,
        MACOS_ICON_CANVAS_SIZE,
        bytearray(MACOS_ICON_CANVAS_SIZE * MACOS_ICON_CANVAS_SIZE * 4),
    )
    paste_image_center(canvas, inset_image)
    write_png_rgba(canvas, target)
    inset_path.unlink()


def paste_image_center(canvas: PngImage, image: PngImage) -> None:
    """把图像居中粘贴到透明画布。

    业务意图：
    - 保持图标主体居中，避免 Dock/Finder 缩放时视觉重心偏移。
    - 当前画布完全透明，因此不需要复杂的 alpha 混合，直接复制 RGBA 像素即可。
    """

    x_offset = (canvas.width - image.width) // 2
    y_offset = (canvas.height - image.height) // 2
    for y in range(image.height):
        source_start = y * image.width * 4
        source_end = source_start + image.width * 4
        target_start = ((y + y_offset) * canvas.width + x_offset) * 4
        target_end = target_start + image.width * 4
        canvas.rgba[target_start:target_end] = image.rgba[source_start:source_end]


def remove_edge_connected_light_background(image: PngImage) -> None:
    """抠除从边缘连通进来的近白背景。

    业务意图：
    - 新图标的主体外部带有白色画布，如果直接缩放进 macOS 透明安全区，会在 Dock/Finder 中形成一圈明显白边。
    - 只从四条边做连通搜索，能删除外部画布，同时保留图标内部白色纸张、折角和高光。

    边界条件：
    - 近白阈值必须保守，避免误删蓝色底板的浅色高光。
    - 已经透明或几乎透明的像素也作为背景传播，兼容后续重新处理透明 PNG 的场景。
    """

    visited = bytearray(image.width * image.height)
    pending: list[tuple[int, int]] = []

    def enqueue_if_background(x: int, y: int) -> None:
        index = y * image.width + x
        if visited[index] != 0:
            return
        visited[index] = 1
        if is_edge_light_background_pixel(image, x, y):
            pending.append((x, y))

    for x in range(image.width):
        enqueue_if_background(x, 0)
        enqueue_if_background(x, image.height - 1)
    for y in range(image.height):
        enqueue_if_background(0, y)
        enqueue_if_background(image.width - 1, y)

    while pending:
        x, y = pending.pop()
        pixel_index = (y * image.width + x) * 4
        image.rgba[pixel_index : pixel_index + 4] = b"\x00\x00\x00\x00"

        if x > 0:
            enqueue_if_background(x - 1, y)
        if x + 1 < image.width:
            enqueue_if_background(x + 1, y)
        if y > 0:
            enqueue_if_background(x, y - 1)
        if y + 1 < image.height:
            enqueue_if_background(x, y + 1)


def is_edge_light_background_pixel(image: PngImage, x: int, y: int) -> bool:
    """判断像素是否属于可抠除的边缘浅色背景。"""

    pixel_index = (y * image.width + x) * 4
    red = image.rgba[pixel_index]
    green = image.rgba[pixel_index + 1]
    blue = image.rgba[pixel_index + 2]
    alpha = image.rgba[pixel_index + 3]

    if alpha <= EDGE_BACKGROUND_ALPHA_THRESHOLD:
        return True

    min_channel = min(red, green, blue)
    max_channel = max(red, green, blue)
    return (
        min_channel >= EDGE_BACKGROUND_MIN_CHANNEL
        and max_channel - min_channel <= EDGE_BACKGROUND_MAX_CHANNEL_DELTA
    )


def read_png_rgba(path: Path) -> PngImage:
    """读取 8-bit RGB/RGBA PNG 并转换为 RGBA 像素。

    边界条件：
    - `sips` 输出的图标 PNG 为非交错 RGB/RGBA；如果未来源图导致其它色彩类型，这里会明确报错，而不是生成错误图标。
    - PNG 每行可能使用 5 种过滤器，读取时必须反过滤，否则像素数据会被破坏。
    """

    data = path.read_bytes()
    if not data.startswith(PNG_SIGNATURE):
        raise RuntimeError(f"不是有效 PNG 文件：{path}")

    offset = len(PNG_SIGNATURE)
    width = height = bit_depth = color_type = None
    idat = bytearray()

    while offset + 8 <= len(data):
        length = struct.unpack(">I", data[offset : offset + 4])[0]
        chunk_type = data[offset + 4 : offset + 8]
        chunk_data = data[offset + 8 : offset + 8 + length]
        offset += 12 + length

        if chunk_type == b"IHDR":
            width, height, bit_depth, color_type, _compression, _filter, interlace = struct.unpack(
                ">IIBBBBB", chunk_data
            )
            if bit_depth != 8 or color_type not in (2, 6) or interlace != 0:
                raise RuntimeError(
                    f"仅支持 8-bit RGB/RGBA 非交错 PNG，当前文件不兼容：{path}"
                )
        elif chunk_type == b"IDAT":
            idat.extend(chunk_data)
        elif chunk_type == b"IEND":
            break

    if width is None or height is None or bit_depth is None or color_type is None:
        raise RuntimeError(f"PNG 缺少 IHDR：{path}")

    channels = 4 if color_type == 6 else 3
    row_length = width * channels
    raw = zlib.decompress(bytes(idat))
    expected = (row_length + 1) * height
    if len(raw) != expected:
        raise RuntimeError(f"PNG 像素长度不符合预期：{path}")

    rows = bytearray()
    previous = bytearray(row_length)
    position = 0
    for _row_index in range(height):
        filter_type = raw[position]
        position += 1
        current = bytearray(raw[position : position + row_length])
        position += row_length
        unfilter_png_row(current, previous, filter_type, channels)
        rows.extend(current)
        previous = current

    rgba = bytearray(width * height * 4)
    if color_type == 6:
        rgba[:] = rows
    else:
        for index in range(width * height):
            source_index = index * 3
            target_index = index * 4
            rgba[target_index : target_index + 3] = rows[source_index : source_index + 3]
            rgba[target_index + 3] = 255

    return PngImage(width, height, rgba)


def unfilter_png_row(
    current: bytearray, previous: bytearray, filter_type: int, bytes_per_pixel: int
) -> None:
    """还原 PNG 行过滤。

    业务意图：
    - PNG 为了压缩率会按行存储差分数据；直接使用过滤后的字节会导致颜色完全错误。

    边界条件：
    - 当前脚本只处理标准过滤器 0-4；遇到未知过滤器说明 PNG 损坏或格式异常，直接报错。
    """

    for index in range(len(current)):
        left = current[index - bytes_per_pixel] if index >= bytes_per_pixel else 0
        up = previous[index]
        up_left = previous[index - bytes_per_pixel] if index >= bytes_per_pixel else 0

        if filter_type == 0:
            value = current[index]
        elif filter_type == 1:
            value = current[index] + left
        elif filter_type == 2:
            value = current[index] + up
        elif filter_type == 3:
            value = current[index] + ((left + up) // 2)
        elif filter_type == 4:
            value = current[index] + paeth_predictor(left, up, up_left)
        else:
            raise RuntimeError(f"不支持的 PNG 过滤器类型：{filter_type}")

        current[index] = value & 0xFF


def paeth_predictor(left: int, up: int, up_left: int) -> int:
    """PNG Paeth 过滤器预测函数。"""

    estimate = left + up - up_left
    left_distance = abs(estimate - left)
    up_distance = abs(estimate - up)
    up_left_distance = abs(estimate - up_left)
    if left_distance <= up_distance and left_distance <= up_left_distance:
        return left
    if up_distance <= up_left_distance:
        return up
    return up_left


def apply_rounded_corners(image: PngImage, radius_ratio: float) -> None:
    """为图标应用透明圆角蒙版。

    业务意图：
    - 用户提供的原图是完整方形，在 macOS Dock/Finder 中会显得突兀；透明圆角让图标视觉上贴近系统应用图标。
    - 蒙版只影响四角透明度，不缩放或裁切图标主体，尽量保留原始设计内容。

    边界条件：
    - 小尺寸图标也按同一比例计算圆角，并使用 1 像素抗锯齿过渡，避免 16px/32px 图标边缘过硬。
    """

    radius = max(1.0, min(image.width, image.height) * radius_ratio)
    right_center = image.width - radius - 0.5
    bottom_center = image.height - radius - 0.5
    left_center = radius - 0.5
    top_center = radius - 0.5

    for y in range(image.height):
        for x in range(image.width):
            if radius <= x < image.width - radius or radius <= y < image.height - radius:
                continue

            corner_x = left_center if x < radius else right_center
            corner_y = top_center if y < radius else bottom_center
            distance = math.hypot(x - corner_x, y - corner_y)
            coverage = max(0.0, min(1.0, radius + 0.5 - distance))
            if coverage >= 1.0:
                continue

            alpha_index = (y * image.width + x) * 4 + 3
            image.rgba[alpha_index] = round(image.rgba[alpha_index] * coverage)


def write_png_rgba(image: PngImage, path: Path) -> None:
    """写出 RGBA PNG。

    业务意图：
    - 输出使用最简单的 filter 0 行格式，再交给 zlib 压缩；这样生成结果稳定、可复现，也足够满足图标文件体积要求。
    """

    raw = bytearray()
    row_length = image.width * 4
    for y in range(image.height):
        raw.append(0)
        start = y * row_length
        raw.extend(image.rgba[start : start + row_length])

    ihdr = struct.pack(">IIBBBBB", image.width, image.height, 8, 6, 0, 0, 0)
    png = bytearray(PNG_SIGNATURE)
    png.extend(png_chunk(b"IHDR", ihdr))
    png.extend(png_chunk(b"IDAT", zlib.compress(bytes(raw), level=9)))
    png.extend(png_chunk(b"IEND", b""))
    path.write_bytes(png)


def png_chunk(chunk_type: bytes, chunk_data: bytes) -> bytes:
    """构造 PNG chunk，并计算 CRC。

    边界条件：
    - PNG CRC 覆盖 chunk 类型和数据；不正确的 CRC 会导致 Finder、浏览器或 Windows shell 拒绝读取图标。
    """

    checksum = binascii.crc32(chunk_type + chunk_data) & 0xFFFFFFFF
    return (
        struct.pack(">I", len(chunk_data))
        + chunk_type
        + chunk_data
        + struct.pack(">I", checksum)
    )


def write_png_ico(png_paths: list[Path], ico_path: Path) -> None:
    """把多张 PNG 写入 Windows ICO 容器。

    业务意图：
    - Windows `.ico` 可以直接包含 PNG 编码图像；这种方式不需要手写 BMP/DIB 像素和透明掩码。
    - 每个目录项记录尺寸、颜色位深、数据大小和偏移量，Windows 会按显示场景选择最合适的尺寸。

    边界条件：
    - ICO 宽高字段用 1 字节表示，256 必须写成 0，这是 Windows ICO 格式规定。
    - 当前源图为 RGB PNG，颜色位深统一写 32 位，满足现代 Windows 图标透明度和缩放需求。
    """

    png_entries: list[tuple[int, bytes]] = []
    for path in png_paths:
        size = int(path.stem.split("_")[-1])
        png_entries.append((size, path.read_bytes()))

    header = struct.pack("<HHH", 0, 1, len(png_entries))
    directory_size = 16 * len(png_entries)
    offset = len(header) + directory_size
    directory = bytearray()
    payload = bytearray()

    for size, data in png_entries:
        dimension = 0 if size >= 256 else size
        directory.extend(
            struct.pack(
                "<BBBBHHII",
                dimension,
                dimension,
                0,
                0,
                1,
                32,
                len(data),
                offset,
            )
        )
        payload.extend(data)
        offset += len(data)

    ico_path.write_bytes(header + directory + payload)


def write_png_icns(png_paths: dict[str, Path], icns_path: Path) -> None:
    """把多张 PNG 写入 macOS ICNS 容器。

    业务意图：
    - macOS `.icns` 支持直接存放 PNG 编码图像；直接写 ICNS 容器可以避免 `iconutil` 对 RGB/alpha 等源图细节的严格校验。
    - 生成出的 `.icns` 仍包含 Finder 和 Dock 常用尺寸，供 `.app` 的 `CFBundleIconFile` 使用。

    边界条件：
    - ICNS 文件头由 `icns` 魔数和总长度组成，每个图标块由 4 字节类型、4 字节长度和 PNG 数据组成。
    - 这里只写现代 macOS 支持的 PNG 图标块，不写旧式 1-bit mask；目标系统最低版本已在打包脚本中声明为 macOS 12。
    """

    payload = bytearray()
    for chunk_type, path in png_paths.items():
        data = path.read_bytes()
        payload.extend(chunk_type.encode("ascii"))
        payload.extend(struct.pack(">I", len(data) + 8))
        payload.extend(data)

    header = b"icns" + struct.pack(">I", len(payload) + 8)
    icns_path.write_bytes(header + payload)


def generate_icons(source: Path, output_dir: Path) -> None:
    """生成仓库内应用图标资源。

    业务意图：
    - 保留一份归档后的源 PNG，便于后续重新生成不同平台图标。
    - `.iconset` 仅作为中间目录，生成 `.icns` 后删除，避免源码仓库出现大量重复尺寸的临时 PNG。
    """

    if not source.exists():
        raise RuntimeError(f"源图片不存在：{source}")

    output_dir.mkdir(parents=True, exist_ok=True)
    archived_source = output_dir / "app-icon.png"
    build_macos_standard_source_icon(source, archived_source)

    iconset_dir = output_dir / "LogClinic.iconset"
    if iconset_dir.exists():
        shutil.rmtree(iconset_dir)
    iconset_dir.mkdir(parents=True)

    # macOS iconset 需要同时提供 1x 和 2x 命名；512@2x 对应最终 1024 图标。
    mac_icon_specs = (
        ("icon_16x16.png", 16, "icp4"),
        ("icon_16x16@2x.png", 32, "icp5"),
        ("icon_32x32.png", 32, "icp5"),
        ("icon_32x32@2x.png", 64, "icp6"),
        ("icon_128x128.png", 128, "ic07"),
        ("icon_128x128@2x.png", 256, "ic08"),
        ("icon_256x256.png", 256, "ic08"),
        ("icon_256x256@2x.png", 512, "ic09"),
        ("icon_512x512.png", 512, "ic09"),
        ("icon_512x512@2x.png", 1024, "ic10"),
    )
    icns_pngs: dict[str, Path] = {}
    for filename, size, chunk_type in mac_icon_specs:
        target = iconset_dir / filename
        resize_png(archived_source, target, size, rounded=False)
        icns_pngs[chunk_type] = target

    icns_path = output_dir / "LogClinic.icns"
    if icns_path.exists():
        icns_path.unlink()
    write_png_icns(icns_pngs, icns_path)
    shutil.rmtree(iconset_dir)

    # Windows ICO 复用 `sips` 生成的多尺寸 PNG，然后写入单个 ICO 容器。
    ico_png_dir = output_dir / "ico-png"
    if ico_png_dir.exists():
        shutil.rmtree(ico_png_dir)
    ico_png_dir.mkdir(parents=True)

    ico_pngs: list[Path] = []
    for size in ICON_SIZES:
        target = ico_png_dir / f"icon_{size}.png"
        resize_png(archived_source, target, size, rounded=False)
        ico_pngs.append(target)

    write_png_ico(ico_pngs, output_dir / "LogClinic.ico")
    shutil.rmtree(ico_png_dir)


def main() -> int:
    """解析命令行参数并生成图标资源。"""

    parser = argparse.ArgumentParser(description="生成 LogClinic 应用图标资源")
    parser.add_argument("source", type=Path, help="源 PNG 图片路径")
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path("assets/icons"),
        help="图标输出目录，默认 assets/icons",
    )
    args = parser.parse_args()

    try:
        generate_icons(args.source.expanduser().resolve(), args.output_dir.resolve())
    except RuntimeError as error:
        print(str(error), file=sys.stderr)
        return 1

    print(f"应用图标已生成到：{args.output_dir.resolve()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
