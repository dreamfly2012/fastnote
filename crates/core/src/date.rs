//! 本地日期计算：每日笔记（Daily Note）用。
//!
//! 刻意不引入 `chrono` / `time`——这里只需要「今天是几号」，
//! 用 Howard Hinnant 的 `civil_from_days` 算法（纯整数运算）就够，
//! 代价是几十行代码，换来的是零依赖和可完整单测的确定性。

use std::time::{SystemTime, UNIX_EPOCH};

/// 默认时区偏移：中国标准时间 UTC+8。
pub const DEFAULT_OFFSET_MINUTES: i32 = 8 * 60;

/// 从 Unix 纪元起的天数换算成公历年月日。
///
/// 算法出自 Howard Hinnant 的 `chrono`-compatible 日期算法，
/// 对负数天数（1970 年以前）同样成立。
pub fn civil_from_days(days: i64) -> (i32, u32, u32) {
    // 把纪元挪到 0000-03-01，让闰日落在年末，避免分支
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]，3 月为 0
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = y + if m <= 2 { 1 } else { 0 };
    (year as i32, m, d)
}

/// 把 Unix 秒换算成 `YYYY-MM-DD`。
pub fn date_from_unix(secs: i64) -> String {
    // 用 div_euclid / rem_euclid，保证 1970 年之前也得到正确的"负数天"
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// 按给定 UTC 偏移（分钟）返回当天日期。
pub fn today_with_offset(offset_minutes: i32) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    date_from_unix(secs + offset_minutes as i64 * 60)
}

/// 按默认时区（UTC+8）返回当天日期。
pub fn today() -> String {
    today_with_offset(DEFAULT_OFFSET_MINUTES)
}

/// 每日笔记在笔记库中的相对路径，形如 `daily/2026-09-17.md`。
pub fn daily_note_rel(date: &str) -> String {
    format!("daily/{date}.md")
}

/// `civil_from_days` 的逆运算：公历年月日换算成 Unix 纪元起的天数。
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = ((m + 9) % 12) as i64; // 3 月为 0
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// 星期几（中文单字），输入 `YYYY-MM-DD`。
///
/// 解析失败时返回空串，模板里就变成空字符串 —— 比 panic 合适。
pub fn weekday_cn(date: &str) -> String {
    const NAMES: [&str; 7] = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];
    let days = parse_ymd(date).map(|(y, m, d)| days_from_civil(y, m, d));
    // 1970-01-01 是周四：days % 7 == 0 对应周四，偏移 4 后 0 对应周日
    match days {
        Some(days) => NAMES[((days + 4).rem_euclid(7)) as usize].to_string(),
        None => String::new(),
    }
}

/// 解析 `YYYY-MM-DD`（也接受 `YYYY/MM/DD`）。
pub fn parse_ymd(date: &str) -> Option<(i32, u32, u32)> {
    let mut it = date
        .split(|c| c == '-' || c == '/')
        .map(|s| s.trim().parse::<i64>().ok());
    let (y, m, d) = (it.next()??, it.next()??, it.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y as i32, m as u32, d as u32))
}

/// 按给定 UTC 偏移返回当天的 `HH:MM:SS`。
pub fn time_with_offset(offset_minutes: i32) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let local = (secs + offset_minutes as i64 * 60).rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        local / 3600,
        (local % 3600) / 60,
        local % 60
    )
}

/// Unix 秒 -> `YYYY-MM-DD HH:MM:SS`（默认时区）。
pub fn datetime_from_unix(secs: i64) -> String {
    let local = secs + DEFAULT_OFFSET_MINUTES as i64 * 60;
    let (y, m, d) = civil_from_days(local.div_euclid(86_400));
    let t = local.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        t / 3600,
        (t % 3600) / 60,
        t % 60
    )
}

/// 当前 Unix 秒。
pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_day_zero_is_1970_01_01() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn known_day_numbers_convert_correctly() {
        // 2000-01-01 = 10957 天；2024-01-01 = 19723 天；闰日 2024-02-29 = 19782 天
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        // 1970-01-01 前一天
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn formats_with_zero_padding() {
        assert_eq!(date_from_unix(0), "1970-01-01");
        // 2024-01-01T00:00:00Z = 19723 天
        assert_eq!(date_from_unix(19_723 * 86_400), "2024-01-01");
        // 2026-09-17T00:00:00Z = 20713 天，顺便验证"今天"这条链路
        assert_eq!(date_from_unix(20_713 * 86_400), "2026-09-17");
    }

    #[test]
    fn offset_shifts_across_midnight() {
        // UTC 2024-01-01 23:00 → UTC+8 已经是 1 月 2 日
        let utc_23 = 19_723 * 86_400 + 23 * 3600;
        assert_eq!(date_from_unix(utc_23), "2024-01-01");
        assert_eq!(date_from_unix(utc_23 + 480 * 60), "2024-01-02");
    }

    #[test]
    fn daily_note_path_is_dated() {
        assert_eq!(daily_note_rel("2026-09-17"), "daily/2026-09-17.md");
    }

    #[test]
    fn days_from_civil_inverts_civil_from_days() {
        for days in [-400_000i64, -1, 0, 1, 10_957, 19_723, 19_782, 20_713, 900_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "{y}-{m}-{d} 往返失败");
        }
    }

    #[test]
    fn weekday_names_are_correct() {
        // 1970-01-01 是周四；2026-09-17 也是周四（相差 20713 天，恰好整除 7）
        assert_eq!(weekday_cn("1970-01-01"), "周四");
        assert_eq!(weekday_cn("2026-09-17"), "周四");
        assert_eq!(weekday_cn("2024-02-29"), "周四");
        assert_eq!(weekday_cn("2000-01-01"), "周六");
        // 非法输入不能 panic
        assert_eq!(weekday_cn(""), "");
        assert_eq!(weekday_cn("2026-13-40"), "");
    }

    #[test]
    fn datetime_formats_with_clock() {
        // 1970-01-01 00:00:00 UTC -> +8 时区为当天 08:00
        assert_eq!(datetime_from_unix(0), "1970-01-01 08:00:00");
        // 2026-09-17 06:05:33 UTC -> 14:05:33 本地
        let secs = 20_713 * 86_400 + 6 * 3600 + 5 * 60 + 33;
        assert_eq!(datetime_from_unix(secs), "2026-09-17 14:05:33");
    }

    #[test]
    fn time_with_offset_is_zero_padded() {
        let t = time_with_offset(0);
        assert_eq!(t.len(), 8, "应形如 HH:MM:SS，实际 {t}");
        assert_eq!(t.matches(':').count(), 2);
    }
}
