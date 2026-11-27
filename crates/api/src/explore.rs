//! Phase 5: the companies grid and map, her own ranking, stories and media, and her display
//! settings. The grid and map get travel times only, never the home coordinate.

use crate::intake::who;
use crate::matching::{locked, unlocked};
use crate::{ApiError, ApiResult, AppState};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use cc_core::company::headcount_number;
use cc_core::grid::{clean_ranking, keep, order, ring, ring_counts, size_band, Filter, Row};
use cc_core::matching::DEFAULT_TRAVEL_MIN;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row as _;
use std::collections::{BTreeSet, HashMap, HashSet};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/matching/grid", post(grid))
        .route("/matching/ranking", get(ranking).post(save_ranking))
        .route("/matching/stories", get(stories))
        .route("/settings", get(settings).post(save_settings))
        .route("/settings/digest-seen", post(digest_seen))
}

async fn max_travel(st: &AppState, user: Uuid) -> ApiResult<u32> {
    let m: Option<i64> =
        sqlx::query_scalar("SELECT (config->>'max_travel_min')::bigint FROM match_configs WHERE user_id = $1")
            .bind(user)
            .fetch_optional(&st.pool)
            .await?
            .flatten();
    Ok(m.and_then(|m| u32::try_from(m).ok()).unwrap_or(DEFAULT_TRAVEL_MIN))
}

async fn ranked_ids(st: &AppState, user: Uuid) -> ApiResult<Vec<Uuid>> {
    Ok(
        sqlx::query_scalar("SELECT company_id FROM user_ranking WHERE user_id = $1 ORDER BY position")
            .bind(user)
            .fetch_all(&st.pool)
            .await?,
    )
}

/// Every company still in the base, with her fit, ring, size band and role forms.
async fn rows(st: &AppState, user: Uuid, max: u32) -> ApiResult<Vec<Row>> {
    let rank: HashMap<Uuid, usize> = ranked_ids(st, user)
        .await?
        .into_iter()
        .enumerate()
        .map(|(i, id)| (id, i))
        .collect();
    let heads = sqlx::query(
        "SELECT DISTINCT ON (company_id) company_id, value FROM company_facts \
         WHERE field = 'headcount' ORDER BY company_id, fetched_at DESC",
    )
    .fetch_all(&st.pool)
    .await?;
    let heads: HashMap<Uuid, f64> = heads
        .iter()
        .filter_map(|r| Some((r.get("company_id"), headcount_number(r.get("value"))?)))
        .collect();
    let db = sqlx::query(
        "SELECT c.id, c.name, c.subsector, c.city, c.lat, c.lon, f.score, f.excluded, f.travel_min, \
         f.axes->'forms' AS forms FROM companies c \
         LEFT JOIN fit_scores f ON f.company_id = c.id AND f.user_id = $1",
    )
    .bind(user)
    .fetch_all(&st.pool)
    .await?;
    Ok(db
        .iter()
        .map(|r| {
            let id: Uuid = r.get("id");
            let travel = r
                .get::<Option<i32>, _>("travel_min")
                .and_then(|t| u32::try_from(t).ok());
            Row {
                id,
                name: r.get("name"),
                sector: r.get("subsector"),
                city: r.get("city"),
                size: size_band(heads.get(&id).copied()).into(),
                ring: ring(travel, max),
                travel_min: travel,
                forms: r
                    .get::<Option<Value>, _>("forms")
                    .and_then(|v| serde_json::from_value(v).ok()),
                score: r.get::<Option<f64>, _>("score").map(f64::round),
                excluded: r.get("excluded"),
                rank: rank.get(&id).copied(),
                lat: r.get("lat"),
                lon: r.get("lon"),
            }
        })
        .collect())
}

#[derive(Deserialize, Default)]
struct GridIn {
    #[serde(default)]
    filter: Filter,
}

async fn grid(State(st): State<AppState>, h: HeaderMap, Json(b): Json<GridIn>) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    let max = max_travel(&st, user).await?;
    let all = rows(&st, user, max).await?;
    let (inside, outside, unknown) = ring_counts(&all);
    let sectors: BTreeSet<String> = all.iter().filter_map(|r| r.sector.clone()).collect();
    let mut shown: Vec<Row> = all.iter().filter(|r| keep(&b.filter, r)).cloned().collect();
    order(&mut shown);
    Ok(Json(json!({
        "locked": false,
        "max_travel_min": max,
        "counts": {"inside": inside, "outside": outside, "unknown": unknown,
                   "shown": shown.len(), "total": all.len()},
        "sectors": sectors,
        "rows": shown,
    })))
}

async fn ranking(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    let max = max_travel(&st, user).await?;
    let mut mine: Vec<Row> = rows(&st, user, max)
        .await?
        .into_iter()
        .filter(|r| r.rank.is_some())
        .collect();
    order(&mut mine);
    Ok(Json(json!({"locked": false, "rows": mine})))
}

#[derive(Deserialize)]
struct RankingIn {
    company_ids: Vec<Uuid>,
}

/// Replaces her whole ranking: the order she sends is the order she gets back.
async fn save_ranking(State(st): State<AppState>, h: HeaderMap, Json(b): Json<RankingIn>) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Err(ApiError::Conflict("not_approved"));
    };
    let known: HashSet<Uuid> = sqlx::query_scalar("SELECT id FROM companies WHERE id = ANY($1)")
        .bind(&b.company_ids)
        .fetch_all(&st.pool)
        .await?
        .into_iter()
        .collect();
    let ids = clean_ranking(&b.company_ids, &known);
    let mut tx = st.pool.begin().await?;
    sqlx::query("DELETE FROM user_ranking WHERE user_id = $1")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    for (i, id) in ids.iter().enumerate() {
        sqlx::query("INSERT INTO user_ranking (user_id, company_id, position) VALUES ($1, $2, $3)")
            .bind(user)
            .bind(id)
            .bind(i as i32)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"ok": true, "company_ids": ids})))
}

/// News and media the enrichment already found, newest first, each with its source.
async fn stories(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    if unlocked(&st, &h).await?.is_none() {
        return Ok(locked());
    }
    let ev = sqlx::query(
        "SELECT e.company_id, c.name, e.date, e.kind, e.title, e.snippet, e.source_url FROM company_events e \
         JOIN companies c ON c.id = e.company_id ORDER BY e.date DESC, c.name LIMIT 100",
    )
    .fetch_all(&st.pool)
    .await?;
    let items: Vec<Value> = ev
        .iter()
        .map(|r| {
            json!({
                "company_id": r.get::<Uuid, _>("company_id"),
                "name": r.get::<String, _>("name"),
                "date": r.get::<String, _>("date"),
                "kind": r.get::<String, _>("kind"),
                "title": r.get::<String, _>("title"),
                "snippet": r.get::<String, _>("snippet"),
                "source_url": r.get::<String, _>("source_url"),
            })
        })
        .collect();
    Ok(Json(json!({"locked": false, "items": items})))
}

/// Her display settings and whether a digest is waiting. Not locked: she can set the text
/// size before she has a profile.
async fn settings(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?.id;
    let s = sqlx::query("SELECT font_step, high_contrast, digest_seen_at FROM user_settings WHERE user_id = $1")
        .bind(user)
        .fetch_optional(&st.pool)
        .await?;
    let digest_at: Option<DateTime<Utc>> = sqlx::query_scalar("SELECT max(created_at) FROM digests WHERE user_id = $1")
        .bind(user)
        .fetch_one(&st.pool)
        .await?;
    let seen = s
        .as_ref()
        .and_then(|s| s.get::<Option<DateTime<Utc>>, _>("digest_seen_at"));
    Ok(Json(json!({
        "font_step": s.as_ref().map_or(0, |s| s.get::<i16, _>("font_step")),
        "high_contrast": s.as_ref().is_some_and(|s| s.get::<bool, _>("high_contrast")),
        "digest_at": digest_at,
        "digest_unseen": digest_at.is_some_and(|d| seen.is_none_or(|s| s < d)),
    })))
}

#[derive(Deserialize)]
struct SettingsIn {
    font_step: Option<i16>,
    high_contrast: Option<bool>,
}

async fn save_settings(State(st): State<AppState>, h: HeaderMap, Json(b): Json<SettingsIn>) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?.id;
    if b.font_step.is_some_and(|f| !(0..=2).contains(&f)) {
        return Err(ApiError::BadRequest("font_step"));
    }
    sqlx::query(
        "INSERT INTO user_settings (user_id, font_step, high_contrast) \
         VALUES ($1, COALESCE($2, 0), COALESCE($3, false)) \
         ON CONFLICT (user_id) DO UPDATE SET font_step = COALESCE($2, user_settings.font_step), \
         high_contrast = COALESCE($3, user_settings.high_contrast), updated_at = now()",
    )
    .bind(user)
    .bind(b.font_step)
    .bind(b.high_contrast)
    .execute(&st.pool)
    .await?;
    settings(State(st), h).await
}

async fn digest_seen(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?.id;
    sqlx::query(
        "INSERT INTO user_settings (user_id, digest_seen_at) VALUES ($1, now()) \
         ON CONFLICT (user_id) DO UPDATE SET digest_seen_at = now(), updated_at = now()",
    )
    .bind(user)
    .execute(&st.pool)
    .await?;
    Ok(Json(json!({"ok": true})))
}
