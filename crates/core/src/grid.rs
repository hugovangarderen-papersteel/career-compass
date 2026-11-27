//! Phase 5: the companies grid, the map's travel ring and her own ranking. Pure functions; the
//! api feeds them rows from the database. Unknown stays unknown: a company with no travel time
//! is neither inside nor outside the ring, and one with no headcount has no size.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

/// At most this many companies in her ranking.
pub const MAX_RANKED: usize = 50;

/// Where a company sits against her travel limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ring {
    Inside,
    Outside,
    Unknown,
}

impl Ring {
    pub fn as_str(self) -> &'static str {
        match self {
            Ring::Inside => "inside",
            Ring::Outside => "outside",
            Ring::Unknown => "unknown",
        }
    }
}

/// Inside when the road time is at most her limit; no time is `Unknown`, never a guess.
pub fn ring(travel_min: Option<u32>, max_min: u32) -> Ring {
    match travel_min {
        Some(t) if t <= max_min => Ring::Inside,
        Some(_) => Ring::Outside,
        None => Ring::Unknown,
    }
}

/// Size bands by headcount (the EU SME cut-offs): small < 50, medium 50-249, large 250+.
pub fn size_band(headcount: Option<f64>) -> &'static str {
    match headcount {
        Some(n) if n < 50.0 => "small",
        Some(n) if n < 250.0 => "medium",
        Some(_) => "large",
        None => "unknown",
    }
}

pub const ROLE_FORMS: [&str; 4] = ["permanent", "interim", "part_time", "advisory"];

/// Words on a careers page that show a role form. `permanent` comes from the ops-role signal.
pub fn role_words(form: &str) -> &'static [&'static str] {
    match form {
        "interim" => &["interim", "freelance", "zzp"],
        "part_time" => &["part-time", "part time", "parttime"],
        "advisory" => &["advisor", "adviseur", "advisory board"],
        _ => &[],
    }
}

/// The role forms a company shows signs of: permanent from a fresh ops-role signal, the
/// others from words on its careers pages.
pub fn role_forms(ops_strength: f64, careers: &str) -> Vec<String> {
    let page = careers.to_lowercase();
    ROLE_FORMS
        .iter()
        .filter(|f| match **f {
            "permanent" => ops_strength > 0.0,
            f => role_words(f).iter().any(|w| page.contains(w)),
        })
        .map(|f| f.to_string())
        .collect()
}

/// One company in the grid and on the map. `forms` is None until it is scored with Phase 5.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub id: Uuid,
    pub name: String,
    pub sector: Option<String>,
    pub city: Option<String>,
    pub size: String,
    pub ring: Ring,
    pub travel_min: Option<u32>,
    pub forms: Option<Vec<String>>,
    pub score: Option<f64>,
    pub excluded: Option<String>,
    pub rank: Option<usize>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

/// The grid's filters; empty or missing means "all".
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Filter {
    pub size: Option<String>,
    pub sector: Option<String>,
    pub ring: Option<String>,
    pub role: Option<String>,
}

fn set(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// Does a row pass every filter that is set? A role filter keeps only companies that show
/// signs of that form; one not yet checked for forms does not pass.
pub fn keep(f: &Filter, r: &Row) -> bool {
    set(&f.size).is_none_or(|s| r.size == s)
        && set(&f.sector).is_none_or(|s| r.sector.as_deref() == Some(s))
        && set(&f.ring).is_none_or(|s| r.ring.as_str() == s)
        && set(&f.role).is_none_or(|s| r.forms.as_ref().is_some_and(|fs| fs.iter().any(|x| x == s)))
}

/// Her ranking first (in her order), then the best fit, then the excluded and unscored by name.
pub fn order(rows: &mut [Row]) {
    rows.sort_by(|a, b| {
        let rank = |r: &Row| r.rank.unwrap_or(usize::MAX);
        rank(a)
            .cmp(&rank(b))
            .then_with(|| b.score.unwrap_or(f64::MIN).total_cmp(&a.score.unwrap_or(f64::MIN)))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// The ranking she saves: known companies only, each once, in her order, at most `MAX_RANKED`.
pub fn clean_ranking(ids: &[Uuid], known: &HashSet<Uuid>) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    ids.iter()
        .copied()
        .filter(|id| known.contains(id) && seen.insert(*id))
        .take(MAX_RANKED)
        .collect()
}

/// How many rows sit inside, outside and with unknown travel.
pub fn ring_counts(rows: &[Row]) -> (usize, usize, usize) {
    let n = |k: Ring| rows.iter().filter(|r| r.ring == k).count();
    (n(Ring::Inside), n(Ring::Outside), n(Ring::Unknown))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, score: Option<f64>, rank: Option<usize>) -> Row {
        Row {
            id: Uuid::new_v4(),
            name: name.into(),
            sector: Some("medtech".into()),
            city: None,
            size: "medium".into(),
            ring: Ring::Inside,
            travel_min: Some(40),
            forms: Some(vec!["permanent".into()]),
            score,
            excluded: None,
            rank,
            lat: None,
            lon: None,
        }
    }

    #[test]
    fn ring_classifies_by_road_time() {
        assert_eq!(ring(Some(90), 90), Ring::Inside);
        assert_eq!(ring(Some(0), 90), Ring::Inside);
        assert_eq!(ring(Some(91), 90), Ring::Outside);
        assert_eq!(ring(None, 90), Ring::Unknown);
        assert_eq!(ring(Some(50), 45), Ring::Outside);
        let mut rows = vec![row("a", None, None), row("b", None, None), row("c", None, None)];
        rows[1].ring = ring(Some(120), 90);
        rows[2].ring = ring(None, 90);
        assert_eq!(ring_counts(&rows), (1, 1, 1));
    }

    #[test]
    fn sizes_and_role_forms() {
        assert_eq!(size_band(Some(12.0)), "small");
        assert_eq!(size_band(Some(50.0)), "medium");
        assert_eq!(size_band(Some(249.0)), "medium");
        assert_eq!(size_band(Some(250.0)), "large");
        assert_eq!(size_band(None), "unknown");
        assert_eq!(
            role_forms(0.0, "We hire an Interim lead, Part-time OK"),
            ["interim", "part_time"]
        );
        assert_eq!(role_forms(0.8, ""), ["permanent"]);
        assert!(role_forms(0.0, "").is_empty());
    }

    #[test]
    fn filters_keep_only_matching_rows() {
        let mut big = row("Big", Some(70.0), None);
        big.size = "large".into();
        big.ring = Ring::Outside;
        big.sector = Some("diagnostics".into());
        big.forms = Some(vec!["interim".into()]);
        let mut unchecked = row("New", None, None);
        unchecked.forms = None;
        let rows = [row("Mid", Some(60.0), None), big, unchecked];
        let names =
            |f: Filter| -> Vec<String> { rows.iter().filter(|r| keep(&f, r)).map(|r| r.name.clone()).collect() };
        assert_eq!(names(Filter::default()).len(), 3);
        let all = Filter {
            size: Some(" ".into()),
            sector: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(names(all).len(), 3);
        let size = Filter {
            size: Some("large".into()),
            ..Default::default()
        };
        assert_eq!(names(size), ["Big"]);
        let sector = Filter {
            sector: Some("medtech".into()),
            ..Default::default()
        };
        assert_eq!(names(sector), ["Mid", "New"]);
        let inside = Filter {
            ring: Some("inside".into()),
            ..Default::default()
        };
        assert_eq!(names(inside), ["Mid", "New"]);
        let interim = Filter {
            role: Some("interim".into()),
            ..Default::default()
        };
        assert_eq!(names(interim), ["Big"]);
        let both = Filter {
            ring: Some("inside".into()),
            role: Some("permanent".into()),
            ..Default::default()
        };
        assert_eq!(names(both), ["Mid"]);
    }

    #[test]
    fn ranking_comes_first_then_score_then_name() {
        let mut rows = vec![
            row("zeta", None, None),
            row("Alpha", None, None),
            row("best", Some(90.0), None),
            row("good", Some(50.0), None),
            row("mine2", Some(10.0), Some(1)),
            row("mine1", None, Some(0)),
        ];
        order(&mut rows);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["mine1", "mine2", "best", "good", "Alpha", "zeta"]);
    }

    #[test]
    fn saved_ranking_is_clean() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let known: HashSet<Uuid> = [a, b].into_iter().collect();
        assert_eq!(clean_ranking(&[b, c, a, b], &known), [b, a]);
        let many: Vec<Uuid> = (0..80).map(|_| Uuid::new_v4()).collect();
        let known: HashSet<Uuid> = many.iter().copied().collect();
        assert_eq!(clean_ranking(&many, &known).len(), MAX_RANKED);
        assert_eq!(clean_ranking(&many, &known)[0], many[0]);
    }
}
