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

    // The model key lives only in the server .env; without it Kompas reports offline.
    let llm = std::env::var("CC_ANTHROPIC_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .map(|k| {
            let base = std::env::var("CC_ANTHROPIC_URL").unwrap_or_else(|_| "https://api.anthropic.com".into());
            cc_llm::Client::new(&base, k.trim())
        });
    if llm.is_none() {
        tracing::warn!("CC_ANTHROPIC_API_KEY not set: Kompas is offline");
    }
    let eur_per_usd = match std::env::var("CC_EUR_PER_USD") {
        Ok(v) => v
            .parse::<f64>()
            .ok()
            .filter(|x| *x > 0.0)
            .context("CC_EUR_PER_USD must be a positive number")?,
        Err(_) => 1.0,
    };
    // CC_MOCK_MODEL=1: `llm` is the canned `cc-mock-model` service, answered to test accounts only.
    let mock_model = std::env::var("CC_MOCK_MODEL").is_ok_and(|v| v.trim() == "1");
    if mock_model {
        tracing::warn!("CC_MOCK_MODEL=1: Kompas is the canned test model, test accounts only");
    }
    let web_dir = std::env::var("CC_WEB_DIR").ok().map(std::path::PathBuf::from);

    let app = cc_api::app(cc_api::AppState {
        pool,
        jwt_secret: jwt_secret.into_bytes(),
        admin_token,
        llm,
        eur_per_usd,
        web_dir,
        mock_model,
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
