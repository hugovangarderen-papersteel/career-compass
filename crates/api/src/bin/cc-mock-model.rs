//! `cc-mock-model`: a canned stand-in for the model API, so a test account can click through
//! the whole intake on a phone without a model key. Every word is made up and marked as such.
//! cc-api only uses it with CC_MOCK_MODEL=1, and then answers test accounts only.
//! It reports zero tokens, so the monthly meter stays untouched.

use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{http::StatusCode, routing::post, Json, Router};
use serde_json::{json, Value};

const QUESTIONS: [&str; 8] = [
    "(testmodel) Welkom! Hoe wil je dat ik je noem, en waar woon je ongeveer?",
    "(testmodel) Vertel eens over je laatste baan: wat deed je daar op een gewone dag?",
    "(testmodel) Wat is een moment waar je trots op bent? Wat gebeurde er, en wat deed jij?",
    "(testmodel) Waar krijg je energie van in je werk, en waarvan juist niet?",
    "(testmodel) Wat voor rol en wat voor organisatie zoek je nu?",
    "(testmodel) Hoeveel uur per week wil je werken, en hoe ver wil je reizen?",
    "(testmodel) Is er iets in je loopbaan dat je graag goed wilt uitleggen, zoals een pauze?",
    "(testmodel) Dank je. Wil je nog iets aanvullen voordat ik je profiel maak?",
];

static TURN: AtomicUsize = AtomicUsize::new(0);

/// The answer the request's schema asks for (the same switch as the test mock).
fn answer(b: &Value) -> Option<Value> {
    let props = &b["output_config"]["format"]["schema"]["properties"];
    Some(if props.get("phase_complete").is_some() {
        let i = TURN.fetch_add(1, Ordering::Relaxed) % QUESTIONS.len();
        json!({"message": QUESTIONS[i], "phase_complete": true})
    } else if props.get("timeline").is_some() {
        json!({
            "timeline": [{"role": "Programmamanager (testdata)", "org": "Verzonnen zorggroep", "start": "2015",
                "end": "2024", "scope": "team van 12", "source_ids": ["T2"]}],
            "evidence": [{"id": "E1", "title": "Fusie (testdata)", "situation": "Twee teams moesten samen.",
                "task": "Het team bij elkaar houden.", "action": "Ze liet beide kanten elkaar uitleggen.",
                "result": "Niemand vertrok.", "metric": "0 van 12 vertrokken", "skills": ["verbinden"],
                "source_ids": ["T4"]}],
            "strengths": [{"skill": "Mensen verbinden (testdata)", "rank": 5, "evidence_ids": ["E1"], "source_ids": ["T4"]}],
            "languages": [{"language": "Nederlands", "level": "moedertaal", "source_ids": ["T2"]}],
            "anti_prefs": [], "work_style": [{"text": "Rustig overleg, dan doorpakken (testdata).", "source_ids": ["T6"]}]
        })
    } else if props.get("home_base").is_some() {
        json!({
            "role_types": [{"value": "interim", "weight": 0.8, "source_ids": ["T8"]}],
            "stages": [{"value": "scaleup", "weight": 0.7, "source_ids": ["T8"]}],
            "mission_themes": [], "work_mode": null, "start_date": null,
            "max_travel": {"minutes": 45, "mode": "auto", "source_ids": ["T10"]},
            "hours_per_week": {"min": 24, "max": 32, "source_ids": ["T10"]},
            "subsectors": [{"value": "care_tech", "weight": 0.9, "source_ids": ["T8"]}],
            "home_base": {"city": "Utrecht (testdata)", "source_ids": ["T2"]},
            "gap_framing": {"text": "Een jaar zorg voor familie (testdata).", "source_ids": ["T12"]}
        })
    } else if props.get("voice_sample").is_some() {
        json!({
            "pitch": {"text": "(testdata) Ik verbind mensen die elkaar eerst niet verstaan, en bouw zo zorg die werkt.",
                "source_ids": ["T4", "T6"]},
            "voice_sample": [{"text": "(testdata) Ik laat beide kanten eerst uitpraten.", "source_ids": ["T4"]}]
        })
    } else if props.get("updated").is_some() {
        json!({"updated": {"gap_framing": {"text": "(testdata) Aangepast zoals gevraagd.", "source_ids": ["T2"]}}})
    } else {
        return None;
    })
}

async fn messages(Json(b): Json<Value>) -> (StatusCode, Json<Value>) {
    match answer(&b) {
        Some(input) => (
            StatusCode::OK,
            Json(json!({
                "stop_reason": "end_turn",
                "content": [{"type": "text", "text": input.to_string()}],
                "usage": {"input_tokens": 0, "output_tokens": 0,
                          "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}
            })),
        ),
        None => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"type": "unknown_tool"}})),
        ),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bind = std::env::var("CC_MOCK_BIND").unwrap_or_else(|_| "0.0.0.0:9000".into());
    let app = Router::new().route("/v1/messages", post(messages));
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
