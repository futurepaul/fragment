//! Cron schedules for triggers: five fields (minute, hour, day of month,
//! month, day of week), in UTC, with Cloudflare's conventions (the old
//! runtime's): day of week is 1 = Sunday … 7 = Saturday, names (`jan`,
//! `mon`) are accepted, and when both day fields are restricted a day
//! matching either one fires.

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
const MINUTE_MS: i64 = 60_000;
const DAY_MS: i64 = 24 * 3600 * 1000;

#[derive(Debug, Clone, PartialEq)]
pub struct Cron {
    minute: u64,
    hour: u64,
    dom: u64,
    month: u64,
    /// Bit 1 = Sunday … bit 7 = Saturday.
    dow: u64,
    dom_any: bool,
    dow_any: bool,
}

fn value(x: &str, names: Option<&[&str]>, first: u32) -> Option<u32> {
    if !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()) {
        return x.parse().ok();
    }
    let lower = x.to_ascii_lowercase();
    names?.iter().position(|n| *n == lower).map(|i| i as u32 + first)
}

fn field(spec: &str, min: u32, max: u32, names: Option<&[&str]>) -> Result<u64, String> {
    if spec.is_empty() {
        return Err("an empty field".into());
    }
    let mut bits = 0u64;
    for part in spec.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().ok().filter(|s| *s >= 1).ok_or_else(|| format!("bad step in {part:?}"))?),
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            if spec.contains(',') {
                return Err("* inside a list".into());
            }
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            let (a, b) = (value(a, names, min), value(b, names, min));
            match (a, b) {
                (Some(a), Some(b)) if a <= b => (a, b),
                (Some(_), Some(_)) => return Err(format!("descending range {range:?}")),
                _ => return Err(format!("bad range {range:?}")),
            }
        } else {
            let v = value(range, names, min).ok_or_else(|| format!("bad value {range:?}"))?;
            (v, if part.contains('/') { max } else { v })
        };
        if lo < min || hi > max {
            return Err(format!("{range:?} is outside {min}-{max}"));
        }
        let mut v = lo;
        while v <= hi {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok(bits)
}

/// Days since 1970-01-01 → (year, month 1-12, day 1-31) (Hinnant's civil_from_days).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Cron, String> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!("a cron schedule has 5 fields (minute hour day month weekday), not {}", fields.len()));
        }
        if fields.iter().any(|f| f.contains(['?', '#'])) {
            return Err("the ? L W # extensions are not supported".into());
        }
        let dow_field = fields[4];
        if dow_field.split([',', '-', '/']).any(|p| p == "0") {
            return Err("day of week 0 is refused: 1 = Sunday … 7 = Saturday".into());
        }
        let cron = Cron {
            minute: field(fields[0], 0, 59, None)?,
            hour: field(fields[1], 0, 23, None)?,
            dom: field(fields[2], 1, 31, None)?,
            month: field(fields[3], 1, 12, Some(&MONTHS))?,
            dow: field(dow_field, 1, 7, Some(&DAYS))?,
            dom_any: fields[2] == "*",
            dow_any: dow_field == "*",
        };
        if cron.next_after(0).is_none() {
            return Err("this schedule never fires".into());
        }
        Ok(cron)
    }

    fn day_matches(&self, days: i64) -> bool {
        let (_, month, dom) = civil(days);
        // 1970-01-01 was a Thursday (5 in 1 = Sunday numbering).
        let dow = (days + 4).rem_euclid(7) as u32 + 1;
        if self.month & (1 << month) == 0 {
            return false;
        }
        let (d, w) = (self.dom & (1 << dom) != 0, self.dow & (1 << dow) != 0);
        if !self.dom_any && !self.dow_any {
            d || w
        } else {
            d && w
        }
    }

    /// The first minute strictly after `after_ms` (Unix ms) that matches,
    /// within about eight years; `None` if there is none.
    pub fn next_after(&self, after_ms: i64) -> Option<i64> {
        let mut t = after_ms.div_euclid(MINUTE_MS) * MINUTE_MS + MINUTE_MS;
        let limit = after_ms + 8 * 366 * DAY_MS;
        while t < limit {
            let days = t.div_euclid(DAY_MS);
            if !self.day_matches(days) {
                t = (days + 1) * DAY_MS;
                continue;
            }
            let minute_of_day = (t - days * DAY_MS) / MINUTE_MS;
            let (hour, minute) = (minute_of_day / 60, minute_of_day % 60);
            if self.hour & (1 << hour) == 0 {
                t = days * DAY_MS + (hour + 1) * 3_600_000;
                continue;
            }
            if self.minute & (1 << minute) != 0 {
                return Some(t);
            }
            t += MINUTE_MS;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-09-23 12:34:56 UTC, a Wednesday
    const NOW: i64 = 1_790_166_896_000;

    fn next(expr: &str, after: i64) -> i64 {
        Cron::parse(expr).unwrap().next_after(after).unwrap()
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(NOW / DAY_MS), (2026, 9, 23));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    #[test]
    fn next_fire() {
        let minute = NOW / MINUTE_MS * MINUTE_MS;
        assert_eq!(next("* * * * *", NOW), minute + MINUTE_MS);
        assert_eq!(next("* * * * *", minute), minute + MINUTE_MS, "strictly after");
        assert_eq!(next("*/15 * * * *", NOW), minute + 11 * MINUTE_MS, "12:45");
        let day = NOW / DAY_MS * DAY_MS;
        assert_eq!(next("0 9 * * *", NOW), day + DAY_MS + 9 * 3_600_000, "tomorrow 09:00");
        assert_eq!(next("30 14 * * *", NOW), day + 14 * 3_600_000 + 30 * MINUTE_MS, "today 14:30");
        // Wednesday = 4; the next Monday (2) is 2026-09-28
        assert_eq!(next("0 0 * * mon", NOW), day + 5 * DAY_MS);
        assert_eq!(next("0 0 * * 2", NOW), day + 5 * DAY_MS);
        assert_eq!(next("0 0 1 jan *", NOW) / DAY_MS, civil_days(2027, 1, 1));
        // both day fields restricted: either matches (the 1st, or a Friday = 6 on 2026-09-25)
        assert_eq!(next("0 0 1 * 6", NOW), day + 2 * DAY_MS);
        assert_eq!(next("0 0 29 2 *", NOW) / DAY_MS, civil_days(2028, 2, 29));
    }

    fn civil_days(y: i64, m: u32, d: u32) -> i64 {
        (0..40_000).find(|&n| civil(n) == (y, m, d)).unwrap()
    }

    #[test]
    fn refusals() {
        for bad in ["* * * *", "* * * * * *", "60 * * * *", "* 24 * * *", "* * 0 * *", "* * * 13 *", "* * * * 0", "* * * * 8", "*/0 * * * *",
            "5-1 * * * *", "* * L * *", "*,5 * * * *", "0 0 31 2 *", "x * * * *"]
        {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
        assert!(Cron::parse("0 9 * JUL MON-FRI").is_ok(), "names in any case");
    }
}
