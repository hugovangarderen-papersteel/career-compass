//! End-to-end pairing and login against a real Postgres (sqlx::test makes a fresh DB per test).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

const ADMIN: &str = "test-admin-token-0123456789abcdef0123";

fn app(pool: PgPool) -> Router {
    cc_api::app(cc_api::AppState {
        pool,
        jwt_secret: b"test-jwt-secret-0123456789abcdef0123".to_vec(),
        admin_token: ADMIN.into(),
        llm: None,
        eur_per_usd: 1.0,
        web_dir: None,
        test_only: false,
    })
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let req = match body {
        Some(j) => b
            .header("content-type", "application/json")
            .body(Body::from(j.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let code = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (code, v)
}

fn admin() -> (&'static str, String) {
    ("authorization", format!("Bearer {ADMIN}"))
}

fn key() -> SigningKey {
    let mut b = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
    SigningKey::from_bytes(&b)
}

async fn request(app: &Router, k: &SigningKey) -> (String, String) {
    let pk = STANDARD.encode(k.verifying_key().to_bytes());
    let (c, v) = call(
        app,
        "POST",
        "/pair/request",
        &[],
        Some(json!({ "device_name": "MacBook Air van mama", "public_key": pk })),
    )
    .await;
    assert_eq!(c, StatusCode::CREATED, "{v}");
    (
        v["id"].as_str().unwrap().to_string(),
        v["code"].as_str().unwrap().to_string(),
    )
}

async fn claim(app: &Router, k: &SigningKey, id: &str) -> (StatusCode, Value) {
    let msg = format!("cc-pair:{id}");
    let sig = STANDARD.encode(k.sign(msg.as_bytes()).to_bytes());
    call(
        app,
        "POST",
        &format!("/pair/{id}/claim"),
        &[],
        Some(json!({ "signature": sig })),
    )
    .await
}

async fn decide(app: &Router, id: &str, approve: bool) -> StatusCode {
    let (h, v) = admin();
    call(
        app,
        "POST",
        &format!("/stark/pairing/{id}/decision"),
        &[(h, &v)],
        Some(json!({ "approve": approve })),
    )
    .await
    .0
}

#[sqlx::test(migrations = "../db/migrations")]
async fn full_pairing_and_login(pool: PgPool) {
    cc_db::create_user(&pool, "Seeker@Example.org", cc_core::Role::Seeker)
        .await
        .unwrap();
    let app = app(pool);
    let k = key();
    let (id, code) = request(&app, &k).await;

    // STARK sees the pending request with the same code; no admin token, no status.
    assert_eq!(
        call(&app, "GET", "/stark/status", &[], None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (h, v) = admin();
    let (c, s) = call(&app, "GET", "/stark/status", &[(h, &v)], None).await;
    assert_eq!(c, StatusCode::OK);
    assert_eq!(s["pending"][0]["code"], code);
    assert_eq!(s["level"], "green");
    assert_eq!(s["cap_eur"], 30.0);

    // Before Hugo decides, the device waits.
    assert_eq!(claim(&app, &k, &id).await.0, StatusCode::ACCEPTED);
    // Another key cannot claim this pairing.
    assert_eq!(claim(&app, &key(), &id).await.0, StatusCode::UNAUTHORIZED);

    assert_eq!(decide(&app, &id, true).await, StatusCode::OK);
    assert_eq!(
        decide(&app, &id, false).await,
        StatusCode::CONFLICT,
        "decided once only"
    );

    let (c, v) = claim(&app, &k, &id).await;
    assert_eq!(c, StatusCode::OK);
    let token = v["device_token"].as_str().unwrap().to_string();
    assert_eq!(
        claim(&app, &k, &id).await.0,
        StatusCode::CONFLICT,
        "token is handed out once"
    );

    let dt = ("x-device-token", token.as_str());
    let creds = json!({ "email": "seeker@example.org", "password": "correct horse" });
    assert_eq!(
        call(&app, "POST", "/auth/set-password", &[], Some(creds.clone()))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (c, v) = call(&app, "POST", "/auth/set-password", &[dt], Some(creds.clone())).await;
    assert_eq!(c, StatusCode::OK, "{v}");
    assert_eq!(
        call(&app, "POST", "/auth/set-password", &[dt], Some(creds.clone()))
            .await
            .0,
        StatusCode::CONFLICT
    );

    let bad = json!({ "email": "seeker@example.org", "password": "wrong password" });
    assert_eq!(
        call(&app, "POST", "/auth/login", &[dt], Some(bad)).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (c, v) = call(&app, "POST", "/auth/login", &[dt], Some(creds)).await;
    assert_eq!(c, StatusCode::OK);
    let bearer = format!("Bearer {}", v["session"].as_str().unwrap());

    let (c, me) = call(&app, "GET", "/me", &[dt, ("authorization", &bearer)], None).await;
    assert_eq!(c, StatusCode::OK);
    assert_eq!(me["email"], "seeker@example.org");
    // A session without its device token is refused.
    assert_eq!(
        call(&app, "GET", "/me", &[("authorization", &bearer)], None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "POST", "/auth/refresh", &[dt, ("authorization", &bearer)], None)
            .await
            .0,
        StatusCode::OK
    );

    // Revoking the device kills the session at once.
    let (h, v) = admin();
    assert_eq!(
        call(&app, "POST", &format!("/stark/devices/{id}/revoke"), &[(h, &v)], None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/me", &[dt, ("authorization", &bearer)], None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrations = "../db/migrations")]
async fn denied_and_expired(pool: PgPool) {
    let app = app(pool.clone());
    let k = key();
    let (id, _) = request(&app, &k).await;
    assert_eq!(decide(&app, &id, false).await, StatusCode::OK);
    assert_eq!(claim(&app, &k, &id).await.0, StatusCode::GONE);

    let (id2, _) = request(&app, &k).await;
    sqlx::query("UPDATE devices SET expires_at = now() - interval '1 minute' WHERE id = $1::uuid")
        .bind(&id2)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        decide(&app, &id2, true).await,
        StatusCode::CONFLICT,
        "an expired request cannot be approved"
    );
    assert_eq!(claim(&app, &k, &id2).await.0, StatusCode::GONE);
}

#[sqlx::test(migrations = "../db/migrations")]
async fn guards(pool: PgPool) {
    let app = app(pool.clone());
    let (c, _) = call(
        &app,
        "POST",
        "/pair/request",
        &[],
        Some(json!({ "device_name": "x", "public_key": "bm90LWEta2V5" })),
    )
    .await;
    assert_eq!(c, StatusCode::BAD_REQUEST);
    for _ in 0..cc_api::MAX_PENDING {
        request(&app, &key()).await;
    }
    let pk = STANDARD.encode(key().verifying_key().to_bytes());
    let (c, _) = call(
        &app,
        "POST",
        "/pair/request",
        &[],
        Some(json!({ "device_name": "flood", "public_key": pk })),
    )
    .await;
    assert_eq!(c, StatusCode::TOO_MANY_REQUESTS);

    let wrong = ("authorization", "Bearer wrong-token");
    assert_eq!(
        call(&app, "GET", "/stark/status", &[wrong], None).await.0,
        StatusCode::UNAUTHORIZED
    );

    cc_db::log_llm_call(
        &pool,
        &cc_db::LlmCall {
            purpose: "test",
            model: "claude-haiku",
            tokens_in: 100,
            tokens_out: 50,
            cache_read: 0,
            cache_write: 0,
            cost_eur: 28.0,
        },
    )
    .await
    .unwrap();
    let (h, v) = admin();
    let (_, s) = call(&app, "GET", "/stark/status", &[(h, &v)], None).await;
    assert_eq!(s["level"], "red");
}
