//! `cc-api`: the Career Compass server. Config comes from the environment only.

use anyhow::{bail, Context};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .init();

    let db_url = std::env::var("DATABASE_URL").context("DATABASE_URL not set")?;
    let jwt_secret = std::env::var("CC_JWT_SECRET").context("CC_JWT_SECRET not set")?;
    let admin_token = std::env::var("CC_ADMIN_TOKEN").context("CC_ADMIN_TOKEN not set")?;
    if jwt_secret.len() < 32 || admin_token.len() < 32 {
        bail!("CC_JWT_SECRET and CC_ADMIN_TOKEN must each be at least 32 characters");
    }
    let bind = std::env::var("CC_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());

    let pool = cc_db::connect(&db_url).await.context("connect to Postgres")?;
    cc_db::migrate(&pool).await.context("run migrations")?;

    let app = cc_api::app(cc_api::AppState {
        pool,
        jwt_secret: jwt_secret.into_bytes(),
        admin_token,
    });
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    tracing::info!("listening on {bind}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
