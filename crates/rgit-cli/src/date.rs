//! git's date parser (date.c): `parse_date_basic` for full dates, else
//! `approxidate` for forms like `yesterday 5pm`, `last friday`,
//! `3.days.ago` or `Jan 5`.

use crate::pretty::{days_from_civil, local_parts, local_time};

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS: [&str; 7] = [
    "Sundays",
    "Mondays",
    "Tuesdays",
    "Wednesdays",
    "Thursdays",
    "Fridays",
    "Saturdays",
];
const NUMBERS: [&str; 11] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
];
const ZONES: [(&str, i64, i64); 44] = [
    ("IDLW", -12, 0),
    ("NT", -11, 0),
    ("CAT", -10, 0),
    ("HST", -10, 0),
    ("HDT", -10, 1),
    ("YST", -9, 0),
    ("YDT", -9, 1),
    ("PST", -8, 0),
    ("PDT", -8, 1),
    ("MST", -7, 0),
    ("MDT", -7, 1),
    ("CST", -6, 0),
    ("CDT", -6, 1),
    ("EST", -5, 0),
    ("EDT", -5, 1),
    ("AST", -3, 0),
    ("ADT", -3, 1),
    ("WAT", -1, 0),
    ("GMT", 0, 0),
    ("UTC", 0, 0),
    ("Z", 0, 0),
    ("WET", 0, 0),
    ("BST", 0, 1),
    ("CET", 1, 0),
    ("MET", 1, 0),
    ("MEWT", 1, 0),
    ("MEST", 1, 1),
    ("CEST", 1, 1),
    ("MESZ", 1, 1),
    ("FWT", 1, 0),
    ("FST", 1, 1),
    ("EET", 2, 0),
    ("EEST", 2, 1),
    ("WAST", 7, 0),
    ("WADT", 7, 1),
    ("CCT", 8, 0),
    ("JST", 9, 0),
    ("EAST", 10, 0),
    ("EADT", 10, 1),
    ("GST", 10, 0),
    ("NZT", 12, 0),
    ("NZST", 12, 0),
    ("NZDT", 12, 1),
    ("IDLE", 12, 0),
];

/// C's `struct tm`: years since 1900, months from 0, -1 for unset.
#[derive(Clone, Copy)]
struct Tm {
    year: i64,
    mon: i64,
    mday: i64,
    hour: i64,
    min: i64,
    sec: i64,
    wday: i64,
    /// tm_isdst, which git's mktime calls carry over from `now`: -1 unknown.
    isdst: i32,
}

fn localtime(t: i64) -> Tm {
    let (y, m, d, hour, min, sec) = local_parts(t);
    Tm {
        year: y - 1900,
        mon: m - 1,
        mday: d,
        hour,
        min,
        sec,
        wday: (days_from_civil(y, m, d) + 4).rem_euclid(7),
        isdst: isdst(t),
    }
}

#[cfg(unix)]
fn isdst(t: i64) -> i32 {
    // SAFETY: localtime_r only writes the tm we own.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = t as libc::time_t;
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return -1;
    }
    tm.tm_isdst
}

#[cfg(not(unix))]
fn isdst(_: i64) -> i32 {
    -1
}

fn gmtime(t: i64) -> Tm {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let mut tm = localtime(0);
    let (mut lo, mut hi) = (-1_000_000i64, 1_000_000i64);
    // days_from_civil is monotonic in the year; find the year, then the month.
    while lo < hi {
        let mid = (lo + hi + 1).div_euclid(2);
        if days_from_civil(mid, 1, 1) <= days {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut mon = 12;
    while days_from_civil(lo, mon, 1) > days {
        mon -= 1;
    }
    tm.year = lo - 1900;
    tm.mon = mon - 1;
    tm.mday = days - days_from_civil(lo, mon, 1) + 1;
    tm.hour = secs / 3600;
    tm.min = secs / 60 % 60;
    tm.sec = secs % 60;
    tm.wday = (days + 4).rem_euclid(7);
    tm
}

fn mktime(tm: &Tm) -> i64 {
    #[cfg(unix)]
    if tm.isdst >= 0 {
        // SAFETY: mktime only reads and normalizes the tm we own.
        let mut c: libc::tm = unsafe { std::mem::zeroed() };
        c.tm_year = tm.year as i32;
        c.tm_mon = tm.mon as i32;
        c.tm_mday = tm.mday as i32;
        c.tm_hour = tm.hour as i32;
        c.tm_min = tm.min as i32;
        c.tm_sec = tm.sec as i32;
        c.tm_isdst = tm.isdst;
        let t = unsafe { libc::mktime(&mut c) };
        if t != -1 {
            return t as i64;
        }
    }
    local_time(tm.year + 1900, tm.mon + 1, tm.mday, tm.hour, tm.min, tm.sec)
}

/// git's match_string: how many leading characters of `date` match `s`
/// case-insensitively, or 0 when an alphanumeric one differs.
fn match_string(date: &[u8], s: &str) -> usize {
    let s = s.as_bytes();
    let mut i = 0;
    while i < date.len() {
        let (d, c) = (date[i], s.get(i).copied().unwrap_or(0));
        if d.eq_ignore_ascii_case(&c) {
            i += 1;
            continue;
        }
        if !d.is_ascii_alphanumeric() {
            break;
        }
        return 0;
    }
    i
}

/// strtol: the number at the start of `s` and the digits it used.
fn number(s: &[u8]) -> (i64, usize) {
    let n = s.iter().take_while(|b| b.is_ascii_digit()).count();
    let v = std::str::from_utf8(&s[..n])
        .ok()
        .and_then(|d| d.parse().ok())
        .unwrap_or(i64::MAX);
    (v, n)
}

fn tm_to_time_t(tm: &Tm) -> i64 {
    const MDAYS: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let year = tm.year - 70;
    let month = tm.mon;
    let mut day = tm.mday;
    if !(0..=129).contains(&year) || !(0..=11).contains(&month) {
        return -1;
    }
    if month < 2 || (year + 2) % 4 != 0 {
        day -= 1;
    }
    if tm.hour < 0 || tm.min < 0 || tm.sec < 0 {
        return -1;
    }
    (year * 365 + (year + 1) / 4 + MDAYS[month as usize] + day) * 86400
        + tm.hour * 3600
        + tm.min * 60
        + tm.sec
}

fn set_date(year: i64, month: i64, day: i64, now: Option<i64>, tm: &mut Tm) -> bool {
    if !(1..13).contains(&month) || !(1..32).contains(&day) {
        return false;
    }
    let mut r = *tm;
    r.mon = month - 1;
    r.mday = day;
    if year == -1 {
        let Some(now) = now else {
            return false;
        };
        r.year = gmtime(now).year;
    } else if (1970..2100).contains(&year) {
        r.year = year - 1900;
    } else if year > 70 && year < 100 {
        r.year = year;
    } else if year < 38 {
        r.year = year + 100;
    } else {
        return false;
    }
    if let Some(now) = now {
        let specified = tm_to_time_t(&r);
        if specified != -1 && now + 10 * 24 * 3600 < specified {
            return false;
        }
    }
    tm.mon = r.mon;
    tm.mday = r.mday;
    if year != -1 {
        tm.year = r.year;
    }
    true
}

fn set_time(hour: i64, min: i64, sec: i64, tm: &mut Tm) -> bool {
    if (0..=24).contains(&hour) && (0..60).contains(&min) && (0..=60).contains(&sec) {
        tm.hour = hour;
        tm.min = min;
        tm.sec = sec;
        return true;
    }
    false
}

/// `num[-./:]num[same]num`: a date or a time; the bytes used, or 0.
fn match_multi_number(num: i64, date: &[u8], at: usize, tm: &mut Tm, now: i64) -> usize {
    let c = date[at];
    let (num2, n2) = number(&date[at + 1..]);
    let mut end = at + 1 + n2;
    let mut num3 = -1;
    if date.get(end) == Some(&c) && date.get(end + 1).is_some_and(u8::is_ascii_digit) {
        let (n, used) = number(&date[end + 1..]);
        num3 = n;
        end += 1 + used;
    }
    match c {
        b':' => {
            if num3 < 0 {
                num3 = 0;
            }
            if !set_time(num, num2, num3, tm) {
                return 0;
            }
            let known = tm.year >= 0 && tm.mon >= 0 && tm.mday >= 0;
            if date.get(end) == Some(&b'.')
                && date.get(end + 1).is_some_and(u8::is_ascii_digit)
                && known
            {
                end += 1 + number(&date[end + 1..]).1;
            }
        }
        _ => {
            let now = Some(now);
            let ok = (num > 70
                && (set_date(num, num2, num3, None, tm) || set_date(num, num3, num2, None, tm)))
                || (c != b'.' && set_date(num3, num, num2, now, tm))
                || set_date(num3, num2, num, now, tm)
                || (c == b'.' && set_date(num3, num, num2, now, tm));
            if !ok {
                return 0;
            }
        }
    }
    end
}

fn nodate(tm: &Tm) -> bool {
    tm.year < 0 && tm.mon < 0 && tm.mday < 0 && tm.hour < 0 && tm.min < 0 && tm.sec < 0
}

fn match_digit(
    date: &[u8],
    tm: &mut Tm,
    offset: &mut Option<i64>,
    gmt: &mut bool,
    now: i64,
) -> usize {
    let (num, n) = number(date);
    if num >= 100_000_000 && nodate(tm) {
        *tm = gmtime(num);
        *gmt = true;
        return n;
    }
    if matches!(date.get(n), Some(b':' | b'.' | b'/' | b'-'))
        && date.get(n + 1).is_some_and(u8::is_ascii_digit)
    {
        let used = match_multi_number(num, date, n, tm, now);
        if used > 0 {
            return used;
        }
    }
    if n == 8 || n == 6 {
        let (a, b, c) = (num / 10000, num % 10000 / 100, num % 100);
        let mut end = n;
        if n == 8 {
            set_date(a, b, c, None, tm);
        } else if set_time(a, b, c, tm)
            && date.get(n) == Some(&b'.')
            && date.get(n + 1).is_some_and(u8::is_ascii_digit)
        {
            end += 1 + number(&date[n + 1..]).1;
        }
        return end;
    }
    if n == 4 {
        if num <= 1400 && offset.is_none() {
            *offset = Some(num / 100 * 60 + num % 100);
        } else if num > 1900 && num < 2100 {
            tm.year = num - 1900;
        }
        return n;
    }
    if n > 2 {
        return n;
    }
    if num > 0 && num < 32 && tm.mday < 0 {
        tm.mday = num;
        return n;
    }
    if n == 2 && tm.year < 0 {
        if num < 10 && tm.mday >= 0 {
            tm.year = num + 100;
            return n;
        }
        if num >= 70 {
            tm.year = num;
            return n;
        }
    }
    if num > 0 && num < 13 && tm.mon < 0 {
        tm.mon = num - 1;
    }
    n
}

fn match_alpha(date: &[u8], tm: &mut Tm, offset: &mut Option<i64>) -> usize {
    for (i, m) in MONTHS.iter().enumerate() {
        let n = match_string(date, m);
        if n >= 3 {
            tm.mon = i as i64;
            return n;
        }
    }
    for (i, d) in WEEKDAYS.iter().enumerate() {
        let n = match_string(date, d);
        if n >= 3 {
            tm.wday = i as i64;
            return n;
        }
    }
    for (name, off, dst) in ZONES {
        let n = match_string(date, name);
        if n >= 3 || n == name.len() {
            offset.get_or_insert(60 * (off + dst));
            return n;
        }
    }
    if match_string(date, "PM") == 2 {
        tm.hour = tm.hour % 12 + 12;
        return 2;
    }
    if match_string(date, "AM") == 2 {
        tm.hour %= 12;
        return 2;
    }
    if date[0] == b'T' && date.get(1).is_some_and(u8::is_ascii_digit) && tm.hour == -1 {
        tm.min = 0;
        tm.sec = 0;
        return 1;
    }
    date.iter().take_while(|b| b.is_ascii_alphabetic()).count()
}

fn match_tz(date: &[u8], offset: &mut Option<i64>) -> usize {
    let (mut hour, n) = number(&date[1..]);
    let mut end = 1 + n;
    let mut min = 0;
    if n == 4 {
        min = hour % 100;
        hour /= 100;
    } else if n != 2 {
        min = 99;
    } else if date.get(end) == Some(&b':') {
        let (m, used) = number(&date[end + 1..]);
        min = m;
        end += 1 + used;
        if end - 1 != 5 {
            min = 99;
        }
    }
    if min < 60 && hour < 24 {
        let off = hour * 60 + min;
        *offset = Some(if date[0] == b'-' { -off } else { off });
    }
    end
}

/// git's parse_date_basic: a fully given date and time, as unix seconds.
fn parse_date_basic(s: &[u8], now: i64) -> Option<i64> {
    let mut tm = Tm {
        year: -1,
        mon: -1,
        mday: -1,
        hour: -1,
        min: -1,
        sec: -1,
        wday: 0,
        isdst: -1,
    };
    let mut offset = None;
    let mut gmt = false;
    if let Some(rest) = s.strip_prefix(b"@") {
        let (stamp, n) = number(rest);
        let tz = &rest[n.min(rest.len())..];
        if n > 0
            && tz.len() == 6
            && tz[0] == b' '
            && matches!(tz[1], b'+' | b'-')
            && tz[2..].iter().all(u8::is_ascii_digit)
        {
            return Some(stamp);
        }
    }
    let mut i = 0;
    while i < s.len() && s[i] != b'\n' {
        let c = s[i];
        let rest = &s[i..];
        let used = if c.is_ascii_alphabetic() {
            match_alpha(rest, &mut tm, &mut offset)
        } else if c.is_ascii_digit() {
            match_digit(rest, &mut tm, &mut offset, &mut gmt, now)
        } else if matches!(c, b'-' | b'+') && rest.get(1).is_some_and(u8::is_ascii_digit) {
            match_tz(rest, &mut offset)
        } else {
            0
        };
        i += used.max(1);
    }
    let t = tm_to_time_t(&tm);
    if t == -1 {
        return None;
    }
    if gmt {
        return Some(t);
    }
    let offset = offset.unwrap_or_else(|| (t - mktime(&tm)) / 60);
    Some(t - offset * 60)
}

fn update_tm(tm: &mut Tm, now: &Tm, mut sec: i64) -> i64 {
    if tm.mday < 0 {
        let offset = tm.mday + 1;
        if sec == 0 && offset < 0 {
            sec = -offset * 86400;
        }
        tm.mday = now.mday;
    }
    if tm.mon < 0 {
        tm.mon = now.mon;
    }
    if tm.year < 0 {
        tm.year = now.year;
        if tm.mon > now.mon {
            tm.year -= 1;
        }
    }
    let n = mktime(tm) - sec;
    *tm = localtime(n);
    n
}

fn pending_number(tm: &mut Tm, num: &mut i64) {
    let n = std::mem::take(num);
    if n == 0 {
        return;
    }
    if tm.mday < 0 && n < 32 {
        tm.mday = n;
    } else if tm.mon < 0 && n < 13 {
        tm.mon = n - 1;
    } else if tm.year < 0 {
        if n > 1969 && n < 2100 {
            tm.year = n - 1900;
        } else if n > 69 && n < 100 {
            tm.year = n;
        } else if n < 38 {
            tm.year = 100 + n;
        }
    }
}

fn date_time(tm: &mut Tm, hour: i64) {
    if tm.mday < 0 && tm.hour < hour {
        tm.mday = -2;
    }
    tm.hour = hour;
    tm.min = 0;
    tm.sec = 0;
}

fn approxidate_alpha(
    date: &[u8],
    tm: &mut Tm,
    now: &Tm,
    num: &mut i64,
    touched: &mut bool,
) -> usize {
    let end = date.iter().take_while(|b| b.is_ascii_alphabetic()).count();
    for (i, m) in MONTHS.iter().enumerate() {
        if match_string(date, m) >= 3 {
            tm.mon = i as i64;
            *touched = true;
            return end;
        }
    }
    let special = |name: &str| match_string(date, name) == name.len();
    let hour = |tm: &mut Tm, num: &mut i64, h| {
        pending_number(tm, num);
        date_time(tm, h);
    };
    let ampm = |tm: &mut Tm, num: &mut i64, pm: i64| {
        let n = std::mem::take(num);
        let mut h = tm.hour;
        if n != 0 {
            h = n;
            tm.min = 0;
            tm.sec = 0;
        }
        tm.hour = h % 12 + pm;
    };
    let matched = if special("yesterday") {
        *num = 0;
        tm.mday = -1;
        update_tm(tm, now, 86400);
        true
    } else if special("noon") {
        hour(tm, num, 12);
        true
    } else if special("midnight") {
        hour(tm, num, 0);
        true
    } else if special("tea") {
        hour(tm, num, 17);
        true
    } else if special("PM") {
        ampm(tm, num, 12);
        true
    } else if special("AM") {
        ampm(tm, num, 0);
        true
    } else if special("never") {
        *tm = localtime(0);
        *num = 0;
        true
    } else if special("now") {
        *num = 0;
        update_tm(tm, now, 0);
        true
    } else {
        false
    };
    if matched {
        *touched = true;
        return end;
    }
    if *num == 0 {
        for (i, name) in NUMBERS.iter().enumerate().skip(1) {
            if match_string(date, name) == name.len() {
                *num = i as i64;
                *touched = true;
                return end;
            }
        }
        if match_string(date, "last") == 4 {
            *num = 1;
            *touched = true;
        }
        return end;
    }
    for (unit, secs) in [
        ("seconds", 1),
        ("minutes", 60),
        ("hours", 3600),
        ("days", 86400),
        ("weeks", 7 * 86400),
    ] {
        if match_string(date, unit) >= unit.len() - 1 {
            update_tm(tm, now, secs * *num);
            *num = 0;
            *touched = true;
            return end;
        }
    }
    for (i, day) in WEEKDAYS.iter().enumerate() {
        if match_string(date, day) >= 3 {
            let mut n = *num - 1;
            *num = 0;
            let mut diff = tm.wday - i as i64;
            if diff <= 0 {
                n += 1;
            }
            diff += 7 * n;
            update_tm(tm, now, diff * 86400);
            *touched = true;
            return end;
        }
    }
    if match_string(date, "months") >= 5 {
        update_tm(tm, now, 0);
        let mut n = tm.mon - *num;
        *num = 0;
        while n < 0 {
            n += 12;
            tm.year -= 1;
        }
        tm.mon = n;
        *touched = true;
        return end;
    }
    if match_string(date, "years") >= 4 {
        update_tm(tm, now, 0);
        tm.year -= *num;
        *num = 0;
        *touched = true;
    }
    end
}

fn approxidate_digit(date: &[u8], tm: &mut Tm, num: &mut i64, now: i64) -> usize {
    let (number, n) = number(date);
    if matches!(date.get(n), Some(b':' | b'.' | b'/' | b'-'))
        && date.get(n + 1).is_some_and(u8::is_ascii_digit)
    {
        let used = match_multi_number(number, date, n, tm, now);
        if used > 0 {
            return used;
        }
    }
    if date[0] != b'0' || n <= 2 {
        *num = number;
    }
    n
}

/// git's approxidate_careful at `now`: unix seconds, or None when nothing in
/// `s` reads as a date.
pub fn approxidate_at(s: &str, now: i64) -> Option<i64> {
    let s = s.as_bytes();
    if let Some(t) = parse_date_basic(s, now) {
        return Some(t);
    }
    let today = localtime(now);
    let mut tm = today;
    tm.year = -1;
    tm.mon = -1;
    tm.mday = -1;
    let (mut num, mut touched) = (0, false);
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c.is_ascii_digit() {
            pending_number(&mut tm, &mut num);
            i += approxidate_digit(&s[i..], &mut tm, &mut num, now);
            touched = true;
        } else if c.is_ascii_alphabetic() {
            i += approxidate_alpha(&s[i..], &mut tm, &today, &mut num, &mut touched);
        } else {
            i += 1;
        }
    }
    pending_number(&mut tm, &mut num);
    touched.then(|| update_tm(&mut tm, &today, 0))
}

#[cfg(test)]
mod tests {
    use super::approxidate_at;
    use crate::pretty::{local_parts, local_time};

    #[test]
    fn dates_parse_like_git() {
        // 2024-01-10, a Wednesday, 12:00 UTC: the same day in most zones.
        let now = 1704888000;
        let (y, m, d, h, mi, s) = local_parts(now);
        let at = |s| approxidate_at(s, now);
        assert_eq!(at("2024-01-03 12:00:00 +0100"), Some(1704279600));
        assert_eq!(at("Wed, 3 Jan 2024 12:00:00 -0000"), Some(1704283200));
        assert_eq!(at("@1704400000"), Some(1704400000));
        assert_eq!(at("2024-01-03"), Some(local_time(2024, 1, 3, h, mi, s)));
        assert_eq!(at("yesterday"), Some(now - 86400));
        assert_eq!(at("3.days.ago"), Some(now - 3 * 86400));
        assert_eq!(at("2 weeks 3 days ago"), Some(now - 17 * 86400));
        assert_eq!(at("yesterday 5pm"), Some(local_time(y, m, d - 1, 17, 0, 0)));
        assert_eq!(at("last friday"), Some(now - 5 * 86400));
        assert_eq!(at("Jan 5"), Some(local_time(2024, 1, 5, h, mi, s)));
        assert_eq!(at("2 months ago"), Some(local_time(2023, 11, d, h, mi, s)));
        assert_eq!(at("never"), Some(0));
        assert_eq!(at("garbage"), None);
    }

    #[test]
    fn noon_keeps_an_explicit_day_before_noon() {
        let early = local_time(2024, 1, 10, 2, 0, 0);
        let noon = |s| approxidate_at(s, early);
        assert_eq!(
            noon("2024-01-07 noon"),
            Some(local_time(2024, 1, 7, 12, 0, 0))
        );
        assert_eq!(noon("noon"), Some(local_time(2024, 1, 9, 12, 0, 0)));
    }
}
