//! Career Compass domain types shared by the api, the worker and the desktop app.
//! Numbers here come from the build spec v2 (§2b pairing, §10b cost cap).

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
    fn status_round_trip() {
        for s in ["pending", "approved", "denied", "expired", "revoked"] {
            assert_eq!(DeviceStatus::parse(s).unwrap().as_str(), s);
        }
        assert!(DeviceStatus::parse("x").is_none());
    }
}
