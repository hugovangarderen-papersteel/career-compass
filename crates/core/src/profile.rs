//! Her profile (spec §4) and the intake rules that do not need a model or a database.
//!
//! Every item carries `source_ids`: `T<n>` is transcript turn n, `D<n>` is a document she
//! confirmed. An item without a valid source is dropped, never kept on trust (spec §3, §11).
//! Salary is left out on purpose: it needs field-level encryption first (spec §11).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const PHASES: u8 = 8;
pub const PITCH_MAX_WORDS: usize = 150;
pub const VOICE_MIN: usize = 3;
pub const VOICE_MAX: usize = 5;

/// Dutch names of the eight intake phases (spec §3), shown as "Fase 3 van 8 · Verhalen".
pub const PHASE_NAMES: [&str; 8] = [
    "Kennismaken",
    "Loopbaan",
    "Verhalen",
    "Sterktes en energie",
    "Wat je zoekt",
    "Randvoorwaarden",
    "De pauze",
    "Terugblik",
];

pub fn phase_name(phase: u8) -> &'static str {
    PHASE_NAMES[(phase.clamp(1, PHASES) - 1) as usize]
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Text {
    pub text: String,
    #[serde(default)]
    pub source_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Weighted {
    pub value: String,
    /// 0..=1, how much it matters to her.
    pub weight: f64,
    #[serde(default)]
    pub source_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub role: String,
    pub org: String,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub team_size: Option<String>,
    #[serde(default)]
    pub source_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// `E1`, `E2`… so strengths can point at it.
    pub id: String,
    pub title: String,
    pub situation: String,
    pub task: String,
    pub action: String,
    pub result: String,
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub source_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Strength {
    pub skill: String,
    /// 1..=5
    /// 1 to 5; 5 = the strongest-evidenced strength.
    pub rank: u8,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub source_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Language {
    pub language: String,
    pub level: String,
    #[serde(default)]
    pub source_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HomeBase {
    pub city: String,
    #[serde(default)]
    pub source_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Travel {
    pub minutes: u32,
    /// "auto", "ov" or empty when she did not say.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub source_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Hours {
    pub min: u32,
    pub max: u32,
    #[serde(default)]
    pub source_ids: Vec<String>,
}

pub const ROLE_TYPES: [&str; 4] = ["permanent", "interim", "part_time", "advisory"];
pub const STAGES: [&str; 4] = ["startup", "scaleup", "midsize", "corporate"];
pub const SUBSECTORS: [&str; 5] = ["medtech", "digital_health", "care_tech", "diagnostics", "other"];

/// Spec §4. Lists default to empty and optionals to None, so a partial draft still parses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub pitch: Text,
    #[serde(default)]
    pub timeline: Vec<TimelineEntry>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    #[serde(default)]
    pub strengths: Vec<Strength>,
    #[serde(default)]
    pub anti_prefs: Vec<Text>,
    #[serde(default)]
    pub work_style: Vec<Text>,
    #[serde(default)]
    pub role_types: Vec<Weighted>,
    #[serde(default)]
    pub stages: Vec<Weighted>,
    #[serde(default)]
    pub subsectors: Vec<Weighted>,
    #[serde(default)]
    pub mission_themes: Vec<Weighted>,
    #[serde(default)]
    pub home_base: Option<HomeBase>,
    #[serde(default)]
    pub max_travel: Option<Travel>,
    #[serde(default)]
    pub hours_per_week: Option<Hours>,
    #[serde(default)]
    pub work_mode: Option<Text>,
    #[serde(default)]
    pub start_date: Option<Text>,
    #[serde(default)]
    pub gap_framing: Option<Text>,
    #[serde(default)]
    pub voice_sample: Vec<Text>,
    #[serde(default)]
    pub languages: Vec<Language>,
}

/// Playback sections (spec §3 phase 8), in reading order, with the profile keys each one holds.
pub const SECTIONS: [(&str, &str, &[&str]); 8] = [
    ("pitch", "Je pitch", &["pitch"]),
    ("timeline", "Je loopbaan", &["timeline"]),
    ("evidence", "Je verhalen", &["evidence"]),
    (
        "strengths",
        "Waar je sterk in bent",
        &["strengths", "work_style", "anti_prefs"],
    ),
    (
        "wants",
        "Wat je zoekt",
        &["role_types", "stages", "subsectors", "mission_themes"],
    ),
    (
        "constraints",
        "Je randvoorwaarden",
        &[
            "home_base",
            "max_travel",
            "hours_per_week",
            "work_mode",
            "start_date",
            "languages",
        ],
    ),
    ("gap", "De pauze in één zin", &["gap_framing"]),
    ("voice", "Zo klink jij", &["voice_sample"]),
];

pub fn section_keys(section: &str) -> Option<&'static [&'static str]> {
    SECTIONS.iter().find(|s| s.0 == section).map(|s| s.2)
}

/// Collapses whitespace and case so a quote can be checked against what she said.
pub fn normalise(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

pub fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Cut `s` to at most `max` words, ending on the last full sentence past the halfway mark
/// when there is one, so a too-long pitch never stops mid-thought.
pub fn trim_to_words(s: &str, max: usize) -> String {
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.len() <= max {
        return s.trim().to_string();
    }
    let cut = &words[..max];
    let end = cut
        .iter()
        .rposition(|w| w.ends_with(['.', '!', '?']))
        .filter(|i| *i + 1 >= max / 2);
    match end {
        Some(i) => cut[..=i].join(" "),
        None => format!("{}…", cut.join(" ")),
    }
}

/// What `clean` removed or found wrong; shown in logs, never to her.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct CleanReport {
    pub dropped: Vec<String>,
    pub problems: Vec<String>,
}

/// Strips JSON debris the model sometimes leaves at the end of a text (`staat."}."}`).
/// Only a tail that holds a brace or bracket is touched, so a real closing quote stays.
pub fn tidy(s: &str) -> String {
    let t = s.trim_end();
    let body = t.trim_end_matches(['"', '}', ']', '.', ' ']);
    let tail = &t[body.len()..];
    if !tail.contains(['}', ']']) {
        return t.to_string();
    }
    if tail.contains('.') {
        format!("{body}.")
    } else {
        body.to_string()
    }
}

fn keep_sources(ids: &mut Vec<String>, valid: &HashSet<String>) -> bool {
    ids.retain(|i| valid.contains(i.trim()));
    ids.sort();
    ids.dedup();
    !ids.is_empty()
}

impl Profile {
    /// Enforces "no field without a source": drops every item whose sources are all unknown,
    /// clamps ranks and weights, drops voice sentences that are not her own words, and
    /// lists what still falls short (pitch length, voice sample count).
    /// `valid` = the source ids that exist; `her_text` = everything she said, for quote checks.
    pub fn clean(&mut self, valid: &HashSet<String>, her_text: &[String]) -> CleanReport {
        let mut r = CleanReport::default();
        macro_rules! retain {
            ($v:expr, $name:literal) => {
                let before = $v.len();
                $v.retain_mut(|x| keep_sources(&mut x.source_ids, valid));
                for _ in $v.len()..before {
                    r.dropped.push($name.to_string());
                }
            };
        }
        macro_rules! opt {
            ($o:expr, $name:literal) => {
                if let Some(x) = $o.as_mut() {
                    if !keep_sources(&mut x.source_ids, valid) {
                        $o = None;
                        r.dropped.push($name.to_string());
                    }
                }
            };
        }
        retain!(self.timeline, "timeline");
        retain!(self.evidence, "evidence");
        retain!(self.strengths, "strengths");
        retain!(self.anti_prefs, "anti_prefs");
        retain!(self.work_style, "work_style");
        retain!(self.role_types, "role_types");
        retain!(self.stages, "stages");
        retain!(self.subsectors, "subsectors");
        retain!(self.mission_themes, "mission_themes");
        retain!(self.languages, "languages");
        opt!(self.home_base, "home_base");
        opt!(self.max_travel, "max_travel");
        opt!(self.hours_per_week, "hours_per_week");
        opt!(self.work_mode, "work_mode");
        opt!(self.start_date, "start_date");
        opt!(self.gap_framing, "gap_framing");
        self.pitch.text = tidy(&self.pitch.text);
        for t in self
            .voice_sample
            .iter_mut()
            .chain(self.gap_framing.iter_mut())
            .chain(self.work_mode.iter_mut())
            .chain(self.start_date.iter_mut())
        {
            t.text = tidy(&t.text);
        }
        if !self.pitch.text.trim().is_empty() && !keep_sources(&mut self.pitch.source_ids, valid) {
            r.problems.push("pitch has no source".into());
        }

        self.role_types.retain(|x| ROLE_TYPES.contains(&x.value.as_str()));
        self.stages.retain(|x| STAGES.contains(&x.value.as_str()));
        for s in &mut self.subsectors {
            if !SUBSECTORS.contains(&s.value.as_str()) {
                s.value = "other".into();
            }
        }
        for w in self
            .role_types
            .iter_mut()
            .chain(self.stages.iter_mut())
            .chain(self.subsectors.iter_mut())
            .chain(self.mission_themes.iter_mut())
        {
            w.weight = if w.weight.is_finite() {
                w.weight.clamp(0.0, 1.0)
            } else {
                0.5
            };
        }
        let ev: HashSet<String> = self.evidence.iter().map(|e| e.id.clone()).collect();
        for s in &mut self.strengths {
            s.rank = s.rank.clamp(1, 5);
            s.evidence_ids.retain(|e| ev.contains(e));
        }
        self.strengths.sort_by_key(|s| std::cmp::Reverse(s.rank));
        if let Some(h) = self.hours_per_week.as_mut() {
            if h.min > h.max {
                std::mem::swap(&mut h.min, &mut h.max);
            }
        }

        // Voice sample: only sentences she actually said, verbatim (spec §3 output c).
        let hers: Vec<String> = her_text.iter().map(|t| normalise(t)).collect();
        let before = self.voice_sample.len();
        self.voice_sample.retain_mut(|v| {
            let q = normalise(&v.text);
            !q.is_empty() && hers.iter().any(|h| h.contains(&q)) && keep_sources(&mut v.source_ids, valid)
        });
        for _ in self.voice_sample.len()..before {
            r.dropped.push("voice_sample (not her own words)".into());
        }
        self.voice_sample.truncate(VOICE_MAX);

        let words = word_count(&self.pitch.text);
        if words > PITCH_MAX_WORDS {
            r.problems
                .push(format!("pitch is {words} words, max {PITCH_MAX_WORDS}"));
        }
        if self.voice_sample.len() < VOICE_MIN {
            r.problems.push(format!(
                "voice sample has {} sentences, wants {VOICE_MIN}-{VOICE_MAX}",
                self.voice_sample.len()
            ));
        }
        r
    }
}

/// Where the intake stands; the server owns it, the model only says "this phase is done".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// 1..=8, the phase Kompas is in now.
    pub phase: u8,
    /// Phases covered enough to move on.
    pub done: Vec<u8>,
}

impl Default for Progress {
    fn default() -> Self {
        Progress { phase: 1, done: vec![] }
    }
}

impl Progress {
    /// Moves on one phase at most per answer, never back, never past 8.
    pub fn advance(&mut self, phase_complete: bool) {
        if phase_complete && self.phase < PHASES {
            if !self.done.contains(&self.phase) {
                self.done.push(self.phase);
            }
            self.phase += 1;
        }
    }
    /// The "N van 8" on her screen: phases covered so far.
    pub fn covered(&self) -> usize {
        self.done.len()
    }
    /// Phase 8 is the playback: the conversation part is complete.
    pub fn ready_for_playback(&self) -> bool {
        self.phase == PHASES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_strips_json_debris_only() {
        assert_eq!(tidy("centraal staat.\"}.\"}"), "centraal staat.");
        assert_eq!(tidy("klaar\"}"), "klaar");
        assert_eq!(tidy("Ze zei \"ja\"."), "Ze zei \"ja\".");
        assert_eq!(tidy("Waarom niet? "), "Waarom niet?");
    }

    #[test]
    fn trims_at_sentence_end() {
        assert_eq!(trim_to_words("Een twee drie.", 5), "Een twee drie.");
        let long = "Ik bouw teams. Ik maak zorg beter. En dan nog veel meer woorden hier";
        assert_eq!(trim_to_words(long, 10), "Ik bouw teams. Ik maak zorg beter.");
        assert_eq!(trim_to_words("a b c d e f", 3), "a b c…");
    }

    fn ids(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn sample() -> Profile {
        serde_json::from_value(serde_json::json!({
            "pitch": {"text": "Ik help zorgteams nieuwe technologie echt te gebruiken.", "source_ids": ["T4", "T99"]},
            "timeline": [
                {"role": "Programmamanager", "org": "Voorbeeld BV", "start": "2010", "end": "2016", "source_ids": ["T4"]},
                {"role": "Verzonnen rol", "org": "Nergens", "source_ids": ["T77"]}
            ],
            "evidence": [{"id": "E1", "title": "Uitrol", "situation": "s", "task": "t", "action": "a", "result": "r",
                          "skills": ["verandermanagement"], "source_ids": ["T6"]}],
            "strengths": [{"skill": "verandermanagement", "rank": 9, "evidence_ids": ["E1", "E7"], "source_ids": ["T6"]}],
            "role_types": [{"value": "interim", "weight": 1.4, "source_ids": ["T8"]},
                           {"value": "bogus", "weight": 1, "source_ids": ["T8"]}],
            "subsectors": [{"value": "space", "weight": 0.5, "source_ids": ["T8"]}],
            "hours_per_week": {"min": 32, "max": 24, "source_ids": ["T10"]},
            "home_base": {"city": "Ergens", "source_ids": []},
            "voice_sample": [
                {"text": "Ik word blij van een team dat het zelf gaat doen.", "source_ids": ["T6"]},
                {"text": "Dit heeft ze nooit gezegd.", "source_ids": ["T6"]}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn clean_drops_unsourced_and_invented() {
        let mut p = sample();
        let her = vec!["Nou, ik word   blij van een team dat het zelf gaat doen! Echt.".to_string()];
        let r = p.clean(&ids(&["T4", "T6", "T8", "T10"]), &her);
        assert_eq!(p.timeline.len(), 1, "item with only an unknown source is dropped");
        assert_eq!(p.pitch.source_ids, vec!["T4"]);
        assert!(p.home_base.is_none(), "no source → dropped");
        assert_eq!(p.strengths[0].rank, 5);
        assert_eq!(p.strengths[0].evidence_ids, vec!["E1"]);
        assert_eq!(p.role_types.len(), 1);
        assert_eq!(p.role_types[0].weight, 1.0);
        assert_eq!(p.subsectors[0].value, "other");
        let h = p.hours_per_week.as_ref().unwrap();
        assert_eq!((h.min, h.max), (24, 32));
        assert_eq!(p.voice_sample.len(), 1, "a sentence she never said is dropped");
        assert!(r.problems.iter().any(|x| x.contains("voice sample has 1")));
        assert!(r.dropped.contains(&"home_base".to_string()));
    }

    #[test]
    fn pitch_word_limit() {
        let mut p = Profile {
            pitch: Text {
                text: "woord ".repeat(151),
                source_ids: vec!["T1".into()],
            },
            ..Default::default()
        };
        let r = p.clean(&ids(&["T1"]), &[]);
        assert!(r.problems.iter().any(|x| x.contains("151 words")));
    }

    #[test]
    fn partial_json_parses() {
        let p: Profile = serde_json::from_str("{}").unwrap();
        assert!(p.timeline.is_empty() && p.home_base.is_none());
    }

    #[test]
    fn progress_moves_one_step_and_stops_at_eight() {
        let mut g = Progress::default();
        g.advance(false);
        assert_eq!(g.phase, 1);
        for _ in 0..20 {
            g.advance(true);
        }
        assert_eq!(g.phase, 8);
        assert_eq!(g.covered(), 7);
        assert!(g.ready_for_playback());
        assert_eq!(phase_name(3), "Verhalen");
    }

    #[test]
    fn every_profile_key_is_in_one_section() {
        let v = serde_json::to_value(Profile::default()).unwrap();
        let keys: Vec<&str> = SECTIONS.iter().flat_map(|s| s.2.iter().copied()).collect();
        for k in v.as_object().unwrap().keys() {
            assert_eq!(keys.iter().filter(|x| **x == k).count(), 1, "{k}");
        }
    }
}
