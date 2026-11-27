//! Career Compass domain types shared by the api, the worker and the desktop app.
//! Numbers here come from the build spec v2 (§2b pairing, §10b cost cap).

pub mod company;
pub mod grid;
pub mod matching;
pub mod outreach;
pub mod profile;

use rand::Rng;
use serde::{Deserialize, Serialize};

/// A pairing request expires after 15 minutes (spec §2b).
pub const PAIRING_TTL_MINUTES: i64 = 15;
/// Hard budget for everything she uses, EUR per month (spec §10b).
pub const DEFAULT_CAP_EUR: f64 = 30.0;
/// Pill thresholds as a fraction of the cap: green below, amber between, red above (spec §10b).
pub const AMBER_FROM: f64 = 0.60;
pub const RED_ABOVE: f64 = 0.90;
/// Her session lifetime; the app refreshes it silently while the device stays approved.
pub const SESSION_HOURS: i64 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Seeker,
    Admin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Seeker => "seeker",
            Role::Admin => "admin",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "seeker" => Some(Role::Seeker),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
}

/// Lifecycle of one device: pending (waiting for Hugo) -> approved -> revoked,
/// or pending -> denied / expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceStatus {
    Pending,
    Approved,
    Denied,
    Expired,
    Revoked,
}

impl DeviceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceStatus::Pending => "pending",
            DeviceStatus::Approved => "approved",
            DeviceStatus::Denied => "denied",
            DeviceStatus::Expired => "expired",
            DeviceStatus::Revoked => "revoked",
        }
    }
    pub fn parse(s: &str) -> Option<DeviceStatus> {
        Some(match s {
            "pending" => DeviceStatus::Pending,
            "approved" => DeviceStatus::Approved,
            "denied" => DeviceStatus::Denied,
            "expired" => DeviceStatus::Expired,
            "revoked" => DeviceStatus::Revoked,
            _ => return None,
        })
    }
}

/// Pairing code shown on her screen and in Hugo's approval, e.g. `KDF-427`.
/// Letters without I/O and digits without 0/1, so it reads unambiguously over the phone.
pub fn pairing_code() -> String {
    const L: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
    const D: &[u8] = b"23456789";
    let mut r = rand::thread_rng();
    let mut s = String::with_capacity(7);
    for _ in 0..3 {
        s.push(L[r.gen_range(0..L.len())] as char);
    }
    s.push('-');
    for _ in 0..3 {
        s.push(D[r.gen_range(0..D.len())] as char);
    }
    s
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UsageLevel {
    Green,
    Amber,
    Red,
}

/// Colour of the usage pill for a month's spend against the cap.
pub fn usage_level(spent_eur: f64, cap_eur: f64) -> UsageLevel {
    if cap_eur <= 0.0 {
        return UsageLevel::Red;
    }
    let f = spent_eur / cap_eur;
    if f > RED_ABOVE {
        UsageLevel::Red
    } else if f >= AMBER_FROM {
        UsageLevel::Amber
    } else {
        UsageLevel::Green
    }
}

/// What STARK reads every 60 s: the pill and the pairings waiting for Hugo.
/// A fixed allowlist of fields; nothing about her profile or conversations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StarkStatus {
    pub month: String,
    pub spent_eur: f64,
    pub cap_eur: f64,
    pub level: UsageLevel,
    pub pending: Vec<PendingPairing>,
    pub devices: Vec<DeviceSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingPairing {
    pub id: uuid::Uuid,
    pub label: String,
    pub code: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceSummary {
    pub id: uuid::Uuid,
    pub label: String,
    pub status: DeviceStatus,
    pub last_seen: Option<chrono::DateTime<chrono::Utc>>,
}

/// Who a model call is for. Her own work may run to 110 % of the cap so a conversation
/// is never cut off mid-sentence; background jobs stop at 100 % (spec §10b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Hers,
    Background,
}

/// Her lane may overrun the cap by this fraction (spec §10b: €33 on a €30 cap).
pub const HER_OVERRUN: f64 = 0.10;

/// Pre-call check: would `estimate_eur` on top of `spent_eur` stay inside the lane's limit?
pub fn may_spend(spent_eur: f64, estimate_eur: f64, cap_eur: f64, lane: Lane) -> bool {
    let limit = match lane {
        Lane::Hers => cap_eur * (1.0 + HER_OVERRUN),
        Lane::Background => cap_eur,
    };
    cap_eur > 0.0 && spent_eur + estimate_eur <= limit
}

/// One fact Kompas read in a document: what it says and the exact words it rests on.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct DocFact {
    pub kind: String,
    pub text: String,
    pub snippet: String,
}

/// Honesty by code, not by prompt: keeps a fact only when its snippet occurs verbatim in the
/// document text (whitespace folded, since PDF text breaks lines freely). Returns the kept facts
/// and how many were dropped.
pub fn guard_facts(text: &str, facts: Vec<DocFact>) -> (Vec<DocFact>, usize) {
    let (mut kept, mut dropped) = (Vec::new(), 0);
    for f in facts {
        if !f.text.trim().is_empty() && quote_in(text, &f.snippet) {
            kept.push(f);
        } else {
            dropped += 1;
        }
    }
    (kept, dropped)
}

/// Does `snippet` occur verbatim in `text` (whitespace folded)? An empty snippet never does.
pub fn quote_in(text: &str, snippet: &str) -> bool {
    let needle = fold_ws(snippet);
    !needle.is_empty() && fold_ws(text).contains(&needle)
}

fn fold_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_shape() {
        for _ in 0..200 {
            let c = pairing_code();
            assert_eq!(c.len(), 7);
            assert_eq!(&c[3..4], "-");
            assert!(!c.contains(['I', 'O', '0', '1']));
        }
    }

    #[test]
    fn pill_thresholds() {
        assert_eq!(usage_level(4.20, 30.0), UsageLevel::Green);
        assert_eq!(usage_level(18.0, 30.0), UsageLevel::Amber); // exactly 60 %
        assert_eq!(usage_level(27.0, 30.0), UsageLevel::Amber); // exactly 90 %
        assert_eq!(usage_level(27.1, 30.0), UsageLevel::Red);
        assert_eq!(usage_level(1.0, 0.0), UsageLevel::Red);
    }

    #[test]
    fn cap_lanes() {
        assert!(may_spend(29.0, 0.5, 30.0, Lane::Background));
        assert!(!may_spend(29.9, 0.2, 30.0, Lane::Background));
        assert!(may_spend(32.5, 0.4, 30.0, Lane::Hers));
        assert!(!may_spend(32.9, 0.2, 30.0, Lane::Hers));
        assert!(!may_spend(0.0, 0.01, 0.0, Lane::Hers));
    }

    #[test]
    fn status_round_trip() {
        for s in ["pending", "approved", "denied", "expired", "revoked"] {
            assert_eq!(DeviceStatus::parse(s).unwrap().as_str(), s);
        }
        assert!(DeviceStatus::parse("x").is_none());
    }

    #[test]
    fn guard_drops_a_fact_whose_quote_is_not_in_the_text() {
        let text = "Curriculum vitae
Projectleider bij Voorbeeld BV,
2015-2020.";
        let f = |t: &str, q: &str| DocFact {
            kind: "role".into(),
            text: t.into(),
            snippet: q.into(),
        };
        let facts = vec![
            // A Dutch CV: the quote stays in its language, the fact is in English.
            f(
                "Project lead at Voorbeeld BV, 2015-2020",
                "Projectleider bij Voorbeeld BV, 2015-2020.",
            ),
            f("Director at Nowhere Ltd", "Director at Nowhere Ltd"),
            f("Empty quote", "   "),
        ];
        let (kept, dropped) = guard_facts(text, facts);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].text, "Project lead at Voorbeeld BV, 2015-2020");
        assert_eq!(dropped, 2);
    }
}
