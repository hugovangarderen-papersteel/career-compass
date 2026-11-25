//! `cc-mock-model`: a canned stand-in for the model API, so a test account can click through
//! the whole intake on a phone without a model key. Every word is made up and marked as such.
//! cc-api only uses it with CC_MOCK_MODEL=1, and then answers test accounts only.
//! It reports zero tokens, so the monthly meter stays untouched.

use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{http::StatusCode, routing::post, Json, Router};
use serde_json::{json, Value};

const QUESTIONS: [&str; 8] = [
    "(test model) Welcome! What would you like me to call you, and roughly where do you live?",
    "(test model) Tell me about your last job: what did you do there on an ordinary day?",
    "(test model) What is a moment you are proud of? What happened, and what did you do?",
    "(test model) What gives you energy in your work, and what takes it away?",
    "(test model) What kind of role and what kind of organisation are you looking for now?",
    "(test model) How many hours a week would you like to work, and how far are you willing to travel?",
    "(test model) Is there anything in your career you would like to explain well, such as a break?",
    "(test model) Thank you. Is there anything you would like to add before I make your profile?",
];

static TURN: AtomicUsize = AtomicUsize::new(0);

/// The answer the request's schema asks for (the same switch as the test mock).
fn answer(b: &Value) -> Option<Value> {
    let props = &b["output_config"]["format"]["schema"]["properties"];
    Some(if props.get("skills_reason").is_some() {
        json!({"skills": 0.7, "skills_reason": "Her strengths match what this company needs (test data).",
            "mission": 0.6, "mission_reason": "Its mission sits close to her themes (test data).",
            "anti_pref_hit": "", "working_language": "dutch"})
    } else if props.get("phase_complete").is_some() {
        let i = TURN.fetch_add(1, Ordering::Relaxed) % QUESTIONS.len();
        json!({"message": QUESTIONS[i], "phase_complete": true})
    } else if props.get("timeline").is_some() {
        json!({
            "timeline": [{"role": "Programme manager (test data)", "org": "Made-up care group", "start": "2015",
                "end": "2024", "scope": "team of 12", "source_ids": ["T2"]}],
            "evidence": [{"id": "E1", "title": "Merger (test data)", "situation": "Two teams had to merge.",
                "task": "Keep the team together.", "action": "She had both sides explain themselves to each other.",
                "result": "Nobody left.", "metric": "0 of 12 left", "skills": ["connecting people"],
                "source_ids": ["T4"]}],
            "strengths": [{"skill": "Connecting people (test data)", "rank": 5, "evidence_ids": ["E1"], "source_ids": ["T4"]}],
            "languages": [{"language": "Dutch", "level": "native", "source_ids": ["T2"]}],
            "anti_prefs": [], "work_style": [{"text": "Calm discussion, then decisive action (test data).", "source_ids": ["T6"]}]
        })
    } else if props.get("home_base").is_some() {
        json!({
            "role_types": [{"value": "interim", "weight": 0.8, "source_ids": ["T8"]}],
            "stages": [{"value": "scaleup", "weight": 0.7, "source_ids": ["T8"]}],
            "mission_themes": [], "work_mode": null, "start_date": null,
            "max_travel": {"minutes": 45, "mode": "car", "source_ids": ["T10"]},
            "hours_per_week": {"min": 24, "max": 32, "source_ids": ["T10"]},
            "subsectors": [{"value": "care_tech", "weight": 0.9, "source_ids": ["T8"]}],
            "home_base": {"city": "Utrecht (test data)", "source_ids": ["T2"]},
            "gap_framing": {"text": "A year caring for family (test data).", "source_ids": ["T12"]}
        })
    } else if props.get("voice_sample").is_some() {
        json!({
            "pitch": {"text": "(test data) I connect people who at first do not understand each other, and that is how I build care that works.",
                "source_ids": ["T4", "T6"]},
            "voice_sample": [{"text": "(test data) I let both sides finish talking first.", "source_ids": ["T4"]}]
        })
    } else if props.get("updated").is_some() {
        json!({"updated": {"gap_framing": {"text": "(test data) Changed as asked.", "source_ids": ["T2"]}}})
    } else if props.get("facts").is_some() {
        // The quote has to be in the document, so the first line of its text is the one fact.
        let msg = b["messages"][0]["content"][0]["text"].as_str().unwrap_or_default();
        let line = msg
            .split_once("\n\n")
            .map_or("", |x| x.1)
            .lines()
            .find(|l| !l.trim().is_empty());
        json!({"facts": line.map_or(vec![], |l| vec![json!({"kind": "other",
            "text": "(test data) The document's first line.", "snippet": l.trim()})])})
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
