//! Queries and migrations. Runtime-checked queries (no database needed to compile).

use cc_core::{DeviceStatus, DeviceSummary, PendingPairing, Role};
use chrono::{DateTime, Duration, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;
use uuid::Uuid;

pub use sqlx::Error;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(url: &str) -> Result<PgPool, Error> {
    PgPoolOptions::new().max_connections(5).connect(url).await
}

pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}

// ---------- users ----------

pub struct User {
    pub id: Uuid,
    pub email: String,
    pub role: Role,
    pub password_hash: Option<String>,
    /// Hugo's test accounts: may reset their intake. Real accounts never can.
    pub is_test: bool,
}

pub async fn create_user(pool: &PgPool, email: &str, role: Role) -> Result<Uuid, Error> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO users (id, email, role) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(email.trim().to_lowercase())
        .bind(role.as_str())
        .execute(pool)
        .await?;
    Ok(id)
}

pub async fn user_by_email(pool: &PgPool, email: &str) -> Result<Option<User>, Error> {
    let row = sqlx::query("SELECT id, email, role, password_hash, is_test FROM users WHERE email = $1")
        .bind(email.trim().to_lowercase())
        .fetch_optional(pool)
        .await?;
    Ok(row.map(user_from_row))
}

pub async fn user_by_id(pool: &PgPool, id: Uuid) -> Result<Option<User>, Error> {
    let row = sqlx::query("SELECT id, email, role, password_hash, is_test FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(user_from_row))
}

fn user_from_row(r: sqlx::postgres::PgRow) -> User {
    User {
        id: r.get("id"),
        email: r.get("email"),
        is_test: r.get("is_test"),
        role: Role::parse(r.get::<&str, _>("role")).unwrap_or(Role::Seeker),
        password_hash: r.get("password_hash"),
    }
}

/// Sets the password only if none is set yet; returns false otherwise.
pub async fn set_first_password(pool: &PgPool, user: Uuid, hash: &str) -> Result<bool, Error> {
    let r = sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1 AND password_hash IS NULL")
        .bind(user)
        .bind(hash)
        .execute(pool)
        .await?;
    Ok(r.rows_affected() == 1)
}

// ---------- devices / pairing ----------

pub struct Device {
    pub id: Uuid,
    pub label: String,
    pub public_key: Vec<u8>,
    pub code: String,
    pub status: DeviceStatus,
    pub expires_at: DateTime<Utc>,
    pub claimed: bool,
}

pub async fn create_pairing(
    pool: &PgPool,
    label: &str,
    public_key: &[u8],
    code: &str,
) -> Result<(Uuid, DateTime<Utc>), Error> {
    let id = Uuid::new_v4();
    let expires = Utc::now() + Duration::minutes(cc_core::PAIRING_TTL_MINUTES);
    sqlx::query(
        "INSERT INTO devices (id, label, public_key, code, status, expires_at) VALUES ($1, $2, $3, $4, 'pending', $5)",
    )
    .bind(id)
    .bind(label)
    .bind(public_key)
    .bind(code)
    .bind(expires)
    .execute(pool)
    .await?;
    Ok((id, expires))
}

/// Pending requests whose 15 minutes are over become `expired`.
pub async fn expire_pairings(pool: &PgPool) -> Result<u64, Error> {
    let r = sqlx::query("UPDATE devices SET status = 'expired' WHERE status = 'pending' AND expires_at < now()")
        .execute(pool)
        .await?;
    Ok(r.rows_affected())
}

pub async fn device(pool: &PgPool, id: Uuid) -> Result<Option<Device>, Error> {
    expire_pairings(pool).await?;
    let row = sqlx::query("SELECT id, label, public_key, code, status, expires_at, claimed FROM devices WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| Device {
        id: r.get("id"),
        label: r.get("label"),
        public_key: r.get("public_key"),
        code: r.get("code"),
        status: DeviceStatus::parse(r.get::<&str, _>("status")).unwrap_or(DeviceStatus::Denied),
        expires_at: r.get("expires_at"),
        claimed: r.get("claimed"),
    }))
}

/// Hugo's decision. Only a pending, unexpired request can be decided; returns false otherwise.
pub async fn decide_pairing(pool: &PgPool, id: Uuid, approve: bool) -> Result<bool, Error> {
    expire_pairings(pool).await?;
    let to = if approve { "approved" } else { "denied" };
    let r = sqlx::query("UPDATE devices SET status = $2, decided_at = now() WHERE id = $1 AND status = 'pending'")
        .bind(id)
        .bind(to)
        .execute(pool)
        .await?;
    Ok(r.rows_affected() == 1)
}

/// Stores the token hash once, for an approved device that has not claimed yet.
pub async fn claim_token(pool: &PgPool, id: Uuid, token_hash: &[u8]) -> Result<bool, Error> {
    let r = sqlx::query("UPDATE devices SET token_hash = $2, claimed = true, last_seen = now() WHERE id = $1 AND status = 'approved' AND NOT claimed")
        .bind(id)
        .bind(token_hash)
        .execute(pool)
        .await?;
    Ok(r.rows_affected() == 1)
}

/// The approved device a token belongs to; bumps last_seen.
pub async fn device_by_token(pool: &PgPool, token_hash: &[u8]) -> Result<Option<Uuid>, Error> {
    let row =
        sqlx::query("UPDATE devices SET last_seen = now() WHERE token_hash = $1 AND status = 'approved' RETURNING id")
            .bind(token_hash)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|r| r.get("id")))
}

pub async fn revoke_device(pool: &PgPool, id: Uuid) -> Result<bool, Error> {
    let r = sqlx::query(
        "UPDATE devices SET status = 'revoked', decided_at = now() WHERE id = $1 AND status IN ('approved', 'pending')",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() == 1)
}

pub async fn pending_pairings(pool: &PgPool) -> Result<Vec<PendingPairing>, Error> {
    expire_pairings(pool).await?;
    let rows =
        sqlx::query("SELECT id, label, code, expires_at FROM devices WHERE status = 'pending' ORDER BY created_at")
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|r| PendingPairing {
            id: r.get("id"),
            label: r.get("label"),
            code: r.get("code"),
            expires_at: r.get("expires_at"),
        })
        .collect())
}

pub async fn devices(pool: &PgPool) -> Result<Vec<DeviceSummary>, Error> {
    let rows = sqlx::query(
        "SELECT id, label, status, last_seen FROM devices WHERE status IN ('approved', 'revoked') ORDER BY created_at",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| DeviceSummary {
            id: r.get("id"),
            label: r.get("label"),
            status: DeviceStatus::parse(r.get::<&str, _>("status")).unwrap_or(DeviceStatus::Revoked),
            last_seen: r.get("last_seen"),
        })
        .collect())
}

// ---------- cost ----------

/// EUR spent on model calls in the current calendar month (UTC).
pub async fn month_spend(pool: &PgPool) -> Result<f64, Error> {
    let row = sqlx::query(
        "SELECT COALESCE(SUM(cost_eur), 0)::float8 AS s FROM llm_calls WHERE created_at >= date_trunc('month', now())",
    )
    .fetch_one(pool)
    .await?;
    Ok(row.get("s"))
}

pub async fn cap_eur(pool: &PgPool) -> Result<f64, Error> {
    let row = sqlx::query("SELECT cap_eur FROM settings").fetch_one(pool).await?;
    Ok(row.get("cap_eur"))
}

pub async fn set_cap_eur(pool: &PgPool, cap: f64) -> Result<(), Error> {
    sqlx::query("UPDATE settings SET cap_eur = $1")
        .bind(cap)
        .execute(pool)
        .await?;
    Ok(())
}

/// One metered model call (spec §10b). `tokens_in` counts every input token, cached or not.
pub struct LlmCall<'a> {
    pub purpose: &'a str,
    pub model: &'a str,
    pub tokens_in: i32,
    pub tokens_out: i32,
    pub cache_read: i32,
    pub cache_write: i32,
    pub cost_eur: f64,
}

pub async fn log_llm_call(pool: &PgPool, c: &LlmCall<'_>) -> Result<(), Error> {
    sqlx::query(
        "INSERT INTO llm_calls (id, purpose, model, tokens_in, tokens_out, cache_read, cache_write, cost_eur) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(Uuid::new_v4())
    .bind(c.purpose)
    .bind(c.model)
    .bind(c.tokens_in)
    .bind(c.tokens_out)
    .bind(c.cache_read)
    .bind(c.cache_write)
    .bind(c.cost_eur)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn mark_test_user(pool: &PgPool, user: Uuid) -> Result<(), Error> {
    sqlx::query("UPDATE users SET is_test = true WHERE id = $1")
        .bind(user)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------- intake (spec §3) ----------

pub struct IntakeSession {
    pub id: Uuid,
    pub phase: i16,
    pub phases_done: Vec<i16>,
    pub status: String,
}

/// Her latest intake, if she ever started one.
pub async fn current_session(pool: &PgPool, user: Uuid) -> Result<Option<IntakeSession>, Error> {
    let row = sqlx::query(
        "SELECT id, phase, phases_done, status FROM intake_sessions WHERE user_id = $1 ORDER BY started_at DESC LIMIT 1",
    )
    .bind(user)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| IntakeSession {
        id: r.get("id"),
        phase: r.get("phase"),
        phases_done: r.get("phases_done"),
        status: r.get("status"),
    }))
}

pub async fn create_session(pool: &PgPool, user: Uuid) -> Result<IntakeSession, Error> {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO intake_sessions (id, user_id) VALUES ($1, $2)")
        .bind(id)
        .bind(user)
        .execute(pool)
        .await?;
    Ok(IntakeSession {
        id,
        phase: 1,
        phases_done: vec![],
        status: "active".into(),
    })
}

pub async fn save_session(pool: &PgPool, s: &IntakeSession) -> Result<(), Error> {
    sqlx::query(
        "UPDATE intake_sessions SET phase = $2, phases_done = $3, status = $4, updated_at = now(), \
         ended_at = CASE WHEN $4 = 'approved' THEN now() ELSE ended_at END WHERE id = $1",
    )
    .bind(s.id)
    .bind(s.phase)
    .bind(&s.phases_done)
    .bind(&s.status)
    .execute(pool)
    .await?;
    Ok(())
}

pub struct Turn {
    pub seq: i32,
    pub speaker: String,
    pub phase: i16,
    pub text: String,
}

pub async fn turns(pool: &PgPool, session: Uuid) -> Result<Vec<Turn>, Error> {
    let rows = sqlx::query("SELECT seq, speaker, phase, text FROM transcript_turns WHERE session_id = $1 ORDER BY seq")
        .bind(session)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| Turn {
            seq: r.get("seq"),
            speaker: r.get("speaker"),
            phase: r.get("phase"),
            text: r.get("text"),
        })
        .collect())
}

/// Appends a turn and returns its number (the `T<n>` profile fields point at).
pub async fn add_turn(pool: &PgPool, session: Uuid, speaker: &str, phase: i16, text: &str) -> Result<i32, Error> {
    let row = sqlx::query(
        "INSERT INTO transcript_turns (id, session_id, seq, speaker, phase, text) \
         SELECT $1, $2, COALESCE(MAX(seq), 0) + 1, $3, $4, $5 FROM transcript_turns WHERE session_id = $2 \
         RETURNING seq",
    )
    .bind(Uuid::new_v4())
    .bind(session)
    .bind(speaker)
    .bind(phase)
    .bind(text)
    .fetch_one(pool)
    .await?;
    Ok(row.get("seq"))
}

/// Test accounts only (the caller checks `is_test`): wipes intake, profiles and documents.
pub async fn reset_intake(pool: &PgPool, user: Uuid) -> Result<(), Error> {
    let mut tx = pool.begin().await?;
    for q in [
        "DELETE FROM profiles WHERE user_id = $1",
        "DELETE FROM intake_sessions WHERE user_id = $1",
        "DELETE FROM documents WHERE user_id = $1",
    ] {
        sqlx::query(q).bind(user).execute(&mut *tx).await?;
    }
    tx.commit().await
}

pub struct ProfileRow {
    pub version: i32,
    pub status: String,
    pub data: serde_json::Value,
}

pub async fn latest_profile(pool: &PgPool, user: Uuid) -> Result<Option<ProfileRow>, Error> {
    let row =
        sqlx::query("SELECT version, status, data FROM profiles WHERE user_id = $1 ORDER BY version DESC LIMIT 1")
            .bind(user)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|r| ProfileRow {
        version: r.get("version"),
        status: r.get("status"),
        data: r.get("data"),
    }))
}

/// Writes the draft: updates the open draft, or opens a new version after an approved one.
pub async fn save_draft(pool: &PgPool, user: Uuid, session: Uuid, data: &serde_json::Value) -> Result<i32, Error> {
    let mut tx = pool.begin().await?;
    let latest =
        sqlx::query("SELECT version, status FROM profiles WHERE user_id = $1 ORDER BY version DESC LIMIT 1 FOR UPDATE")
            .bind(user)
            .fetch_optional(&mut *tx)
            .await?;
    let version = match latest {
        Some(r) if r.get::<&str, _>("status") == "draft" => {
            let v: i32 = r.get("version");
            sqlx::query("UPDATE profiles SET data = $3, session_id = $4 WHERE user_id = $1 AND version = $2")
                .bind(user)
                .bind(v)
                .bind(data)
                .bind(session)
                .execute(&mut *tx)
                .await?;
            v
        }
        other => {
            let v = other.map(|r| r.get::<i32, _>("version") + 1).unwrap_or(1);
            sqlx::query("INSERT INTO profiles (id, user_id, session_id, version, data) VALUES ($1, $2, $3, $4, $5)")
                .bind(Uuid::new_v4())
                .bind(user)
                .bind(session)
                .bind(v)
                .bind(data)
                .execute(&mut *tx)
                .await?;
            v
        }
    };
    tx.commit().await?;
    Ok(version)
}

/// Locks the open draft; returns its version, or None when there is no draft.
pub async fn approve_profile(pool: &PgPool, user: Uuid) -> Result<Option<i32>, Error> {
    let row = sqlx::query(
        "UPDATE profiles SET status = 'approved', approved_at = now() \
         WHERE user_id = $1 AND status = 'draft' RETURNING version",
    )
    .bind(user)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| r.get("version")))
}

pub struct Document {
    pub id: Uuid,
    pub seq: i32,
    pub name: String,
    pub body: String,
    pub status: String,
    /// Facts the model offered whose quote was not in the text (dropped by the snippet guard).
    pub dropped: i32,
}

/// One fact read from one of her documents; nothing reaches Kompas until she confirms it.
pub struct Fact {
    pub id: Uuid,
    pub document_id: Uuid,
    pub kind: String,
    pub text: String,
    pub snippet: String,
    pub status: String,
}

/// Stores a document and the facts that passed the snippet guard, in one transaction.
pub async fn add_document(
    pool: &PgPool,
    user: Uuid,
    name: &str,
    body: &str,
    facts: &[cc_core::DocFact],
    dropped: i32,
) -> Result<Uuid, Error> {
    let id = Uuid::new_v4();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO documents (id, user_id, seq, name, body, dropped)          SELECT $1, $2, COALESCE(MAX(seq), 0) + 1, $3, $4, $5 FROM documents WHERE user_id = $2",
    )
    .bind(id)
    .bind(user)
    .bind(name)
    .bind(body)
    .bind(dropped)
    .execute(&mut *tx)
    .await?;
    for (i, f) in facts.iter().enumerate() {
        sqlx::query(
            "INSERT INTO document_facts (id, document_id, user_id, seq, kind, text, snippet)              VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(Uuid::new_v4())
        .bind(id)
        .bind(user)
        .bind(i as i32 + 1)
        .bind(&f.kind)
        .bind(&f.text)
        .bind(&f.snippet)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(id)
}

pub async fn documents(pool: &PgPool, user: Uuid) -> Result<Vec<Document>, Error> {
    let rows =
        sqlx::query("SELECT id, seq, name, body, status, dropped FROM documents WHERE user_id = $1 ORDER BY seq")
            .bind(user)
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .map(|r| Document {
            id: r.get("id"),
            seq: r.get("seq"),
            name: r.get("name"),
            body: r.get("body"),
            status: r.get("status"),
            dropped: r.get("dropped"),
        })
        .collect())
}

pub async fn document_facts(pool: &PgPool, user: Uuid) -> Result<Vec<Fact>, Error> {
    let rows = sqlx::query(
        "SELECT id, document_id, kind, text, snippet, status FROM document_facts          WHERE user_id = $1 ORDER BY document_id, seq",
    )
    .bind(user)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Fact {
            id: r.get("id"),
            document_id: r.get("document_id"),
            kind: r.get("kind"),
            text: r.get("text"),
            snippet: r.get("snippet"),
            status: r.get("status"),
        })
        .collect())
}

/// Confirms, corrects (new text, confirmed) or rejects one of HER facts; false when it is not hers.
pub async fn set_fact(pool: &PgPool, user: Uuid, id: Uuid, status: &str, text: Option<&str>) -> Result<bool, Error> {
    let r =
        sqlx::query("UPDATE document_facts SET status = $3, text = COALESCE($4, text) WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user)
            .bind(status)
            .bind(text)
            .execute(pool)
            .await?;
    Ok(r.rows_affected() == 1)
}
