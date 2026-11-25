//! The company base (Phase 2): add-by-URL for any signed-in user, the admin list and detail,
//! and re-enrich. Admin routes need the admin role; they stay apart from the pairing control routes.

use crate::intake::who;
use crate::{ApiError, ApiResult, AppState};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use cc_core::Role;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/companies", post(add))
        .route("/admin/companies", get(list))
        .route("/admin/companies/{id}", get(detail))
        .route("/admin/companies/{id}/enrich", post(enrich))
}

#[derive(Deserialize)]
struct AddUrl {
    url: String,
}

async fn add(State(st): State<AppState>, h: HeaderMap, Json(b): Json<AddUrl>) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let site = cc_core::company::site_root(&b.url).ok_or(ApiError::BadRequest("bad_url"))?;
    // Named after its host until its own pages name it.
    let host = site
        .split("://")
        .nth(1)
        .unwrap_or(&site)
        .trim_end_matches('/')
        .to_string();
    let (id, new) = cc_db::add_company(&st.pool, &host, &site, Some(user.id)).await?;
    Ok(Json(json!({"id": id, "website": site, "new": new})))
}

async fn admin(st: &AppState, h: &HeaderMap) -> ApiResult<()> {
    match who(st, h).await?.role {
        Role::Admin => Ok(()),
        _ => Err(ApiError::Forbidden),
    }
}

async fn list(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    admin(&st, &h).await?;
    Ok(Json(json!({
        "companies": cc_db::company_list(&st.pool).await?,
        "spend_7d_eur": cc_db::spend_since(&st.pool, "enrichment", 7).await?,
        "spend_30d_eur": cc_db::spend_since(&st.pool, "enrichment", 30).await?,
    })))
}

async fn detail(State(st): State<AppState>, h: HeaderMap, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    admin(&st, &h).await?;
    let mut v = cc_db::company_detail(&st.pool, id).await?.ok_or(ApiError::NotFound)?;
    // Strength is read-time: the same signal weakens as it ages.
    let today = chrono::Utc::now().date_naive();
    if let Some(signals) = v["signals"].as_array_mut() {
        for s in signals {
            let kind = s["kind"].as_str().unwrap_or("").to_string();
            let dated = s["dated"].as_str().and_then(cc_core::company::parse_date);
            s["strength"] = json!(dated.map_or(0.0, |d| cc_core::company::strength(&kind, d, today)));
        }
    }
    Ok(Json(v))
}

async fn enrich(State(st): State<AppState>, h: HeaderMap, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    admin(&st, &h).await?;
    cc_db::company_detail(&st.pool, id).await?.ok_or(ApiError::NotFound)?;
    cc_db::enqueue_enrich(&st.pool, id, true).await?;
    Ok(Json(json!({"queued": true})))
}
