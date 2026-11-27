//! The profile engine (spec §6, Phase 3): her approved profile becomes a config (weights and
//! hard filters), each company gets a 0–100 fit score with a reason per axis, and the weekly
//! digest picks five. All pure code; the one model judgement (skills, mission) comes in as input.

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::profile::{Profile, Weighted};

/// The seven axes of spec §6, in the order of `DEFAULT_WEIGHTS`.
pub const AXES: [&str; 7] = ["skills", "why_now", "mission", "subsector", "stage", "role", "network"];
pub const DEFAULT_WEIGHTS: [f64; 7] = [0.30, 0.25, 0.15, 0.10, 0.10, 0.05, 0.05];
/// One thumb moves one axis by this much before renormalising; no axis drops below the floor.
pub const NUDGE: f64 = 0.02;
pub const MIN_WEIGHT: f64 = 0.01;
/// Until her intake says otherwise (ruling 6301).
pub const DEFAULT_TRAVEL_MIN: u32 = 90;
/// A matching soft penalty costs this many points, at most `MAX_PENALTY` together.
pub const PENALTY_POINTS: f64 = 5.0;
pub const MAX_PENALTY: f64 = 15.0;
pub const DIGEST_SIZE: usize = 5;
pub const DIGEST_SIGNAL_DAYS: i64 = 60;
pub const DIGEST_PER_SUBSECTOR: usize = 2;

/// What `derive_config` makes of a profile plus her feedback so far.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub weights: [f64; 7],
    pub max_travel_min: u32,
    pub anti_prefs: Vec<String>,
    pub languages: Vec<String>,
    pub subsectors: Vec<(String, f64)>,
    pub stages: Vec<(String, f64)>,
    pub role_types: Vec<(String, f64)>,
    /// Reasons she gave with a thumbs-down; a company that matches one loses points.
    pub penalties: Vec<String>,
}

/// One thumb as stored: +1 or -1, the axis it moves, and her reason.
#[derive(Debug, Clone, PartialEq)]
pub struct Thumb {
    pub up: bool,
    pub axis: String,
    pub reason: String,
}

fn pairs(w: &[Weighted]) -> Vec<(String, f64)> {
    w.iter()
        .map(|x| (x.value.trim().to_lowercase(), x.weight.clamp(0.0, 1.0)))
        .collect()
}

/// How strongly a stated list pulls on its axis: 1 when nothing is stated (the spec default),
/// 0.5 + her strongest weight otherwise.
fn pull(w: &[Weighted]) -> f64 {
    if w.is_empty() {
        return 1.0;
    }
    0.5 + w.iter().map(|x| x.weight.clamp(0.0, 1.0)).fold(0.0, f64::max)
}

pub fn normalise(w: &mut [f64; 7]) {
    for x in w.iter_mut() {
        *x = x.max(MIN_WEIGHT);
    }
    let sum: f64 = w.iter().sum();
    for x in w.iter_mut() {
        *x /= sum;
    }
}

/// Moves one axis by ±`NUDGE`, then renormalises. An unknown axis changes nothing.
pub fn nudge(w: &mut [f64; 7], axis: &str, up: bool) {
    if let Some(i) = AXES.iter().position(|a| *a == axis) {
        w[i] += if up { NUDGE } else { -NUDGE };
        normalise(w);
    }
}

/// Weights from her ranked strengths, role types, stages, sub-sectors and mission themes;
/// what is missing keeps its spec default. Filters from home base, travel, anti-prefs and
/// languages. Then her thumbs, in order.
pub fn derive_config(p: &Profile, thumbs: &[Thumb]) -> Config {
    let skills = if p.strengths.is_empty() {
        1.0
    } else {
        let mut ranks: Vec<u8> = p.strengths.iter().map(|s| s.rank.clamp(1, 5)).collect();
        ranks.sort_unstable_by(|a, b| b.cmp(a));
        let top = &ranks[..ranks.len().min(3)];
        0.5 + top.iter().map(|r| *r as f64).sum::<f64>() / top.len() as f64 / 5.0
    };
    let pulls = [
        skills,
        1.0,
        pull(&p.mission_themes),
        pull(&p.subsectors),
        pull(&p.stages),
        pull(&p.role_types),
        1.0,
    ];
    let mut weights = DEFAULT_WEIGHTS;
    for (w, f) in weights.iter_mut().zip(pulls) {
        *w *= f;
    }
    normalise(&mut weights);
    let mut penalties = Vec::new();
    for t in thumbs {
        nudge(&mut weights, &t.axis, t.up);
        if !t.up && !t.reason.trim().is_empty() {
            penalties.push(t.reason.trim().to_string());
        }
    }
    Config {
        weights,
        max_travel_min: p.max_travel.as_ref().map_or(DEFAULT_TRAVEL_MIN, |t| t.minutes),
        anti_prefs: p.anti_prefs.iter().map(|t| t.text.trim().to_string()).collect(),
        languages: p.languages.iter().map(|l| language(&l.language)).collect(),
        subsectors: pairs(&p.subsectors),
        stages: pairs(&p.stages),
        role_types: pairs(&p.role_types),
        penalties,
    }
}

/// A language name in English lowercase ("Nederlands" → "dutch").
pub fn language(s: &str) -> String {
    let l = s.trim().to_lowercase();
    match l.as_str() {
        "nederlands" | "nl" => "dutch".into(),
        "engels" | "en" => "english".into(),
        "duits" | "deutsch" | "de" => "german".into(),
        "frans" | "français" | "fr" => "french".into(),
        _ => l,
    }
}

/// Spec §6 hard filters, first failing one wins. Unknown travel passes (the page says
/// UNKNOWN; it is never guessed), as does an unknown working language.
pub fn hard_filter(
    cfg: &Config,
    travel_min: Option<u32>,
    anti_pref_hit: &str,
    working_language: &str,
    not_for_me: bool,
) -> Option<String> {
    if not_for_me {
        return Some("You marked it not for you".into());
    }
    if let Some(t) = travel_min.filter(|t| *t > cfg.max_travel_min) {
        return Some(format!("{t} min by car, over your {} min", cfg.max_travel_min));
    }
    let hit = anti_pref_hit.trim();
    if !hit.is_empty() && cfg.anti_prefs.iter().any(|a| a.eq_ignore_ascii_case(hit)) {
        return Some(format!("Hits your \"rather not\": {hit}"));
    }
    let lang = language(working_language);
    if !lang.is_empty() && lang != "unknown" && !cfg.languages.is_empty() && !cfg.languages.contains(&lang) {
        return Some(format!("Works in {lang}, which is not one of your languages"));
    }
    None
}

/// One axis on the company page: its score 0–1, its weight and the one-line reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Axis {
    pub axis: String,
    pub score: f64,
    pub weight: f64,
    pub reason: String,
}

/// Her weight for the company's value on a stated list (sub-sector, stage).
pub fn pref_score(prefs: &[(String, f64)], value: Option<&str>, what: &str) -> (f64, String) {
    if prefs.is_empty() {
        return (0.5, format!("You stated no {what} preference"));
    }
    let Some(v) = value.map(|v| v.trim().to_lowercase()).filter(|v| !v.is_empty()) else {
        return (0.0, format!("Its {what} is UNKNOWN"));
    };
    match prefs.iter().find(|(p, _)| *p == v) {
        Some((_, w)) => (*w, format!("{v}: on your list at weight {w:.1}")),
        None => (0.0, format!("{v}: not on your list")),
    }
}

/// Why-now from the decayed signal strengths (cc-core::company): 1 − Π(1 − s), so two
/// signals count more than one. The reason names the strongest.
pub fn why_now_score(signals: &[(f64, String)]) -> (f64, String) {
    let s = 1.0 - signals.iter().map(|(x, _)| 1.0 - x.clamp(0.0, 1.0)).product::<f64>();
    let best = signals.iter().max_by(|a, b| a.0.total_cmp(&b.0));
    match best {
        Some((x, d)) if *x > 0.0 => (s, format!("{d} (strength {x:.2})")),
        _ => (0.0, "No recent signal".into()),
    }
}

/// Role options: a fresh ops-role signal says they hire; the careers page naming interim,
/// part-time or advisory work says they hire the way she wants.
pub fn role_score(prefs: &[(String, f64)], ops_strength: f64, careers: &str) -> (f64, String) {
    if prefs.is_empty() {
        return (0.5, "You stated no role-type preference".into());
    }
    let page = careers.to_lowercase();
    let mut best = (0.0, "No sign they hire the way you want".to_string());
    for (t, w) in prefs {
        let hit = if t == "permanent" {
            (ops_strength / crate::company::weight("ops_roles")).min(1.0)
        } else if crate::grid::role_words(t).iter().any(|k| page.contains(k)) {
            1.0
        } else {
            0.0
        };
        if hit * w > best.0 {
            best = (hit * w, format!("Signs of {} roles", t.replace('_', "-")));
        }
    }
    best
}

/// Soft penalties: each thumbs-down reason with a telling word in the company's text costs
/// `PENALTY_POINTS`, at most `MAX_PENALTY`. Returns the points and the reasons that matched.
pub fn penalty(reasons: &[String], company_text: &str) -> (f64, Vec<String>) {
    const STOP: [&str; 12] = [
        "about", "their", "there", "these", "those", "which", "would", "could", "should", "because", "rather", "really",
    ];
    let text = company_text.to_lowercase();
    let hits: Vec<String> = reasons
        .iter()
        .filter(|r| {
            r.to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.chars().count() >= 5 && !STOP.contains(w))
                .any(|w| text.contains(w))
        })
        .cloned()
        .collect();
    ((hits.len() as f64 * PENALTY_POINTS).min(MAX_PENALTY), hits)
}

/// Fit = 100 · Σ wᵢ sᵢ − soft penalty, kept within 0–100.
pub fn fit(axes: &[Axis], penalty_points: f64) -> f64 {
    let raw: f64 = axes.iter().map(|a| a.weight * a.score.clamp(0.0, 1.0)).sum();
    (100.0 * raw - penalty_points).clamp(0.0, 100.0)
}

/// The axis that did the most for this score (what a thumb without a named axis moves).
pub fn top_axis(axes: &[Axis]) -> Option<String> {
    axes.iter()
        .max_by(|a, b| (a.weight * a.score).total_cmp(&(b.weight * b.score)))
        .map(|a| a.axis.clone())
}

/// A scored company as the digest sees it.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: Uuid,
    pub score: f64,
    pub subsector: Option<String>,
    /// Date of its newest why-now signal.
    pub newest_signal: Option<NaiveDate>,
    /// Excluded by a hard filter, dismissed or already contacted.
    pub out: bool,
}

/// Spec §6: the top scores, each with a why-now signal from the last 60 days, at most two
/// from one sub-sector (an unknown sub-sector is one group), none excluded. Up to five.
pub fn pick_digest(cands: &[Candidate], today: NaiveDate) -> Vec<Uuid> {
    let mut c: Vec<&Candidate> = cands
        .iter()
        .filter(|c| !c.out)
        .filter(|c| {
            c.newest_signal
                .is_some_and(|d| (today - d).num_days() <= DIGEST_SIGNAL_DAYS)
        })
        .collect();
    c.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut picked: Vec<(Uuid, String)> = Vec::new();
    for x in c {
        let group = x.subsector.clone().unwrap_or_default().to_lowercase();
        if picked.iter().filter(|p| p.1 == group).count() < DIGEST_PER_SUBSECTOR {
            picked.push((x.id, group));
        }
        if picked.len() == DIGEST_SIZE {
            break;
        }
    }
    picked.into_iter().map(|p| p.0).collect()
}

/// Amsterdam's offset from UTC in hours on this instant (EU rule: summer time from the last
/// Sunday of March to the last Sunday of October, 01:00 UTC).
fn amsterdam_offset(t: DateTime<Utc>) -> i64 {
    let last_sunday = |m: u32| {
        let end = NaiveDate::from_ymd_opt(t.year(), m, 31).expect("March and October have 31 days");
        let d = end - Duration::days(end.weekday().num_days_from_sunday() as i64);
        Utc.from_utc_datetime(&d.and_hms_opt(1, 0, 0).expect("valid time"))
    };
    if t >= last_sunday(3) && t < last_sunday(10) {
        2
    } else {
        1
    }
}

/// The next Monday 07:00 Amsterdam time strictly after `now`.
pub fn next_digest_at(now: DateTime<Utc>) -> DateTime<Utc> {
    let mut day = now.date_naive();
    loop {
        if day.weekday() == Weekday::Mon {
            let naive = day.and_hms_opt(7, 0, 0).expect("valid time");
            let guess = Utc.from_utc_datetime(&naive) - Duration::hours(1);
            let at = Utc.from_utc_datetime(&naive) - Duration::hours(amsterdam_offset(guess));
            if at > now {
                return at;
            }
        }
        day += Duration::days(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Language, Strength, Text, Travel};

    fn w(v: &str, weight: f64) -> Weighted {
        Weighted {
            value: v.into(),
            weight,
            source_ids: vec![],
        }
    }

    fn sum(w: &[f64; 7]) -> f64 {
        w.iter().sum()
    }

    /// Three synthetic profiles: empty, partial, full.
    #[test]
    fn derive_config_from_three_synthetic_profiles() {
        // 1. Nothing stated: exactly the spec defaults, 90 min, no filters.
        let c = derive_config(&Profile::default(), &[]);
        for (a, b) in c.weights.iter().zip(DEFAULT_WEIGHTS) {
            assert!((a - b).abs() < 1e-12);
        }
        assert_eq!(c.max_travel_min, DEFAULT_TRAVEL_MIN);
        assert!(c.anti_prefs.is_empty() && c.languages.is_empty());

        // 2. Only a strong sub-sector: that axis gains, the others keep their ratio.
        let p = Profile {
            subsectors: vec![w("Medtech", 1.0), w("care_tech", 0.4)],
            ..Default::default()
        };
        let c = derive_config(&p, &[]);
        assert!((sum(&c.weights) - 1.0).abs() < 1e-12);
        assert!(c.weights[3] > DEFAULT_WEIGHTS[3]);
        assert!((c.weights[0] / c.weights[1] - 0.30 / 0.25).abs() < 1e-9);
        assert_eq!(c.subsectors[0], ("medtech".to_string(), 1.0));

        // 3. Everything stated (a synthetic test profile, made up).
        let p = Profile {
            strengths: vec![
                Strength {
                    skill: "change management".into(),
                    rank: 5,
                    evidence_ids: vec![],
                    source_ids: vec![],
                },
                Strength {
                    skill: "stakeholders".into(),
                    rank: 4,
                    evidence_ids: vec![],
                    source_ids: vec![],
                },
            ],
            role_types: vec![w("interim", 0.6)],
            stages: vec![w("scaleup", 0.2)],
            subsectors: vec![w("digital_health", 0.8)],
            mission_themes: vec![w("hospital at home", 0.9)],
            anti_prefs: vec![Text {
                text: " pure sales targets ".into(),
                source_ids: vec![],
            }],
            max_travel: Some(Travel {
                minutes: 45,
                mode: None,
                source_ids: vec![],
            }),
            languages: vec![Language {
                language: "Nederlands".into(),
                level: "native".into(),
                source_ids: vec![],
            }],
            ..Default::default()
        };
        let c = derive_config(&p, &[]);
        assert!((sum(&c.weights) - 1.0).abs() < 1e-12);
        assert!(c.weights.iter().all(|x| *x >= MIN_WEIGHT && *x < 1.0));
        // skills 0.30 × 1.4 outweighs stage 0.10 × 0.7 by far.
        assert!(c.weights[0] > 5.0 * c.weights[4]);
        assert_eq!(c.max_travel_min, 45);
        assert_eq!(c.anti_prefs, vec!["pure sales targets"]);
        assert_eq!(c.languages, vec!["dutch"]);
    }

    #[test]
    fn feedback_nudges_and_renormalises() {
        let base = derive_config(&Profile::default(), &[]);
        let down = Thumb {
            up: false,
            axis: "subsector".into(),
            reason: "No pharma distributors".into(),
        };
        let c = derive_config(&Profile::default(), std::slice::from_ref(&down));
        assert!((sum(&c.weights) - 1.0).abs() < 1e-12);
        // 0.10 - 0.02 = 0.08 over a total of 0.98.
        assert!((c.weights[3] - 0.08 / 0.98).abs() < 1e-12);
        assert!(c.weights[0] > base.weights[0]);
        assert_eq!(c.penalties, vec!["No pharma distributors"]);
        let up = Thumb {
            up: true,
            axis: "why_now".into(),
            reason: String::new(),
        };
        let c = derive_config(&Profile::default(), &[up]);
        assert!((c.weights[1] - 0.27 / 1.02).abs() < 1e-12);
        assert!(c.penalties.is_empty());
        // Many thumbs down never push an axis to zero.
        let many = vec![down; 50];
        let c = derive_config(&Profile::default(), &many);
        assert!(c.weights[3] >= MIN_WEIGHT / 1.0001 && (sum(&c.weights) - 1.0).abs() < 1e-12);
        let mut x = DEFAULT_WEIGHTS;
        nudge(&mut x, "gossip", true);
        assert_eq!(x, DEFAULT_WEIGHTS);
    }

    #[test]
    fn hard_filters() {
        let mut c = derive_config(&Profile::default(), &[]);
        c.anti_prefs = vec!["pure sales targets".into()];
        c.languages = vec!["dutch".into(), "english".into()];
        assert_eq!(hard_filter(&c, Some(80), "", "Dutch", false), None);
        assert_eq!(
            hard_filter(&c, None, "", "unknown", false),
            None,
            "unknown travel passes"
        );
        assert!(hard_filter(&c, Some(91), "", "", false).unwrap().contains("91 min"));
        assert!(hard_filter(&c, Some(10), "Pure sales targets", "", false).is_some());
        assert_eq!(hard_filter(&c, Some(10), "something else", "", false), None);
        assert!(hard_filter(&c, Some(10), "", "German", false).is_some());
        assert!(hard_filter(&c, Some(10), "", "", true).is_some());
    }

    #[test]
    fn scores_stay_in_bounds() {
        let prefs = vec![("medtech".to_string(), 0.8)];
        assert_eq!(pref_score(&prefs, Some("MedTech"), "sector").0, 0.8);
        assert_eq!(pref_score(&prefs, Some("pharma"), "sector").0, 0.0);
        assert!(pref_score(&prefs, None, "sector").1.contains("UNKNOWN"));
        assert_eq!(pref_score(&[], Some("x"), "sector").0, 0.5);
        let (s, r) = why_now_score(&[(1.0, "Series B".into()), (0.5, "New COO".into())]);
        assert_eq!(s, 1.0);
        assert!(r.starts_with("Series B"));
        let (s, _) = why_now_score(&[(0.5, "a".into()), (0.5, "b".into())]);
        assert!((s - 0.75).abs() < 1e-12);
        assert_eq!(why_now_score(&[]).0, 0.0);
        let roles = vec![("interim".to_string(), 1.0), ("permanent".to_string(), 0.5)];
        assert_eq!(role_score(&roles, 0.0, "We hire an Interim lead").0, 1.0);
        assert!((role_score(&roles, 0.8, "").0 - 0.5).abs() < 1e-12);
        let (p, hits) = penalty(&["no pharma distributors".into()], "A pharma wholesaler");
        assert_eq!((p, hits.len()), (PENALTY_POINTS, 1));
        assert_eq!(penalty(&["no pharma".into()], "medtech").0, 0.0);
        let many: Vec<String> = (0..9).map(|_| "pharma".into()).collect();
        assert_eq!(penalty(&many, "pharma").0, MAX_PENALTY);
        // Fit is 0–100 whatever comes in.
        let axes: Vec<Axis> = AXES
            .iter()
            .zip(DEFAULT_WEIGHTS)
            .map(|(a, w)| Axis {
                axis: a.to_string(),
                score: 7.0,
                weight: w,
                reason: String::new(),
            })
            .collect();
        assert!((fit(&axes, 0.0) - 100.0).abs() < 1e-9);
        assert_eq!(fit(&axes, 500.0), 0.0);
        assert_eq!(top_axis(&axes).as_deref(), Some("skills"));
    }

    #[test]
    fn digest_rules() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 30).unwrap();
        let fresh = Some(today - Duration::days(20));
        let c = |n: u128, score: f64, sub: &str, sig: Option<NaiveDate>, out: bool| Candidate {
            id: Uuid::from_u128(n),
            score,
            subsector: Some(sub.into()),
            newest_signal: sig,
            out,
        };
        let cands = vec![
            c(1, 99.0, "medtech", fresh, true),                             // excluded
            c(2, 95.0, "medtech", Some(today - Duration::days(61)), false), // signal too old
            c(3, 90.0, "medtech", fresh, false),
            c(4, 89.0, "medtech", fresh, false),
            c(5, 88.0, "medtech", fresh, false), // third medtech
            c(6, 80.0, "care_tech", fresh, false),
            c(7, 70.0, "diagnostics", fresh, false),
            c(8, 60.0, "digital_health", fresh, false),
            c(9, 50.0, "other", fresh, false),
            c(10, 95.0, "other", None, false), // no signal at all
        ];
        let ids: Vec<u128> = pick_digest(&cands, today).iter().map(|u| u.as_u128()).collect();
        assert_eq!(ids, vec![3, 4, 6, 7, 8]);
        assert!(pick_digest(&cands[..2], today).is_empty());
    }

    #[test]
    fn digest_runs_monday_seven_amsterdam() {
        let t = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        // Wednesday in winter: next Monday 07:00 CET = 06:00 UTC.
        assert_eq!(next_digest_at(t("2026-11-25T15:00:00Z")), t("2026-11-30T06:00:00Z"));
        // Summer: 07:00 CEST = 05:00 UTC; at that minute exactly it is the week after.
        assert_eq!(next_digest_at(t("2026-06-01T05:00:00Z")), t("2026-06-08T05:00:00Z"));
        assert_eq!(next_digest_at(t("2026-06-01T04:59:00Z")), t("2026-06-01T05:00:00Z"));
    }
}
