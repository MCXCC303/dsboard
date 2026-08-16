//! 日期工具
//!
//! 平台 CSV 日期为 RFC3339(`2026-07-13T00:00:00+08:00`)

/// Unix 秒(UTC)。WASI 提供 wall clock。
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 格里高利历 (y, m, d) → 自 1970-01-01 的天数(Howard Hinnant 算法)。
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = ((m + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// 自 1970-01-01 的天数 → (y, m, d)。
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 本地时区今天的 `YYYY-MM-DD`(offset_min = 宿主时区相对 UTC 分钟数)。
pub fn today_local(offset_min: i32) -> String {
    let days = (unix_now() + offset_min as i64 * 60).div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// 解析平台导出的 RFC3339 时间(`YYYY-MM-DDTHH:MM:SS±HH:MM` 或 `Z`),
/// 换算到本地时区(offset_min)后返回 `YYYY-MM-DD`
pub fn local_date_from_rfc3339(s: &str, offset_min: i32) -> Option<String> {
    let b = s.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let digit = |i: usize| -> Option<i64> {
        let c = *b.get(i)?;
        c.is_ascii_digit().then(|| (c - b'0') as i64)
    };
    let num = |start: usize, len: usize| -> Option<i64> {
        let mut v = 0;
        for i in start..start + len {
            v = v * 10 + digit(i)?;
        }
        Some(v)
    };

    let y = num(0, 4)?;
    let m = num(5, 2)?;
    let d = num(8, 2)?;
    let hh = num(11, 2)?;
    let mm = num(14, 2)?;
    let ss = num(17, 2)?;

    // 偏移:位置 19 为 'Z'/'+'/'-';'+08:00' 形态
    let iso_offset_min = match b.get(19) {
        Some(b'Z') => 0,
        Some(b'+') | Some(b'-') => {
            let oh = num(20, 2)?;
            let om = num(23, 2)?;
            let abs = oh * 60 + om;
            if b[19] == b'-' { -abs } else { abs }
        }
        _ => return None,
    };

    let epoch = days_from_civil(y, m as u32, d as u32) * 86_400 + hh * 3600 + mm * 60 + ss
        - iso_offset_min * 60;
    let local_days = (epoch + offset_min as i64 * 60).div_euclid(86_400);
    let (ly, lm, ld) = civil_from_days(local_days);
    Some(format!("{ly:04}-{lm:02}-{ld:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for (y, m, d) in [(1970, 1, 1), (2026, 7, 13), (2026, 8, 15), (2000, 2, 29)] {
            let z = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(z), (y, m, d));
        }
    }

    #[test]
    fn rfc3339_plus8_to_local() {
        // +08:00 的 2026-07-14T00:00 = UTC 2026-07-13T16:00;
        // 采集端同为 +08:00 → 2026-07-14
        let d = local_date_from_rfc3339("2026-07-14T00:00:00+08:00", 480).unwrap();
        assert_eq!(d, "2026-07-14");
        // UTC+0 采集端 → 2026-07-13
        let d = local_date_from_rfc3339("2026-07-14T00:00:00+08:00", 0).unwrap();
        assert_eq!(d, "2026-07-13");
        // Z 后缀
        let d = local_date_from_rfc3339("2026-07-14T00:00:00Z", 0).unwrap();
        assert_eq!(d, "2026-07-14");
    }
}
