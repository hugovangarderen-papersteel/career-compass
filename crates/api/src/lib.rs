//! Career Compass HTTP api.
//!
//! Three kinds of caller, each with its own credential:
//! - her app before approval: nothing but the pairing endpoints;
//! - her app after approval: device token (`X-Device-Token`) on every call, plus her session JWT;
//! - STARK: `Authorization: Bearer <CC_ADMIN_TOKEN>` on `/stark/*`, which returns only
//!   pairing, device and spend fields (never profile or conversation data).

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use cc_core::{DeviceStatus, StarkStatus};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// At most this many pairing requests wait at once, so a flood cannot spam Hugo's phone.
pub const MAX_PENDING: usize = 3;
pub const MIN_PASSWORD_LEN: usize = 8;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt_secret: Vec<u8>,
    pub admin_token: String,
    /// None when no model key is configured: Kompas then answers 503 `kompas_offline`.
    pub llm: Option<cc_llm::Client>,
    /// USD to EUR for the meter. 1.0 over-reads the spend, which keeps the cap conservative.
    pub eur_per_usd: f64,
    /// The phone web build (trunk dist), served at `/app/` when set.
    pub web_dir: Option<std::path::PathBuf>,
    /// Kompas answers test accounts only: set while `llm` is the canned test model
    /// (`cc-mock-model`) and while a phase is on the test track before Hugo's go.
    pub test_only: bool,
}

mod intake;

pub fn app(state: AppState) -> Router {
    let mut r = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/pair/request", post(pair_request))
        .route("/pair/{id}/claim", post(pair_claim))
        .route("/auth/set-password", post(set_password))
        .route("/auth/login", post(login))
        .route("/auth/refresh", post(refresh))
        .route("/me", get(me))
        .route("/stark/status", get(stark_status))
        .route("/stark/pairing/{id}/decision", post(stark_decide))
        .route("/stark/devices/{id}/revoke", post(stark_revoke))
        .merge(intake::routes());
    if let Some(dir) = &state.web_dir {
        r = r
            .route("/app", get(|| async { axum::response::Redirect::permanent("/app/") }))
            .nest_service("/app/", web_app(dir));
    }
    r.layer(tower_http::trace::TraceLayer::new_for_http()).with_state(state)
}

/// The phone web build: static files with strict headers. Every api call it makes goes
/// through the same pairing gate and login as the desktop app; this only serves the page.
fn web_app(dir: &std::path::Path) -> Router {
    use axum::http::header::{HeaderName, HeaderValue};
    use tower_http::set_header::SetResponseHeaderLayer;
    let files =
        tower_http::services::ServeDir::new(dir).fallback(tower_http::services::ServeFile::new(dir.join("index.html")));
    let h = |name: &'static str, value: &'static str| {
        SetResponseHeaderLayer::overriding(HeaderName::from_static(name), HeaderValue::from_static(value))
    };
    Router::new()
        .fallback_service(files)
        // trunk's loader is one inline module script; everything else must come from here.
        .layer(h(
            "content-security-policy",
            "default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; \
             style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; \
             frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ))
        .layer(h("x-content-type-options", "nosniff"))
        .layer(h("referrer-policy", "no-referrer"))
        .layer(h("cache-control", "no-cache"))
}

// ---------- errors ----------

#[derive(Debug)]
pub enum ApiError {
    BadRequest(&'static str),
    Unauthorized,
    /// Unknown, revoked or missing device token: the app must pair again.
    DeviceRejected,
    NotFound,
    Gone(&'static str),
    Conflict(&'static str),
    TooMany,
    /// A real account asking for a test-only action.
    Forbidden,
    /// Kompas cannot answer now: `kompas_offline`, `cap_reached`, `profile_failed`.
    Unavailable(&'static str),
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (code, msg) = match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ApiError::DeviceRejected => (StatusCode::UNAUTHORIZED, "device_rejected"),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ApiError::Gone(m) => (StatusCode::GONE, m),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m),
            ApiError::TooMany => (StatusCode::TOO_MANY_REQUESTS, "too_many_pending"),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            ApiError::Unavailable(m) => (StatusCode::SERVICE_UNAVAILABLE, m),
            ApiError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        };
        (code, Json(json!({ "error": msg }))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!("db: {e}");
        ApiError::Internal
    }
}

type ApiResult<T> = Result<T, ApiError>;

// ---------- credentials ----------

pub fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn new_device_token() -> String {
    let mut b = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
    URL_SAFE_NO_PAD.encode(b)
}

/// The message her device signs to collect its token: binds the signature to this pairing.
pub fn claim_message(id: Uuid) -> String {
    format!("cc-pair:{id}")
}

async fn device_from_headers(st: &AppState, h: &HeaderMap) -> ApiResult<Uuid> {
    let tok = h
        .get("x-device-token")
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::DeviceRejected)?;
    cc_db::device_by_token(&st.pool, &hash_token(tok))
        .await?
        .ok_or(ApiError::DeviceRejected)
}

fn require_admin(st: &AppState, h: &HeaderMap) -> ApiResult<()> {
    let got = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(ApiError::Unauthorized)?;
    if st.admin_token.is_empty() {
        return Err(ApiError::Unauthorized);
    }
    let a = Sha256::digest(got.as_bytes());
    let b = Sha256::digest(st.admin_token.as_bytes());
    if bool::from(a.ct_eq(&b)) {
        Ok(())
    } else {
        Err(ApiError::Unauthorized)
    }
}

#[derive(Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub dev: Uuid,
    pub exp: i64,
}

fn issue_jwt(st: &AppState, user: Uuid, dev: Uuid) -> ApiResult<SessionOut> {
    let exp = Utc::now() + chrono::Duration::hours(cc_core::SESSION_HOURS);
    let c = Claims {
        sub: user,
        dev,
        exp: exp.timestamp(),
    };
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &c,
        &jsonwebtoken::EncodingKey::from_secret(&st.jwt_secret),
    )
    .map_err(|_| ApiError::Internal)?;
    Ok(SessionOut {
        session: token,
        expires_at: exp,
    })
}

/// Device token AND a session JWT issued to that same device (spec §10: both on every request).
async fn session_from_headers(st: &AppState, h: &HeaderMap) -> ApiResult<Claims> {
    let dev = device_from_headers(st, h).await?;
    let jwt = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(ApiError::Unauthorized)?;
    let data = jsonwebtoken::decode::<Claims>(
        jwt,
        &jsonwebtoken::DecodingKey::from_secret(&st.jwt_secret),
        &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256),
    )
    .map_err(|_| ApiError::Unauthorized)?;
    if data.claims.dev != dev {
        return Err(ApiError::Unauthorized);
    }
    Ok(data.claims)
}

// ---------- pairing ----------

#[derive(Deserialize)]
pub struct PairRequest {
    pub device_name: String,
    /// ed25519 public key, standard base64 of 32 bytes.
    pub public_key: String,
}

#[derive(Serialize, Deserialize)]
pub struct PairCreated {
    pub id: Uuid,
    pub code: String,
    pub expires_at: DateTime<Utc>,
}

async fn pair_request(
    State(st): State<AppState>,
    Json(r): Json<PairRequest>,
) -> ApiResult<(StatusCode, Json<PairCreated>)> {
    let name = r.device_name.trim();
    if name.is_empty() || name.chars().count() > 60 {
        return Err(ApiError::BadRequest("device_name"));
    }
    let pk = STANDARD
        .decode(r.public_key.trim())
        .map_err(|_| ApiError::BadRequest("public_key"))?;
    let pk: [u8; 32] = pk.try_into().map_err(|_| ApiError::BadRequest("public_key"))?;
    ed25519_dalek::VerifyingKey::from_bytes(&pk).map_err(|_| ApiError::BadRequest("public_key"))?;
    if cc_db::pending_pairings(&st.pool).await?.len() >= MAX_PENDING {
        return Err(ApiError::TooMany);
    }
    let code = cc_core::pairing_code();
    let (id, expires_at) = cc_db::create_pairing(&st.pool, name, &pk, &code).await?;
    tracing::info!(%id, "pairing requested");
    Ok((StatusCode::CREATED, Json(PairCreated { id, code, expires_at })))
}

#[derive(Deserialize)]
pub struct ClaimRequest {
    /// ed25519 signature over `claim_message(id)`, standard base64.
    pub signature: String,
}

/// Her app polls this. Pending → 202; approved → the device token, exactly once.
async fn pair_claim(
    State(st): State<AppState>,
    Path(id): Path<Uuid>,
    Json(r): Json<ClaimRequest>,
) -> ApiResult<Response> {
    let d = cc_db::device(&st.pool, id).await?.ok_or(ApiError::NotFound)?;
    let pk: [u8; 32] = d.public_key.as_slice().try_into().map_err(|_| ApiError::Internal)?;
    let vk = ed25519_dalek::VerifyingKey::from_bytes(&pk).map_err(|_| ApiError::Internal)?;
    let sig = STANDARD
        .decode(r.signature.trim())
        .map_err(|_| ApiError::BadRequest("signature"))?;
    let sig = ed25519_dalek::Signature::from_slice(&sig).map_err(|_| ApiError::BadRequest("signature"))?;
    vk.verify_strict(claim_message(id).as_bytes(), &sig)
        .map_err(|_| ApiError::Unauthorized)?;
    match d.status {
        DeviceStatus::Pending => Ok((
            StatusCode::ACCEPTED,
            Json(json!({ "status": "pending", "code": d.code, "expires_at": d.expires_at })),
        )
            .into_response()),
        DeviceStatus::Denied => Err(ApiError::Gone("denied")),
        DeviceStatus::Expired => Err(ApiError::Gone("expired")),
        DeviceStatus::Revoked => Err(ApiError::Gone("revoked")),
        DeviceStatus::Approved if d.claimed => Err(ApiError::Conflict("already_claimed")),
        DeviceStatus::Approved => {
            let token = new_device_token();
            if !cc_db::claim_token(&st.pool, id, &hash_token(&token)).await? {
                return Err(ApiError::Conflict("already_claimed"));
            }
            tracing::info!(%id, "device token issued");
            Ok(Json(json!({ "status": "approved", "device_token": token })).into_response())
        }
    }
}

// ---------- her login ----------

#[derive(Deserialize)]
pub struct Credentials {
    pub email: String,
    pub password: String,
}

#[derive(Serialize, Deserialize)]
pub struct SessionOut {
    pub session: String,
    pub expires_at: DateTime<Utc>,
}

/// First password, set on an approved device for the account Hugo created.
async fn set_password(
    State(st): State<AppState>,
    h: HeaderMap,
    Json(c): Json<Credentials>,
) -> ApiResult<Json<SessionOut>> {
    let dev = device_from_headers(&st, &h).await?;
    if c.password.chars().count() < MIN_PASSWORD_LEN {
        return Err(ApiError::BadRequest("password_too_short"));
    }
    let user = cc_db::user_by_email(&st.pool, &c.email)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    if user.password_hash.is_some() {
        return Err(ApiError::Conflict("password_already_set"));
    }
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(c.password.as_bytes(), &salt)
        .map_err(|_| ApiError::Internal)?
        .to_string();
    if !cc_db::set_first_password(&st.pool, user.id, &hash).await? {
        return Err(ApiError::Conflict("password_already_set"));
    }
    Ok(Json(issue_jwt(&st, user.id, dev)?))
}

async fn login(State(st): State<AppState>, h: HeaderMap, Json(c): Json<Credentials>) -> ApiResult<Json<SessionOut>> {
    let dev = device_from_headers(&st, &h).await?;
    let user = cc_db::user_by_email(&st.pool, &c.email).await?;
    let ok = match user.as_ref().and_then(|u| u.password_hash.as_deref()) {
        Some(stored) => PasswordHash::new(stored)
            .map(|ph| Argon2::default().verify_password(c.password.as_bytes(), &ph).is_ok())
            .unwrap_or(false),
        None => false,
    };
    match (ok, user) {
        (true, Some(u)) => Ok(Json(issue_jwt(&st, u.id, dev)?)),
        _ => Err(ApiError::Unauthorized),
    }
}

async fn refresh(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<SessionOut>> {
    let c = session_from_headers(&st, &h).await?;
    Ok(Json(issue_jwt(&st, c.sub, c.dev)?))
}

async fn me(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<serde_json::Value>> {
    let c = session_from_headers(&st, &h).await?;
    let u = cc_db::user_by_id(&st.pool, c.sub)
        .await?
        .ok_or(ApiError::Unauthorized)?;
    Ok(Json(json!({ "email": u.email, "role": u.role, "is_test": u.is_test })))
}

// ---------- STARK ----------

async fn stark_status(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<StarkStatus>> {
    require_admin(&st, &h)?;
    let spent = cc_db::month_spend(&st.pool).await?;
    let cap = cc_db::cap_eur(&st.pool).await?;
    Ok(Json(StarkStatus {
        month: Utc::now().format("%Y-%m").to_string(),
        spent_eur: spent,
        cap_eur: cap,
        level: cc_core::usage_level(spent, cap),
        pending: cc_db::pending_pairings(&st.pool).await?,
        devices: cc_db::devices(&st.pool).await?,
    }))
}

#[derive(Deserialize)]
pub struct Decision {
    pub approve: bool,
}

async fn stark_decide(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(d): Json<Decision>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&st, &h)?;
    if !cc_db::decide_pairing(&st.pool, id, d.approve).await? {
        return Err(ApiError::Conflict("not_pending"));
    }
    tracing::info!(%id, approve = d.approve, "pairing decided");
    Ok(Json(json!({ "ok": true })))
}

async fn stark_revoke(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&st, &h)?;
    if !cc_db::revoke_device(&st.pool, id).await? {
        return Err(ApiError::NotFound);
    }
    tracing::info!(%id, "device revoked");
    Ok(Json(json!({ "ok": true })))
}
