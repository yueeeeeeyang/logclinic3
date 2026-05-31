// 通用日期时间选择器辅助逻辑。
//
// 业务意图：
// - 插件声明式过滤器需要一种与业务无关的日期时间输入体验，性能列表的开始/结束时间可以复用该通用能力。
// - 本模块复制并裁剪自 xgpui 的 DateTimePicker 纯状态逻辑，保留解析、格式化、日历网格和时间步进这些可测试规则。
// - 渲染和业务副作用由调用页面负责，本模块不读取日志、不理解插件字段含义，也不持久化任何历史记录。
//
// 关键约束：
// - 性能列表现有插件过滤格式是 `yyyy-MM-dd HH:mm:ss`，Rust/chrono 对应格式为 `%Y-%m-%d %H:%M:%S`。
// - 日历按周一作为一周开始，固定生成 6 行 42 个格子，避免月份切换时弹层高度抖动。
// - 手动输入仍允许为空或暂时非法，最终校验继续交给插件业务逻辑；选择器只在用户点击日期/时间时写入规范格式。

use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

/// 应用内日期时间控件的标准格式。
///
/// 边界条件：
/// - 该格式只用于通用选择器写回文本，不限制插件继续用自己的占位文案和解析规则展示错误。
pub(in crate::app) const APP_DATE_TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// 日期时间弹层当前可见月份。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) struct DateTimePickerViewMonth {
    /// 可见年份。
    pub(in crate::app) year: i32,
    /// 可见月份，范围为 1..=12。
    pub(in crate::app) month: u32,
}

/// 可被步进按钮调整的时间部分。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum DateTimePart {
    /// 小时，范围 0..=23。
    Hour,
    /// 分钟，范围 0..=59。
    Minute,
    /// 秒，范围 0..=59。
    Second,
}

/// 解析标准日期时间文本。
///
/// 业务意图：
/// - 用户可能手动输入部分文本，输入框不能因为解析失败就清空；调用方用 `Option` 判断是否能作为选择器当前值。
pub(in crate::app) fn parse_app_date_time(text: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(text.trim(), APP_DATE_TIME_FORMAT).ok()
}

/// 格式化标准日期时间文本。
pub(in crate::app) fn format_app_date_time(value: NaiveDateTime) -> String {
    value.format(APP_DATE_TIME_FORMAT).to_string()
}

/// 返回当前本地日期时间并去掉纳秒。
///
/// 边界条件：
/// - chrono 的本地时间来自系统时钟；这里只用于 UI 默认光标位置，不参与日志排序或插件业务过滤。
pub(in crate::app) fn app_date_time_now() -> NaiveDateTime {
    Local::now()
        .naive_local()
        .with_nanosecond(0)
        .unwrap_or_else(|| NaiveDateTime::new(today(), default_time()))
}

/// 根据输入文本推导弹层初始月份。
pub(in crate::app) fn date_time_picker_month_from_text(text: &str) -> DateTimePickerViewMonth {
    let date = parse_app_date_time(text)
        .map(|value| value.date())
        .unwrap_or_else(today);
    DateTimePickerViewMonth {
        year: date.year(),
        month: date.month(),
    }
}

/// 移动弹层月份。
///
/// 实现原因：
/// - 使用总月份数做 `div_euclid/rem_euclid` 可以统一处理跨年和负数偏移，避免一月向前翻月出错。
pub(in crate::app) fn date_time_picker_move_month(
    month: DateTimePickerViewMonth,
    delta: i32,
) -> DateTimePickerViewMonth {
    let total = month.year * 12 + month.month as i32 - 1 + delta;
    DateTimePickerViewMonth {
        year: total.div_euclid(12),
        month: total.rem_euclid(12) as u32 + 1,
    }
}

/// 返回指定年月的 42 个日历格日期。
///
/// 业务意图：
/// - 与 xgpui DateTimePicker 保持一致，日历从包含当月首日的周一开始，固定 6 周。
pub(in crate::app) fn date_time_picker_calendar_days(year: i32, month: u32) -> Vec<NaiveDate> {
    let month = month.clamp(1, 12);
    let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap_or_else(today);
    let weekday_offset = first.weekday().num_days_from_monday() as i64;
    let start = first - Duration::days(weekday_offset);
    (0..42)
        .map(|offset| start + Duration::days(offset))
        .collect()
}

/// 根据用户选中的日期生成日期时间值。
///
/// 业务意图：
/// - 如果输入框里已经有合法时间，点击新日期时保留时间部分，减少用户在开始/结束时间之间反复调整的成本。
/// - 如果当前文本为空或非法，使用 xgpui 选择器一致的默认时间 `00:00:00`。
pub(in crate::app) fn date_time_picker_value_for_selected_date(
    current_text: &str,
    date: NaiveDate,
) -> NaiveDateTime {
    let time = parse_app_date_time(current_text)
        .map(|value| value.time())
        .unwrap_or_else(default_time);
    NaiveDateTime::new(date, time)
}

/// 调整当前日期时间的某个时间部分。
///
/// 边界条件：
/// - 小时、分钟和秒使用环绕语义，例如 23 点加 1 回到 00 点；这与 xgpui 时间滚轮边界行为一致。
/// - 当前文本非法时以今天和 `00:00:00` 作为可编辑基准，但不会自动应用过滤，用户仍可继续修改。
pub(in crate::app) fn date_time_picker_adjust_time(
    current_text: &str,
    part: DateTimePart,
    delta: i32,
) -> NaiveDateTime {
    let base = parse_app_date_time(current_text)
        .unwrap_or_else(|| NaiveDateTime::new(today(), default_time()));
    let time = base.time();
    let (hour, minute, second) = match part {
        DateTimePart::Hour => (
            wrap_time_component(time.hour(), delta, 24),
            time.minute(),
            time.second(),
        ),
        DateTimePart::Minute => (
            time.hour(),
            wrap_time_component(time.minute(), delta, 60),
            time.second(),
        ),
        DateTimePart::Second => (
            time.hour(),
            time.minute(),
            wrap_time_component(time.second(), delta, 60),
        ),
    };
    let adjusted_time = NaiveTime::from_hms_opt(hour, minute, second).unwrap_or_else(default_time);
    NaiveDateTime::new(base.date(), adjusted_time)
}

/// 返回今天。
fn today() -> NaiveDate {
    Local::now().date_naive()
}

/// 返回默认时间。
fn default_time() -> NaiveTime {
    NaiveTime::from_hms_opt(0, 0, 0).expect("00:00:00 必须是合法时间")
}

/// 环绕调整时间组件。
fn wrap_time_component(value: u32, delta: i32, modulo: u32) -> u32 {
    (value as i32 + delta).rem_euclid(modulo as i32) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造日期。
    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("测试日期必须合法")
    }

    /// 构造日期时间文本。
    fn text(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> String {
        format_app_date_time(NaiveDateTime::new(
            date(year, month, day),
            NaiveTime::from_hms_opt(hour, minute, second).expect("测试时间必须合法"),
        ))
    }

    /// 标准日期时间格式应能解析和格式化性能列表使用的秒级文本。
    #[test]
    fn parses_and_formats_standard_date_time() {
        let value = parse_app_date_time("2026-05-31 09:08:07").expect("应按标准格式解析");
        assert_eq!(format_app_date_time(value), "2026-05-31 09:08:07");
        assert!(parse_app_date_time("2026-05-31").is_none());
    }

    /// 日历网格应固定 42 个格子，并从包含当月一号的周一开始。
    #[test]
    fn calendar_days_start_from_monday_and_keep_fixed_count() {
        let days = date_time_picker_calendar_days(2026, 5);
        assert_eq!(days.len(), 42);
        assert_eq!(days[0], date(2026, 4, 27));
        assert_eq!(days[41], date(2026, 6, 7));
    }

    /// 选中新日期时应保留已有合法时间，避免用户重复选择时分秒。
    #[test]
    fn selected_date_preserves_existing_time() {
        let value = date_time_picker_value_for_selected_date(
            &text(2026, 5, 31, 12, 34, 56),
            date(2026, 6, 1),
        );
        assert_eq!(format_app_date_time(value), "2026-06-01 12:34:56");
    }

    /// 非法输入点击日期时应回落到默认时间。
    #[test]
    fn selected_date_uses_default_time_for_invalid_input() {
        let value = date_time_picker_value_for_selected_date("not-a-date", date(2026, 6, 1));
        assert_eq!(format_app_date_time(value), "2026-06-01 00:00:00");
    }

    /// 时间步进使用环绕语义，覆盖小时和秒的边界。
    #[test]
    fn time_adjustment_wraps_each_time_part() {
        let value =
            date_time_picker_adjust_time(&text(2026, 5, 31, 23, 59, 59), DateTimePart::Hour, 1);
        assert_eq!(format_app_date_time(value), "2026-05-31 00:59:59");

        let value =
            date_time_picker_adjust_time(&text(2026, 5, 31, 23, 59, 0), DateTimePart::Second, -1);
        assert_eq!(format_app_date_time(value), "2026-05-31 23:59:59");
    }

    /// 月份移动应跨年稳定，避免一月向前翻页产生非法月份。
    #[test]
    fn month_movement_crosses_year_boundary() {
        assert_eq!(
            date_time_picker_move_month(
                DateTimePickerViewMonth {
                    year: 2026,
                    month: 1
                },
                -1
            ),
            DateTimePickerViewMonth {
                year: 2025,
                month: 12
            }
        );
        assert_eq!(
            date_time_picker_move_month(
                DateTimePickerViewMonth {
                    year: 2026,
                    month: 12
                },
                1
            ),
            DateTimePickerViewMonth {
                year: 2027,
                month: 1
            }
        );
    }
}
