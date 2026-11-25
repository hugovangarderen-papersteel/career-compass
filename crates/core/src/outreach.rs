//! Outreach (Phase 4): the deterministic checks on every draft, and the pipeline's clock.
//! The model writes; this code decides whether what it wrote may go out.

use chrono::{Datelike, Duration, NaiveDate, Weekday};

pub const EMAIL_MAX_WORDS: usize = 150;
pub const LINKEDIN_MAX_CHARS: usize = 300;
pub const FOLLOW_UP_WORKING_DAYS: u32 = 10;

/// Words that may be capitalised without a source: days, months, the AI line, greetings.
const FREE: &[&str] = &[
    "ai",
    "i",
    "linkedin",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
    "maandag",
    "dinsdag",
    "woensdag",
    "donderdag",
    "vrijdag",
    "januari",
    "februari",
    "maart",
    "mei",
    "juni",
    "juli",
    "augustus",
    "oktober",
    "dear",
    "beste",
    "hello",
    "hi",
    "hallo",
    "best",
    "regards",
    "kind",
    "groet",
    "groeten",
    "met",
    "vriendelijke",
    "mr",
    "ms",
    "mrs",
    "dr",
    "heer",
    "mevrouw",
];

fn words(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '-'))
        .filter(|w| !w.is_empty())
}

/// What the email says that the sources do not: every number, and every capitalised word
/// that does not start a sentence (a name, a product, a place), must appear in the one chosen
/// company fact, the company name, or her own approved profile and times. An empty list means
/// the check passes. It also insists on the AI line and the word limit.
pub fn claim_problems(email: &str, sourced: &str, hers: &str) -> Vec<String> {
    let allowed: std::collections::HashSet<String> =
        words(&format!("{sourced} {hers}")).map(str::to_lowercase).collect();
    let mut out = Vec::new();
    for sentence in email.split(['.', '!', '?', '\n', ':']) {
        for (i, w) in words(sentence).enumerate() {
            let low = w.to_lowercase();
            let number = w.chars().any(|c| c.is_ascii_digit());
            let name = i > 0 && w.len() > 1 && w.chars().next().is_some_and(char::is_uppercase);
            if (number || name) && !allowed.contains(&low) && !FREE.contains(&low.as_str()) {
                let p = format!("\"{w}\" is not in the chosen source");
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    if !words(email).any(|w| w == "AI") {
        out.push("the line saying you used an AI assistant is missing".into());
    }
    let n = words(email).count();
    if n >= EMAIL_MAX_WORDS {
        out.push(format!("{n} words; keep it under {EMAIL_MAX_WORDS}"));
    }
    out
}

/// A LinkedIn connection note: cut at a word boundary to at most 300 characters.
pub fn linkedin_note(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= LINKEDIN_MAX_CHARS {
        return s.to_string();
    }
    let cut: String = s.chars().take(LINKEDIN_MAX_CHARS).collect();
    match cut.rfind(' ') {
        Some(i) => cut[..i].trim_end().to_string(),
        None => cut,
    }
}

/// `n` working days (Monday to Friday; public holidays not counted) after `from`.
pub fn add_working_days(from: NaiveDate, n: u32) -> NaiveDate {
    let mut d = from;
    let mut left = n;
    while left > 0 {
        d += Duration::days(1);
        if !matches!(d.weekday(), Weekday::Sat | Weekday::Sun) {
            left -= 1;
        }
    }
    d
}

#[derive(Debug, PartialEq, Eq)]
pub enum Due {
    Wait,
    /// No reply after 10 working days: suggest one polite follow-up with a draft.
    FollowUp,
    /// No reply to the follow-up either: park it, no more nudges.
    Park,
}

/// What a sent item needs today, from the last send and how many follow-ups went out.
pub fn due(last_sent: NaiveDate, follow_ups_sent: i32, today: NaiveDate) -> Due {
    if today < add_working_days(last_sent, FOLLOW_UP_WORKING_DAYS) {
        Due::Wait
    } else if follow_ups_sent == 0 {
        Due::FollowUp
    } else {
        Due::Park
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FACT: &str = "funding 2026-10-02 Raised a Series A of 12 million to roll out Remote Care";
    const HERS: &str = "I led a team of twelve people through a merger. Tuesday 10:00 or Thursday 14:30";
    const GOOD: &str = "Dear Ms Doe,\n\nTo be upfront: I used an AI assistant to find health tech companies \
        whose goals match my experience, and Testco came out near the top. You recently raised a Series A \
        to roll out Remote Care. I led a team of twelve people through a merger without anyone leaving. \
        If my background looks interesting, a short call is the best way to find out whether there is a \
        fit. Would Tuesday 10:00 or Thursday 14:30 suit you?";

    #[test]
    fn claim_check_passes_sourced_and_flags_invented() {
        let sourced = format!("{FACT} Testco Doe");
        assert_eq!(claim_problems(GOOD, &sourced, HERS), Vec::<String>::new());
        let bad = GOOD.replace("Remote Care", "Remote Care in 200 Hospitals across Germany");
        let p = claim_problems(&bad, &sourced, HERS);
        assert_eq!(p.len(), 3, "{p:?}");
        assert!(p[0].contains("200") && p[1].contains("Hospitals") && p[2].contains("Germany"));
        let no_ai = GOOD.replace("an AI assistant", "a tool");
        assert!(claim_problems(&no_ai, &sourced, HERS).iter().any(|p| p.contains("AI")));
        let long = format!("{GOOD} {}", "very ".repeat(100));
        assert!(claim_problems(&long, &sourced, HERS)
            .iter()
            .any(|p| p.contains("under 150")));
    }

    #[test]
    fn linkedin_note_is_at_most_300_chars() {
        let short = "Hello, I read about your Series A and would like to connect.";
        assert_eq!(linkedin_note(short), short);
        let long = "word ".repeat(100);
        let n = linkedin_note(&long);
        assert!(n.chars().count() <= LINKEDIN_MAX_CHARS);
        assert!(n.ends_with("word"));
        let accents = "é".repeat(400);
        assert_eq!(linkedin_note(&accents).chars().count(), 300);
    }

    #[test]
    fn working_days_skip_weekends() {
        let wed = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap();
        assert_eq!(add_working_days(wed, 10), NaiveDate::from_ymd_opt(2026, 12, 9).unwrap());
        let fri = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
        assert_eq!(add_working_days(fri, 1), NaiveDate::from_ymd_opt(2026, 11, 30).unwrap());
        let sat = NaiveDate::from_ymd_opt(2026, 11, 28).unwrap();
        assert_eq!(add_working_days(sat, 1), NaiveDate::from_ymd_opt(2026, 11, 30).unwrap());
    }

    #[test]
    fn parked_after_the_second_no_reply() {
        let sent = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap();
        let day9 = add_working_days(sent, 9);
        let day10 = add_working_days(sent, 10);
        assert_eq!(due(sent, 0, day9), Due::Wait);
        assert_eq!(due(sent, 0, day10), Due::FollowUp);
        assert_eq!(due(sent, 1, day9), Due::Wait);
        assert_eq!(due(sent, 1, day10), Due::Park);
    }
}
