//! Calendar time to UNIX time, for the "date at boot" response.

/// Seconds since 1970-01-01 00:00:00 UTC. `tz_minutes` is the firmware's
/// offset of local time from UTC (EFI_TIME.TimeZone: local = UTC - offset;
/// 2047 means unspecified, treated as UTC).
pub fn unix_time(
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    tz_minutes: i16,
) -> Option<i64> {
    if !(1970..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let leap = |y: i64| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let mdays = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let y = year as i64;
    let max_day = mdays[month as usize - 1] + if month == 2 && leap(y) { 1 } else { 0 };
    if day == 0 || day as i64 > max_day {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm, public domain).
    let (yy, m) = if month <= 2 {
        (y - 1, month as i64 + 9)
    } else {
        (y, month as i64 - 3)
    };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * m + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let mut t = days * 86_400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64;
    if tz_minutes != 2047 && (-1440..=1440).contains(&tz_minutes) {
        // UEFI: Localtime = UTC - TimeZone, so UTC = Localtime + TimeZone.
        t += tz_minutes as i64 * 60;
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(unix_time(1970, 1, 1, 0, 0, 0, 2047), Some(0));
        assert_eq!(unix_time(2000, 2, 29, 12, 0, 0, 0), Some(951_825_600));
        assert_eq!(unix_time(2026, 11, 21, 10, 0, 0, 2047), Some(1_795_255_200));
        assert_eq!(unix_time(2026, 11, 21, 18, 0, 0, -480), Some(1_795_255_200));
        assert_eq!(unix_time(2001, 2, 29, 0, 0, 0, 0), None);
        assert_eq!(unix_time(2001, 13, 1, 0, 0, 0, 0), None);
    }
}
