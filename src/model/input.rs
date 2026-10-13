//! What a typed date or number means (spec section 6, Editors).

use time::{Date, Duration, Month, Weekday};

/// `YYYY-MM-DD`, when it names a real day.
pub fn parse_iso_date(text: &str) -> Option<Date> {
    let (y, rest) = text.split_once('-')?;
    let (m, d) = rest.split_once('-')?;
    let month = Month::try_from(m.parse::<u8>().ok()?).ok()?;
    Date::from_calendar_date(y.parse().ok()?, month, d.parse().ok()?).ok()
}

pub fn format_iso_date(date: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

fn weekday(word: &str) -> Option<Weekday> {
    Some(match word {
        "mon" | "monday" => Weekday::Monday,
        "tue" | "tues" | "tuesday" => Weekday::Tuesday,
        "wed" | "wednesday" => Weekday::Wednesday,
        "thu" | "thur" | "thurs" | "thursday" => Weekday::Thursday,
        "fri" | "friday" => Weekday::Friday,
        "sat" | "saturday" => Weekday::Saturday,
        "sun" | "sunday" => Weekday::Sunday,
        _ => return None,
    })
}

/// `+3d`, `-2d`, `+1w`, `+3` (days).
fn offset(text: &str, today: Date) -> Option<Date> {
    let (sign, rest) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    let (number, days) = if let Some(n) = rest.strip_suffix('w') {
        (n, 7)
    } else {
        (rest.strip_suffix('d').unwrap_or(rest), 1)
    };
    let n: i64 = number.parse().ok()?;
    today.checked_add(Duration::days(sign * n * days))
}

/// A typed date: `2026-10-03`, `today`, `tomorrow`, `+3d`, `-2d`, `+1w`, or a weekday name
/// (`fri`, `friday`) meaning the next such day after today. Empty means "clear the field".
pub fn parse_date_input(text: &str, today: Date) -> Result<Option<Date>, String> {
    let t = text.trim().to_lowercase();
    if t.is_empty() {
        return Ok(None);
    }
    let found = match t.as_str() {
        "today" => Some(today),
        "tomorrow" => today.next_day(),
        _ => parse_iso_date(&t)
            .or_else(|| offset(&t, today))
            .or_else(|| {
                let w = weekday(&t)?;
                let mut d = today.next_day()?;
                while d.weekday() != w {
                    d = d.next_day()?;
                }
                Some(d)
            }),
    };
    found.map(Some).ok_or_else(|| {
        format!(
            "`{}` is not a date: try 2026-10-03, +3d or fri",
            text.trim()
        )
    })
}

/// A typed number; empty means "clear the field".
pub fn parse_number_input(text: &str) -> Result<Option<f64>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    match t.parse::<f64>() {
        Ok(n) if n.is_finite() => Ok(Some(n)),
        _ => Err(format!("`{t}` is not a number")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    // 2026-10-12 is a Monday.
    const TODAY: Date = date!(2026 - 10 - 12);

    fn parsed(text: &str) -> Option<String> {
        parse_date_input(text, TODAY).unwrap().map(format_iso_date)
    }

    #[test]
    fn iso_dates_offsets_and_words() {
        assert_eq!(parsed("2026-10-03").as_deref(), Some("2026-10-03"));
        assert_eq!(parsed("+3d").as_deref(), Some("2026-10-15"));
        assert_eq!(parsed("+3").as_deref(), Some("2026-10-15"));
        assert_eq!(parsed("-2d").as_deref(), Some("2026-10-10"));
        assert_eq!(parsed("+1w").as_deref(), Some("2026-10-19"));
        assert_eq!(parsed("today").as_deref(), Some("2026-10-12"));
        assert_eq!(parsed(" Tomorrow ").as_deref(), Some("2026-10-13"));
    }

    #[test]
    fn weekday_names_mean_the_next_such_day() {
        assert_eq!(parsed("fri").as_deref(), Some("2026-10-16"));
        assert_eq!(parsed("Friday").as_deref(), Some("2026-10-16"));
        assert_eq!(parsed("mon").as_deref(), Some("2026-10-19"), "not today");
        assert_eq!(parsed("sun").as_deref(), Some("2026-10-18"));
    }

    #[test]
    fn empty_clears_and_nonsense_explains() {
        assert_eq!(parse_date_input("  ", TODAY), Ok(None));
        let err = parse_date_input("soon", TODAY).unwrap_err();
        assert!(
            err.contains("2026-10-03") && err.contains("+3d") && err.contains("fri"),
            "{err}"
        );
        assert!(parse_date_input("2026-02-30", TODAY).is_err());
        assert!(parse_date_input("+xd", TODAY).is_err());
    }

    #[test]
    fn numbers() {
        assert_eq!(parse_number_input("2.5"), Ok(Some(2.5)));
        assert_eq!(parse_number_input(" -3 "), Ok(Some(-3.0)));
        assert_eq!(parse_number_input(""), Ok(None));
        assert!(parse_number_input("three").is_err());
        assert!(parse_number_input("inf").is_err());
    }
}
