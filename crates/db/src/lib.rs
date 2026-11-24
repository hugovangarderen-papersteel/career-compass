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
    let row = sqlx::query("SELECT id, email, role, password_hash FROM users WHERE email = $1")
        .bind(email.trim().to_lowercase())
        .fetch_optional(pool)
        .await?;
    Ok(row.map(user_from_row))
}

pub async fn user_by_id(pool: &PgPool, id: Uuid) -> Result<Option<User>, Error> {
    let row = sqlx::query("SELECT id, email, role, password_hash FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(user_from_row))
}

fn user_from_row(r: sqlx::postgres::PgRow) -> User {
    User {
        id: r.get("id"),
        email: r.get("email"),
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

pub async fn log_llm_call(
    pool: &PgPool,
    purpose: &str,
    model: &str,
    tokens_in: i32,
    tokens_out: i32,
    cost_eur: f64,
) -> Result<(), Error> {
    sqlx::query(
        "INSERT INTO llm_calls (id, purpose, model, tokens_in, tokens_out, cost_eur) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(Uuid::new_v4())
    .bind(purpose)
    .bind(model)
    .bind(tokens_in)
    .bind(tokens_out)
    .bind(cost_eur)
    .execute(pool)
    .await?;
    Ok(())
}
