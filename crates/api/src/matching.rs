//! Matching (Phase 3): the weekly 5, the ranked list, the company page with its fit, and
//! thumbs. Everything is locked until she approves a profile: no empty or made-up lists.

use crate::intake::who;
use crate::{ApiError, ApiResult, AppState};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use cc_core::matching::{top_axis, Axis, AXES};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/matching", get(overview))
        .route("/matching/companies/{id}", get(company))
        .route("/matching/feedback", post(feedback))
}

const LOCKED: &str = "Unlocks when you approve your profile";

/// Some(user id) once a profile is approved; None means the screens stay locked.
pub(crate) async fn unlocked(st: &AppState, h: &HeaderMap) -> ApiResult<Option<Uuid>> {
    let user = who(st, h).await?;
    let ok: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM profiles WHERE user_id = $1 AND status = 'approved')")
            .bind(user.id)
            .fetch_one(&st.pool)
            .await?;
    Ok(ok.then_some(user.id))
}

pub(crate) fn locked() -> Json<Value> {
    Json(json!({"locked": true, "message": LOCKED}))
}

async fn overview(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    let digest =
        sqlx::query("SELECT items, created_at FROM digests WHERE user_id = $1 ORDER BY created_at DESC LIMIT 1")
            .bind(user)
            .fetch_optional(&st.pool)
            .await?;
    let ranked = sqlx::query(
        "SELECT f.company_id, c.name, c.subsector, f.score, f.travel_min FROM fit_scores f \
         JOIN companies c ON c.id = f.company_id WHERE f.user_id = $1 AND f.score IS NOT NULL \
         ORDER BY f.score DESC LIMIT 50",
    )
    .bind(user)
    .fetch_all(&st.pool)
    .await?;
    let ranked: Vec<Value> = ranked
        .iter()
        .map(|r| {
            json!({
                "company_id": r.get::<Uuid, _>("company_id"),
                "name": r.get::<String, _>("name"),
                "subsector": r.get::<Option<String>, _>("subsector"),
                "score": r.get::<Option<f64>, _>("score").map(f64::round),
                "travel_min": r.get::<Option<i32>, _>("travel_min"),
            })
        })
        .collect();
    Ok(Json(json!({
        "locked": false,
        "digest": digest.as_ref().map(|d| d.get::<Value, _>("items")),
        "digest_at": digest.as_ref().map(|d| d.get::<chrono::DateTime<Utc>, _>("created_at")),
        "ranked": ranked,
        // Scoring runs in the background after approval; say so instead of showing nothing.
        "scoring": ranked.is_empty(),
    })))
}

async fn company(State(st): State<AppState>, h: HeaderMap, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    let mut v = cc_db::company_detail(&st.pool, id).await?.ok_or(ApiError::NotFound)?;
    let fit =
        sqlx::query("SELECT score, excluded, axes, travel_min FROM fit_scores WHERE user_id = $1 AND company_id = $2")
            .bind(user)
            .bind(id)
            .fetch_optional(&st.pool)
            .await?;
    v["locked"] = json!(false);
    v["fit"] = fit.map_or(Value::Null, |f| {
        json!({
            "score": f.get::<Option<f64>, _>("score").map(f64::round),
            "excluded": f.get::<Option<String>, _>("excluded"),
            "axes": f.get::<Value, _>("axes"),
            "travel_min": f.get::<Option<i32>, _>("travel_min"),
        })
    });
    let thumb = sqlx::query(
        "SELECT up, reason FROM feedback WHERE user_id = $1 AND company_id = $2 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user)
    .bind(id)
    .fetch_optional(&st.pool)
    .await?;
    v["thumb"] = thumb.map_or(
        Value::Null,
        |t| json!({"up": t.get::<bool, _>("up"), "reason": t.get::<String, _>("reason")}),
    );
    Ok(Json(v))
}

#[derive(Deserialize)]
struct Thumb {
    company_id: Uuid,
    up: bool,
    axis: Option<String>,
    #[serde(default)]
    reason: String,
}

/// A thumb nudges one axis (the one she names, else the one that drove the score) and,
/// when down, its reason becomes a soft penalty. derive_config replays all of them.
async fn feedback(State(st): State<AppState>, h: HeaderMap, Json(b): Json<Thumb>) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Err(ApiError::Conflict("not_approved"));
    };
    let reason = b.reason.trim().chars().take(300).collect::<String>();
    if !b.up && reason.is_empty() {
        return Err(ApiError::BadRequest("reason"));
    }
    let axis = match b.axis {
        Some(a) if AXES.contains(&a.as_str()) => a,
        Some(_) => return Err(ApiError::BadRequest("axis")),
        None => {
            let axes: Option<Value> =
                sqlx::query_scalar("SELECT axes FROM fit_scores WHERE user_id = $1 AND company_id = $2")
                    .bind(user)
                    .bind(b.company_id)
                    .fetch_optional(&st.pool)
                    .await?;
            let axes: Vec<Axis> = axes
                .and_then(|a| serde_json::from_value(a["axes"].clone()).ok())
                .unwrap_or_default();
            top_axis(&axes).ok_or(ApiError::Conflict("not_scored"))?
        }
    };
    sqlx::query("INSERT INTO feedback (id, user_id, company_id, up, axis, reason) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(Uuid::new_v4())
        .bind(user)
        .bind(b.company_id)
        .bind(b.up)
        .bind(&axis)
        .bind(&reason)
        .execute(&st.pool)
        .await?;
    cc_db::enqueue(&st.pool, "derive_config", json!({"user_id": user}), Utc::now()).await?;
    Ok(Json(json!({"ok": true, "axis": axis})))
}
