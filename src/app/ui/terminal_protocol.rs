// 本地终端和远程终端共享的输入协议工具。
//
// 业务意图：
// - “终端”页的本地 PTY 和“连接”页的 SSH 终端都使用自绘终端，键盘控制序列与
//   xterm 鼠标上报必须保持一致。
// - 这些协议映射集中维护，避免后续修复中文输入、快捷键或鼠标坐标时只改到其中一个入口。
//
// 边界条件：
// - 普通可打印字符不在 keydown 阶段发送，由 GPUI 输入提交路径负责，避免 IME 组合文本和
//   ASCII keydown 双路径造成重复输入。
// - alacritty 内部坐标是 0 基，xterm SGR 鼠标协议是 1 基，坐标转换必须在这里统一完成。

use alacritty_terminal::index::Point;

use super::*;

/// 将 GPUI 按键事件映射为终端控制字节。
///
/// 业务意图：
/// - 自绘终端没有系统文本框托管光标和控制键，方向键、回车、退格、Tab、Esc 和 Ctrl 组合
///   需要显式转换为 shell 或 TUI 程序能理解的字节序列。
/// - 同一函数同时服务 SSH channel 与本地 PTY，确保用户在两个入口中的按键行为一致。
///
/// 边界条件：
/// - `Ctrl+C` 在没有选区时映射为 ETX；有选区时上层复制逻辑会先消费，不会走到这里。
/// - 中文 IME 和普通文本输入由 `EntityInputHandler` 提交，本函数只处理控制键和不可打印序列。
pub(in crate::app) fn terminal_input_bytes_for_keystroke(keystroke: &Keystroke) -> Option<Vec<u8>> {
    if keystroke.modifiers.control && keystroke.key.len() == 1 {
        let byte = keystroke.key.as_bytes()[0].to_ascii_lowercase();
        if byte.is_ascii_lowercase() {
            return Some(vec![byte - b'a' + 1]);
        }
    }

    let bytes = match keystroke.key.as_str() {
        "enter" => b"\r".to_vec(),
        "backspace" => vec![0x7f],
        "tab" => b"\t".to_vec(),
        "escape" => vec![0x1b],
        "up" => b"\x1b[A".to_vec(),
        "down" => b"\x1b[B".to_vec(),
        "right" => b"\x1b[C".to_vec(),
        "left" => b"\x1b[D".to_vec(),
        "home" => b"\x1b[H".to_vec(),
        "end" => b"\x1b[F".to_vec(),
        "pageup" => b"\x1b[5~".to_vec(),
        "pagedown" => b"\x1b[6~".to_vec(),
        _ => return None,
    };
    Some(bytes)
}

/// 生成 xterm SGR 鼠标上报序列。
///
/// 业务意图：
/// - 终端内的 vim、less、tmux 等程序可能开启 xterm 鼠标模式；此时鼠标按下、拖拽和释放
///   需要发送给后端，而不是只用于前端选区。
///
/// 协议约束：
/// - `Point` 来自 alacritty grid，行列均为 0 基；xterm SGR 使用 1 基坐标，因此发送前统一加一。
/// - 第一版调用方只上报左键按下、左键拖拽和释放；滚轮和其它按钮后续可复用该函数扩展。
pub(in crate::app) fn terminal_sgr_mouse_report(
    button_code: u8,
    point: Point,
    pressed: bool,
) -> Vec<u8> {
    let suffix = if pressed { 'M' } else { 'm' };
    format!(
        "\x1b[<{};{};{}{}",
        button_code,
        point.column.0 + 1,
        point.line.0 + 1,
        suffix
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::index::{Column, Line, Point};

    use super::*;

    /// 验证终端鼠标上报坐标遵循 xterm SGR 的 1 基坐标规则。
    ///
    /// 业务风险：
    /// - 坐标如果仍按 alacritty 内部 0 基格式发送，vim、less 等远端 TUI 会把点击位置错一行一列。
    #[test]
    fn 终端鼠标上报使用_sgr_一基坐标() {
        let bytes = terminal_sgr_mouse_report(0, Point::new(Line(2), Column(4)), true);
        assert_eq!(
            String::from_utf8(bytes).expect("测试鼠标序列应为 UTF-8"),
            "\x1b[<0;5;3M"
        );
    }

    /// 验证常用控制键映射保持终端兼容。
    ///
    /// 业务风险：
    /// - 如果回车、退格或方向键映射漂移，本地终端和 SSH 终端都会出现 shell 输入异常。
    #[test]
    fn 终端控制键映射为稳定字节序列() {
        let enter = Keystroke::parse("enter").expect("测试按键应能解析");
        let backspace = Keystroke::parse("backspace").expect("测试按键应能解析");
        let left = Keystroke::parse("left").expect("测试按键应能解析");

        assert_eq!(
            terminal_input_bytes_for_keystroke(&enter),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            terminal_input_bytes_for_keystroke(&backspace),
            Some(vec![0x7f])
        );
        assert_eq!(
            terminal_input_bytes_for_keystroke(&left),
            Some(b"\x1b[D".to_vec())
        );
    }
}
