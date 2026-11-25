//! The company base (spec §3, Phase 2): why-now signals, derived by code from dated events,
//! and their strength, a fixed recency decay. Plus the seed CSV reader.

use chrono::NaiveDate;

/// The why-now signal kinds (plan Phase 2).
pub const SIGNAL_KINDS: [&str; 7] = [
    "funding",
    "regulatory",
    "big_contract",
    "new_leader",
    "new_country",
    "ops_roles",
    "headcount_growth",
];

/// Half-life of a signal's strength, and the age after which it counts for nothing.
pub const HALF_LIFE_DAYS: f64 = 180.0;
pub const MAX_AGE_DAYS: i64 = 730;

/// Growth above this (per year) is a signal.
pub const GROWTH_SIGNAL: f64 = 0.20;

/// How much a fresh signal of this kind says about "they need someone now".
pub fn weight(kind: &str) -> f64 {
    match kind {
        "funding" => 1.0,
        "regulatory" => 0.9,
        "big_contract" | "new_country" | "ops_roles" => 0.8,
        "new_leader" | "headcount_growth" => 0.7,
        _ => 0.0,
    }
}

/// Strength 0–1: the kind's weight halved every 180 days, 0 after two years.
/// A date in the future counts as today.
pub fn strength(kind: &str, dated: NaiveDate, today: NaiveDate) -> f64 {
    let age = (today - dated).num_days().max(0);
    if age > MAX_AGE_DAYS {
        return 0.0;
    }
    weight(kind) * 0.5f64.powf(age as f64 / HALF_LIFE_DAYS)
}

/// A page date as precise as it was stated: `YYYY` reads as 1 July, `YYYY-MM` as the 15th.
pub fn parse_date(s: &str) -> Option<NaiveDate> {
    let p: Vec<&str> = s.split('-').collect();
    let num = |i: usize| p.get(i).and_then(|x| x.parse::<u32>().ok());
    let y = p.first().filter(|y| y.len() == 4)?.parse().ok()?;
    match p.len() {
        1 => NaiveDate::from_ymd_opt(y, 7, 1),
        2 => NaiveDate::from_ymd_opt(y, num(1)?, 15),
        3 => NaiveDate::from_ymd_opt(y, num(1)?, num(2)?),
        _ => None,
    }
}

/// The signal an extracted event stands for, if any. Leadership counts only for a new CEO/COO.
pub fn event_signal(kind: &str, title: &str) -> Option<&'static str> {
    match kind {
        "funding" => Some("funding"),
        "regulatory" => Some("regulatory"),
        "contract" => Some("big_contract"),
        "new_country" => Some("new_country"),
        "leadership" => {
            let t = format!(
                " {} ",
                title.to_lowercase().replace(|c: char| !c.is_alphanumeric(), " ")
            );
            [" ceo ", " coo ", "chief executive", "chief operating"]
                .iter()
                .any(|k| t.contains(k))
                .then_some("new_leader")
        }
        _ => None,
    }
}

/// Role words on a careers page that say "they are building operations".
const OPS_ROLES: [&str; 7] = [
    "operations manager",
    "head of operations",
    "implementation",
    "programme manager",
    "program manager",
    "project manager",
    "projectmanager",
];

/// Role words on the careers page now that were not there before (`old` empty on first fetch).
pub fn new_ops_roles(old: &str, new: &str) -> Vec<&'static str> {
    let (old, new) = (old.to_lowercase(), new.to_lowercase());
    OPS_ROLES
        .iter()
        .copied()
        .filter(|k| new.contains(k) && !old.contains(k))
        .collect()
}

/// Yearly headcount growth from the stated history (date, people), oldest vs newest,
/// when they lie at least 180 days apart.
pub fn headcount_growth(history: &[(NaiveDate, f64)]) -> Option<f64> {
    let first = history.iter().min_by_key(|h| h.0)?;
    let last = history.iter().max_by_key(|h| h.0)?;
    let days = (last.0 - first.0).num_days();
    if days < 180 || first.1 <= 0.0 {
        return None;
    }
    Some((last.1 / first.1).powf(365.0 / days as f64) - 1.0)
}

/// The leading number in a stated headcount ("about 1,200 people", "120+").
pub fn headcount_number(s: &str) -> Option<f64> {
    let d: String = s
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .filter(|c| c.is_ascii_digit())
        .collect();
    d.parse().ok()
}

/// A company's identity on the web: `https://host/` (scheme kept, path dropped, lowercase).
/// None for anything that is not a plain http(s) site, and for LinkedIn, which we never fetch.
pub fn site_root(url: &str) -> Option<String> {
    let u = url.trim();
    let (scheme, rest) = match u.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => ("https".to_string(), u),
    };
    let host = rest.split(['/', '?', '#']).next()?.to_ascii_lowercase();
    let bare = host.trim_start_matches("www.");
    let ok = matches!(scheme.as_str(), "http" | "https")
        && bare.contains('.')
        && !host.contains(['@', ' ', '\\'])
        && bare != "linkedin.com"
        && !bare.ends_with(".linkedin.com");
    ok.then(|| format!("{scheme}://{host}/"))
}

/// RFC 4180 CSV: quoted fields may hold commas, newlines and doubled quotes.
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let (mut rows, mut row, mut field) = (Vec::new(), Vec::new(), String::new());
    let (mut quoted, mut chars) = (false, text.chars().peekable());
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            (true, '"') => quoted = false,
            (true, c) => field.push(c),
            (false, '"') => quoted = true,
            (false, ',') => row.push(std::mem::take(&mut field)),
            (false, '\r') => {}
            (false, '\n') => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            (false, c) => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows.retain(|r| r.iter().any(|f| !f.trim().is_empty()));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn d(s: &str) -> NaiveDate {
        parse_date(s).unwrap()
    }

    #[test]
    fn strength_halves_every_180_days_and_stops_at_two_years() {
        let today = d("2026-11-25");
        assert_eq!(strength("funding", today, today), 1.0);
        let half = strength("funding", today - Duration::days(180), today);
        assert!((half - 0.5).abs() < 1e-9);
        let quarter = strength("regulatory", today - Duration::days(360), today);
        assert!((quarter - 0.9 * 0.25).abs() < 1e-9);
        assert_eq!(strength("funding", today - Duration::days(731), today), 0.0);
        assert_eq!(strength("funding", d("2027-03"), today), 1.0);
        assert_eq!(strength("gossip", today, today), 0.0);
        for k in SIGNAL_KINDS {
            assert!(weight(k) > 0.0, "{k}");
        }
    }

    #[test]
    fn dates_and_event_signals() {
        assert_eq!(d("2025"), NaiveDate::from_ymd_opt(2025, 7, 1).unwrap());
        assert_eq!(d("2025-03"), NaiveDate::from_ymd_opt(2025, 3, 15).unwrap());
        assert!(parse_date("25-03").is_none() && parse_date("2025-13").is_none());
        assert_eq!(event_signal("leadership", "Jan Smit joins as CEO"), Some("new_leader"));
        assert_eq!(event_signal("leadership", "New head of procurement"), None);
        assert_eq!(event_signal("contract", "Five-year deal"), Some("big_contract"));
        assert_eq!(event_signal("award", "Best place to work"), None);
    }

    #[test]
    fn careers_diff_and_growth() {
        assert_eq!(
            new_ops_roles("", "We hire a Programme Manager"),
            vec!["programme manager"]
        );
        assert!(new_ops_roles("programme manager", "Programme manager wanted").is_empty());
        let h = [(d("2024-01-01"), 50.0), (d("2025-01-01"), 65.0)];
        assert!((headcount_growth(&h).unwrap() - 0.3).abs() < 0.01);
        assert!(headcount_growth(&h[..1]).is_none());
        assert_eq!(headcount_number("about 1,200 people"), Some(1200.0));
    }

    #[test]
    fn site_roots() {
        assert_eq!(
            site_root(" https://WWW.Example.nl/en/about?x=1").as_deref(),
            Some("https://www.example.nl/")
        );
        assert_eq!(site_root("example.com").as_deref(), Some("https://example.com/"));
        assert!(site_root("ftp://example.com").is_none());
        assert!(site_root("https://nl.linkedin.com/company/x").is_none());
        assert!(site_root("localhost").is_none());
    }

    #[test]
    fn csv_with_quotes_commas_and_newlines() {
        let rows = parse_csv("a,b,c\n\"x, y\",\"say \"\"hi\"\"\",\"two\nlines\"\r\n\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1], vec!["x, y", "say \"hi\"", "two\nlines"]);
    }
}
