//! "Talk to Kompas": the intake conversation (spec §3) and her profile (spec §4).
//!
//! Every route needs her device token AND her session; every query is filtered by her user id.
//! Her answer is saved before Kompas is asked, so nothing she typed is ever lost. Every model
//! call is checked against the monthly cap first and logged in `llm_calls` after (spec §10b).
//! Nothing here logs what she wrote.

use crate::{session_from_headers, ApiError, ApiResult, AppState};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use cc_core::profile::{self, Profile, Progress, PHASES, SECTIONS};
use cc_core::{guard_facts, may_spend, DocFact, Lane};
use cc_db::{IntakeSession, Turn};
use cc_llm::{LlmError, Model, Msg, Role, ToolRequest};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use uuid::Uuid;

const KOMPAS: &str = include_str!("prompts/kompas.md");
const EXTRACT: &str = include_str!("prompts/extract.md");
const PITCH: &str = include_str!("prompts/pitch.md");
const CORRECT: &str = include_str!("prompts/correct.md");
const EXTRACT_DOC: &str = include_str!("prompts/extract_document.md");

/// One answer may be long (a story), but not a pasted book.
pub const MAX_ANSWER_CHARS: usize = 6_000;
pub const MAX_DOC_CHARS: usize = 100_000;
/// How much of her confirmed documents Kompas sees in the conversation.
const DOCS_IN_CHAT: usize = 12_000;
/// Every model call is metered under its purpose (Hugo's ruling 2026-11-24).
const PURPOSE: &str = "intake";
const PURPOSE_DOCS: &str = "documents";
/// An uploaded file (PDF/DOCX/TXT) and a spoken clip, before base64.
const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;
const MAX_AUDIO_BYTES: usize = 8 * 1024 * 1024;
/// Base64 adds a third; room for the JSON around it.
const BODY_LIMIT: usize = MAX_FILE_BYTES * 4 / 3 + 64 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/intake", get(view_route))
        .route("/intake/start", post(start))
        .route("/intake/message", post(message))
        .route("/intake/playback", post(playback))
        .route("/intake/pitch", post(pitch))
        .route("/intake/correct", post(correct))
        .route("/intake/approve", post(approve))
        .route("/intake/reset", post(reset))
        .route(
            "/intake/documents",
            get(list_docs).post(add_doc).layer(DefaultBodyLimit::max(BODY_LIMIT)),
        )
        .route("/intake/facts/{id}", post(decide_fact))
        .route("/intake/audio", post(audio).layer(DefaultBodyLimit::max(BODY_LIMIT)))
}

pub(crate) async fn who(st: &AppState, h: &HeaderMap) -> ApiResult<cc_db::User> {
    let c = session_from_headers(st, h).await?;
    cc_db::user_by_id(&st.pool, c.sub).await?.ok_or(ApiError::Unauthorized)
}

fn progress_of(s: &IntakeSession) -> Progress {
    Progress {
        phase: s.phase.clamp(1, PHASES as i16) as u8,
        done: s.phases_done.iter().map(|d| *d as u8).collect(),
    }
}

// ---------- what her screen shows ----------

async fn view(st: &AppState, user: &cc_db::User) -> ApiResult<Json<Value>> {
    let session = cc_db::current_session(&st.pool, user.id).await?;
    let (status, phase, covered, turns) = match &session {
        Some(s) => {
            let p = progress_of(s);
            let t = cc_db::turns(&st.pool, s.id).await?;
            (s.status.clone(), p.phase, p.covered(), t)
        }
        None => ("none".to_string(), 1, 0, vec![]),
    };
    let profile = cc_db::latest_profile(&st.pool, user.id).await?;
    let pending_facts = cc_db::document_facts(&st.pool, user.id)
        .await?
        .iter()
        .filter(|f| f.status == "pending")
        .count();
    Ok(Json(json!({
        "status": status,
        "phase": phase,
        "phase_name": profile::phase_name(phase),
        "covered": covered,
        "total": PHASES,
        "turns": turns.iter().map(|t| json!({"seq": t.seq, "speaker": t.speaker, "phase": t.phase, "text": t.text})).collect::<Vec<_>>(),
        "profile": profile.map(|p| json!({"version": p.version, "status": p.status, "data": p.data})),
        "sections": SECTIONS.iter().map(|s| json!({"key": s.0, "title": s.1, "fields": s.2})).collect::<Vec<_>>(),
        "pending_facts": pending_facts,
        "is_test": user.is_test,
        "online": st.llm.is_some() && (user.is_test || !st.test_only),
    })))
}

async fn view_route(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    view(&st, &user).await
}

// ---------- model calls, metered ----------

async fn ask(st: &AppState, user: &cc_db::User, req: &ToolRequest) -> ApiResult<Value> {
    ask_as(st, user, req, PURPOSE).await
}

async fn ask_as(st: &AppState, user: &cc_db::User, req: &ToolRequest, purpose: &'static str) -> ApiResult<Value> {
    let llm = st.llm.as_ref().ok_or(ApiError::Unavailable("kompas_offline"))?;
    // Test track (or the canned test model): a real account gets no Kompas until Hugo's go.
    if st.test_only && !user.is_test {
        return Err(ApiError::Unavailable("kompas_offline"));
    }
    let spent = cc_db::month_spend(&st.pool).await?;
    let cap = cc_db::cap_eur(&st.pool).await?;
    if !may_spend(spent, req.estimate_usd() * st.eur_per_usd, cap, Lane::Hers) {
        return Err(ApiError::Unavailable("cap_reached"));
    }
    let res = llm.call(req).await;
    let usage = match &res {
        Ok((_, u)) | Err(LlmError::BadAnswer(_, u)) => Some(*u),
        Err(LlmError::Unavailable(_)) => None,
    };
    if let Some(u) = usage {
        cc_db::log_llm_call(
            &st.pool,
            &cc_db::LlmCall {
                purpose,
                model: req.model.id(),
                tokens_in: u.prompt_tokens() as i32,
                tokens_out: u.output as i32,
                cache_read: u.cache_read as i32,
                cache_write: u.cache_write as i32,
                cost_eur: u.cost_usd(req.model) * st.eur_per_usd,
            },
        )
        .await?;
    }
    res.map(|(v, _)| v).map_err(|e| {
        tracing::warn!("intake model call failed: {e}");
        ApiError::Unavailable("kompas_offline")
    })
}

fn transcript(turns: &[Turn]) -> String {
    turns
        .iter()
        .map(|t| {
            let who = if t.speaker == "seeker" { "She" } else { "Kompas" };
            format!("[T{}] {who}: {}", t.seq, t.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn her_turns(turns: &[Turn]) -> Vec<String> {
    turns
        .iter()
        .filter(|t| t.speaker == "seeker")
        .map(|t| t.text.clone())
        .collect()
}

/// What she confirmed from her documents, as `[D1] name:` blocks, cut at `limit` characters in
/// total: the facts she confirmed (or corrected), or the whole text of a document she confirmed
/// before facts existed.
async fn confirmed_docs(st: &AppState, user: Uuid, limit: usize) -> ApiResult<(String, Vec<String>)> {
    let docs = cc_db::documents(&st.pool, user).await?;
    let facts = cc_db::document_facts(&st.pool, user).await?;
    let mut out = String::new();
    let mut ids = vec![];
    for d in &docs {
        let (head, body) = if d.status == "confirmed" {
            ("", d.body.clone())
        } else {
            let mine: Vec<String> = facts
                .iter()
                .filter(|f| f.document_id == d.id && f.status == "confirmed")
                .map(|f| format!("- {}", f.text))
                .collect();
            if mine.is_empty() {
                continue;
            }
            (" (facts she confirmed)", mine.join("\n"))
        };
        let room = limit.saturating_sub(out.len());
        if room < 200 {
            break;
        }
        let body: String = body.chars().take(room).collect();
        out.push_str(&format!("[D{}] {}{head}:\n{}\n\n", d.seq, d.name, body));
        ids.push(format!("D{}", d.seq));
    }
    Ok((out, ids))
}

fn valid_ids(turns: &[Turn], doc_ids: &[String]) -> HashSet<String> {
    turns
        .iter()
        .map(|t| format!("T{}", t.seq))
        .chain(doc_ids.iter().cloned())
        .collect()
}

fn kompas_request(turns: &[Turn], p: &Progress, docs: &str) -> ToolRequest {
    let done = if p.done.is_empty() {
        "none yet".to_string()
    } else {
        p.done.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(", ")
    };
    let docs = if docs.is_empty() {
        "She has not shared any documents (yet).".to_string()
    } else {
        format!("## Documents she confirmed\n{docs}")
    };
    ToolRequest {
        model: Model::Sonnet,
        system_static: KOMPAS.to_string(),
        system_dynamic: format!(
            "## Where the conversation is now\nCurrent phase: {} ({}). Phases covered: {done}.\n\n{docs}",
            p.phase,
            profile::phase_name(p.phase)
        ),
        messages: turns
            .iter()
            .map(|t| Msg {
                role: if t.speaker == "seeker" {
                    Role::User
                } else {
                    Role::Assistant
                },
                text: t.text.clone(),
            })
            .collect(),
        tool_name: "kompas_turn".into(),
        tool_description: "Your next message to her, and whether the current phase is now covered enough.".into(),
        tool_schema: json!({
            "type": "object",
            "properties": {
                "message": {"type": "string", "description": "Your message to her: short, warm, one question."},
                "phase_complete": {"type": "boolean", "description": "true if the current phase is covered enough with this answer."}
            },
            "required": ["message", "phase_complete"]
        }),
        max_tokens: 1_500,
    }
}

fn kompas_message(v: &Value) -> ApiResult<(String, bool)> {
    let msg = v["message"].as_str().map(str::trim).unwrap_or_default();
    if msg.is_empty() {
        return Err(ApiError::Unavailable("kompas_offline"));
    }
    Ok((msg.to_string(), v["phase_complete"].as_bool().unwrap_or(false)))
}

// ---------- the conversation ----------

/// Starts her intake, or resumes it: an open session is reused, never replaced.
async fn start(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let s = match cc_db::current_session(&st.pool, user.id).await? {
        Some(s) if s.status != "approved" => s,
        _ => cc_db::create_session(&st.pool, user.id).await?,
    };
    if cc_db::turns(&st.pool, s.id).await?.is_empty() {
        let (docs, _) = confirmed_docs(&st, user.id, DOCS_IN_CHAT).await?;
        let v = ask(&st, &user, &kompas_request(&[], &progress_of(&s), &docs)).await?;
        let (msg, _) = kompas_message(&v)?;
        cc_db::add_turn(&st.pool, s.id, "kompas", s.phase, &msg).await?;
    }
    view(&st, &user).await
}

#[derive(Deserialize)]
struct Say {
    /// Empty = "Kompas, answer my last message" (after a failed call; her answer is saved).
    #[serde(default)]
    text: String,
}

async fn message(State(st): State<AppState>, h: HeaderMap, Json(b): Json<Say>) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let mut s = cc_db::current_session(&st.pool, user.id)
        .await?
        .filter(|s| s.status == "active")
        .ok_or(ApiError::Conflict("no_active_intake"))?;
    let text = b.text.trim();
    if text.chars().count() > MAX_ANSWER_CHARS {
        return Err(ApiError::BadRequest("too_long"));
    }
    if !text.is_empty() {
        // Saved before Kompas is asked: her answer survives any failure below.
        cc_db::add_turn(&st.pool, s.id, "seeker", s.phase, text).await?;
    }
    let turns = cc_db::turns(&st.pool, s.id).await?;
    if turns.last().map(|t| t.speaker.as_str()) != Some("seeker") {
        return Err(ApiError::BadRequest("empty"));
    }
    let mut p = progress_of(&s);
    let (docs, _) = confirmed_docs(&st, user.id, DOCS_IN_CHAT).await?;
    let v = ask(&st, &user, &kompas_request(&turns, &p, &docs)).await?;
    let (msg, phase_complete) = kompas_message(&v)?;
    p.advance(phase_complete);
    s.phase = p.phase as i16;
    s.phases_done = p.done.iter().map(|d| *d as i16).collect();
    cc_db::add_turn(&st.pool, s.id, "kompas", s.phase, &msg).await?;
    cc_db::save_session(&st.pool, &s).await?;
    view(&st, &user).await
}

// ---------- playback: extract, pitch, correct, approve ----------

fn src() -> Value {
    json!({"type": "array", "items": {"type": "string"}, "description": "Sources: \"T12\", \"D1\"."})
}

fn text_item() -> Value {
    json!({"type": "object", "properties": {"text": {"type": "string"}, "source_ids": src()}, "required": ["text", "source_ids"]})
}

fn weighted(values: Option<&[&str]>) -> Value {
    let value = match values {
        Some(v) => json!({"type": "string", "enum": v}),
        None => json!({"type": "string"}),
    };
    json!({"type": "array", "items": {"type": "object", "properties": {
        "value": value, "weight": {"type": "number", "minimum": 0, "maximum": 1}, "source_ids": src()
    }, "required": ["value", "weight", "source_ids"]}})
}

/// JSON schema of every profile field (spec §4), for the extraction and correction tools.
pub fn field_schemas() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("pitch".into(), text_item());
    m.insert(
        "timeline".into(),
        json!({"type": "array", "items": {"type": "object", "properties": {
            "role": {"type": "string"}, "org": {"type": "string"},
            "start": {"type": "string", "description": "year or month-year, as she said it"},
            "end": {"type": "string"}, "scope": {"type": "string"}, "team_size": {"type": "string"},
            "source_ids": src()
        }, "required": ["role", "org", "source_ids"]}}),
    );
    m.insert(
        "evidence".into(),
        json!({"type": "array", "items": {"type": "object", "properties": {
            "id": {"type": "string", "description": "E1, E2, ..."}, "title": {"type": "string"},
            "situation": {"type": "string"}, "task": {"type": "string"}, "action": {"type": "string"},
            "result": {"type": "string"}, "metric": {"type": "string"},
            "skills": {"type": "array", "items": {"type": "string"}}, "source_ids": src()
        }, "required": ["id", "title", "situation", "task", "action", "result", "skills", "source_ids"]}}),
    );
    m.insert(
        "strengths".into(),
        json!({"type": "array", "items": {"type": "object", "properties": {
            "skill": {"type": "string"}, "rank": {"type": "integer", "minimum": 1, "maximum": 5, "description": "5 = strongest evidence, 1 = weakest evidence"},
            "evidence_ids": {"type": "array", "items": {"type": "string"}}, "source_ids": src()
        }, "required": ["skill", "rank", "evidence_ids", "source_ids"]}}),
    );
    m.insert("anti_prefs".into(), json!({"type": "array", "items": text_item()}));
    m.insert("work_style".into(), json!({"type": "array", "items": text_item()}));
    m.insert("role_types".into(), weighted(Some(&profile::ROLE_TYPES)));
    m.insert("stages".into(), weighted(Some(&profile::STAGES)));
    m.insert("subsectors".into(), weighted(Some(&profile::SUBSECTORS)));
    m.insert("mission_themes".into(), weighted(None));
    m.insert(
        "home_base".into(),
        json!({"type": "object", "properties": {"city": {"type": "string"}, "source_ids": src()}, "required": ["city", "source_ids"]}),
    );
    m.insert(
        "max_travel".into(),
        json!({"type": "object", "properties": {"minutes": {"type": "integer"}, "mode": {"type": "string"}, "source_ids": src()}, "required": ["minutes", "source_ids"]}),
    );
    m.insert(
        "hours_per_week".into(),
        json!({"type": "object", "properties": {"min": {"type": "integer"}, "max": {"type": "integer"}, "source_ids": src()}, "required": ["min", "max", "source_ids"]}),
    );
    m.insert("work_mode".into(), text_item());
    m.insert("start_date".into(), text_item());
    m.insert("gap_framing".into(), text_item());
    m.insert(
        "voice_sample".into(),
        json!({"type": "array", "items": text_item(), "description": "3-5 sentences she said, word for word"}),
    );
    m.insert(
        "languages".into(),
        json!({"type": "array", "items": {"type": "object", "properties": {
            "language": {"type": "string"}, "level": {"type": "string"}, "source_ids": src()
        }, "required": ["language", "level", "source_ids"]}}),
    );
    m
}

/// The profile fields Haiku extracts, in two calls (pitch and voice sample come from Sonnet).
const EXTRACT_STORIES: [&str; 6] = [
    "timeline",
    "evidence",
    "strengths",
    "languages",
    "anti_prefs",
    "work_style",
];
const EXTRACT_PREFS: [&str; 10] = [
    "role_types",
    "stages",
    "subsectors",
    "mission_themes",
    "home_base",
    "max_travel",
    "hours_per_week",
    "work_mode",
    "start_date",
    "gap_framing",
];

fn object_schema(keys: &[&str]) -> Value {
    let all = field_schemas();
    let props: Map<String, Value> = keys
        .iter()
        .filter_map(|k| all.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect();
    json!({"type": "object", "properties": props})
}

async fn playback_session(st: &AppState, user: Uuid) -> ApiResult<IntakeSession> {
    cc_db::current_session(&st.pool, user)
        .await?
        .filter(|s| s.status == "playback" || (s.status == "active" && s.phase == PHASES as i16))
        .ok_or(ApiError::Conflict("not_ready_for_playback"))
}

async fn draft(st: &AppState, user: Uuid) -> ApiResult<Profile> {
    let row = cc_db::latest_profile(&st.pool, user)
        .await?
        .filter(|p| p.status == "draft")
        .ok_or(ApiError::Conflict("no_draft"))?;
    serde_json::from_value(row.data).map_err(|_| ApiError::Internal)
}

async fn store(
    st: &AppState,
    user: Uuid,
    s: &IntakeSession,
    p: &Profile,
    report: &profile::CleanReport,
) -> ApiResult<()> {
    tracing::info!(
        dropped = report.dropped.len(),
        problems = report.problems.len(),
        "profile draft saved"
    );
    let data = serde_json::to_value(p).map_err(|_| ApiError::Internal)?;
    cc_db::save_draft(&st.pool, user, s.id, &data).await?;
    Ok(())
}

/// Step 1 of playback: Haiku turns the transcript into the profile draft (no pitch yet).
async fn playback(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let mut s = playback_session(&st, user.id).await?;
    let turns = cc_db::turns(&st.pool, s.id).await?;
    let (docs, doc_ids) = confirmed_docs(&st, user.id, MAX_DOC_CHARS).await?;
    let text = format!(
        "## Conversation
{}

## Documents
{}",
        transcript(&turns),
        if docs.is_empty() { "(none)" } else { &docs }
    );
    // Two halves: the whole profile in one schema is past what the API compiles.
    let req = |keys: &[&str]| ToolRequest {
        model: Model::Haiku,
        system_static: EXTRACT.to_string(),
        system_dynamic: String::new(),
        messages: vec![Msg {
            role: Role::User,
            text: text.clone(),
        }],
        tool_name: "save_profile".into(),
        tool_description: "Save the profile that follows from the conversation.".into(),
        tool_schema: object_schema(keys),
        max_tokens: 8_000,
    };
    let (stories, prefs) = (req(&EXTRACT_STORIES), req(&EXTRACT_PREFS));
    let (mut v, more) = tokio::try_join!(ask(&st, &user, &stories), ask(&st, &user, &prefs))?;
    if let (Some(o), Value::Object(m)) = (v.as_object_mut(), more) {
        o.extend(m);
    }
    let mut p: Profile = serde_json::from_value(v).map_err(|e| {
        tracing::warn!("extraction did not parse: {:?}", e.classify());
        ApiError::Unavailable("profile_failed")
    })?;
    // A fresh extraction keeps the pitch and voice sample of an earlier draft until step 2 runs.
    if let Ok(old) = draft(&st, user.id).await {
        p.pitch = old.pitch;
        p.voice_sample = old.voice_sample;
    }
    let report = p.clean(&valid_ids(&turns, &doc_ids), &her_turns(&turns));
    store(&st, user.id, &s, &p, &report).await?;
    s.status = "playback".into();
    cc_db::save_session(&st.pool, &s).await?;
    view(&st, &user).await
}

/// Step 2 of playback: Sonnet writes her pitch and picks her voice sample.
async fn pitch(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let s = playback_session(&st, user.id).await?;
    let mut p = draft(&st, user.id).await?;
    let turns = cc_db::turns(&st.pool, s.id).await?;
    let (_, doc_ids) = confirmed_docs(&st, user.id, MAX_DOC_CHARS).await?;
    let mut facts = serde_json::to_value(&p).map_err(|_| ApiError::Internal)?;
    if let Some(o) = facts.as_object_mut() {
        o.remove("pitch");
        o.remove("voice_sample");
    }
    let mut note = String::new();
    for _attempt in 0..2 {
        let req = ToolRequest {
            model: Model::Sonnet,
            system_static: PITCH.to_string(),
            system_dynamic: String::new(),
            messages: vec![Msg {
                role: Role::User,
                text: format!("## Conversation\n{}\n\n## Profile\n{facts}{note}", transcript(&turns)),
            }],
            tool_name: "write_pitch".into(),
            tool_description: "Her pitch and her voice sample.".into(),
            tool_schema: object_schema(&["pitch", "voice_sample"]),
            max_tokens: 1_500,
        };
        let v = ask(&st, &user, &req).await?;
        p.pitch = serde_json::from_value(v["pitch"].clone()).unwrap_or_default();
        p.voice_sample = serde_json::from_value(v["voice_sample"].clone()).unwrap_or_default();
        let words = profile::word_count(&p.pitch.text);
        if words <= profile::PITCH_MAX_WORDS {
            break;
        }
        note = format!("\n\nNote: your previous pitch was {words} words. Stay well under 150.");
    }
    p.pitch.text = profile::trim_to_words(&p.pitch.text, profile::PITCH_MAX_WORDS);
    let report = p.clean(&valid_ids(&turns, &doc_ids), &her_turns(&turns));
    store(&st, user.id, &s, &p, &report).await?;
    view(&st, &user).await
}

#[derive(Deserialize)]
struct Correction {
    section: String,
    text: String,
}

/// Her correction to one playback section: saved as her turn, then that section is revised.
async fn correct(State(st): State<AppState>, h: HeaderMap, Json(b): Json<Correction>) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let s = cc_db::current_session(&st.pool, user.id)
        .await?
        .filter(|s| s.status == "playback")
        .ok_or(ApiError::Conflict("not_in_playback"))?;
    let keys = profile::section_keys(&b.section).ok_or(ApiError::BadRequest("unknown_section"))?;
    let text = b.text.trim();
    if text.is_empty() || text.chars().count() > MAX_ANSWER_CHARS {
        return Err(ApiError::BadRequest("empty"));
    }
    let current = serde_json::to_value(draft(&st, user.id).await?).map_err(|_| ApiError::Internal)?;
    let seq = cc_db::add_turn(&st.pool, s.id, "seeker", PHASES as i16, text).await?;
    let turns = cc_db::turns(&st.pool, s.id).await?;
    let part: Map<String, Value> = keys.iter().map(|k| (k.to_string(), current[*k].clone())).collect();
    let title = SECTIONS
        .iter()
        .find(|x| x.0 == b.section)
        .map(|x| x.1)
        .unwrap_or_default();
    let mut msg = format!(
        "## Part: {title}\n{}\n\n## Her correction\n[T{seq}] She: {text}",
        Value::Object(part)
    );
    if matches!(b.section.as_str(), "pitch" | "voice") {
        let hers: Vec<String> = turns
            .iter()
            .filter(|t| t.speaker == "seeker")
            .map(|t| format!("[T{}] She: {}", t.seq, t.text))
            .collect();
        msg.push_str(&format!("\n\n## Her turns\n{}", hers.join("\n")));
    }
    let req = ToolRequest {
        model: Model::Sonnet,
        system_static: CORRECT.to_string(),
        system_dynamic: String::new(),
        messages: vec![Msg {
            role: Role::User,
            text: msg,
        }],
        tool_name: "revise_section".into(),
        tool_description: "The revised part.".into(),
        tool_schema: json!({"type": "object", "properties": {"updated": object_schema(keys)}, "required": ["updated"]}),
        max_tokens: 3_000,
    };
    let v = ask(&st, &user, &req).await?;
    let mut data = current;
    for k in keys {
        if let Some(new) = v["updated"].get(*k) {
            data[*k] = new.clone();
        }
    }
    let mut p: Profile = serde_json::from_value(data).map_err(|_| ApiError::Unavailable("profile_failed"))?;
    p.pitch.text = profile::trim_to_words(&p.pitch.text, profile::PITCH_MAX_WORDS);
    let (_, doc_ids) = confirmed_docs(&st, user.id, MAX_DOC_CHARS).await?;
    let report = p.clean(&valid_ids(&turns, &doc_ids), &her_turns(&turns));
    store(&st, user.id, &s, &p, &report).await?;
    view(&st, &user).await
}

/// "This is me": locks the draft as an approved version (spec §4).
async fn approve(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let mut s = cc_db::current_session(&st.pool, user.id)
        .await?
        .filter(|s| s.status == "playback")
        .ok_or(ApiError::Conflict("not_in_playback"))?;
    cc_db::approve_profile(&st.pool, user.id)
        .await?
        .ok_or(ApiError::Conflict("no_draft"))?;
    // Matching starts from the approved version: config, then a rescore and the first digest.
    cc_db::enqueue(
        &st.pool,
        "derive_config",
        json!({"user_id": user.id}),
        chrono::Utc::now(),
    )
    .await?;
    s.status = "approved".into();
    cc_db::save_session(&st.pool, &s).await?;
    view(&st, &user).await
}

/// Test accounts only: start over. A real account gets 403, always.
async fn reset(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    if !user.is_test {
        return Err(ApiError::Forbidden);
    }
    cc_db::reset_intake(&st.pool, user.id).await?;
    view(&st, &user).await
}

// ---------- her documents: an upload, the facts the model reads from it, her confirmation ----------

async fn list_docs(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let docs = cc_db::documents(&st.pool, user.id).await?;
    let facts = cc_db::document_facts(&st.pool, user.id).await?;
    Ok(Json(json!(docs
        .iter()
        .map(|d| json!({
            "id": d.id,
            "label": format!("D{}", d.seq),
            "name": d.name,
            "status": d.status,
            "dropped": d.dropped,
            "facts": facts
                .iter()
                .filter(|f| f.document_id == d.id)
                .map(|f| json!({"id": f.id, "kind": f.kind, "text": f.text, "snippet": f.snippet, "status": f.status}))
                .collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())))
}

/// A pasted text (`body`) or an uploaded file (`data`: base64 of a PDF, DOCX or TXT file).
#[derive(Deserialize)]
struct NewDoc {
    name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    data: Option<String>,
}

/// Reads the text, has the model list the facts it states, keeps only the facts whose quote is
/// really in the text (the snippet guard), and stores them as pending: nothing reaches Kompas
/// until she confirms it. When the model fails, nothing is stored and she can try again.
async fn add_doc(State(st): State<AppState>, h: HeaderMap, Json(b): Json<NewDoc>) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let name = b.name.trim();
    if name.chars().count() > 200 {
        return Err(ApiError::BadRequest("too_long"));
    }
    let (text, empty) = match (b.body, b.data) {
        (Some(t), None) => (t, "empty"),
        (None, Some(d)) => {
            let bytes = STANDARD
                .decode(d.trim())
                .map_err(|_| ApiError::BadRequest("unreadable_file"))?;
            if bytes.len() > MAX_FILE_BYTES {
                return Err(ApiError::BadRequest("too_long"));
            }
            // A broken PDF can panic inside the parser; the blocking task contains it.
            let t = tokio::task::spawn_blocking(move || file_text(&bytes))
                .await
                .map_err(|_| ApiError::BadRequest("unreadable_file"))??;
            (t, "no_text")
        }
        _ => return Err(ApiError::BadRequest("empty")),
    };
    let text: String = text.trim().chars().take(MAX_DOC_CHARS).collect();
    if text.is_empty() {
        return Err(ApiError::BadRequest(empty));
    }
    let name = if name.is_empty() { "Document" } else { name };
    let v = ask_as(&st, &user, &facts_request(name, &text), PURPOSE_DOCS).await?;
    let facts: Vec<DocFact> =
        serde_json::from_value(v["facts"].clone()).map_err(|_| ApiError::Unavailable("kompas_offline"))?;
    let (kept, dropped) = guard_facts(&text, facts);
    cc_db::add_document(&st.pool, user.id, name, &text, &kept, dropped as i32).await?;
    list_docs(State(st), h).await
}

/// The text of an uploaded file: PDF, Word (.docx) or plain UTF-8 text. Anything else is refused.
fn file_text(bytes: &[u8]) -> ApiResult<String> {
    if bytes.starts_with(b"%PDF") {
        pdf_extract::extract_text_from_mem(bytes).map_err(|_| ApiError::BadRequest("unreadable_file"))
    } else if bytes.starts_with(b"PK") {
        docx_text(bytes).ok_or(ApiError::BadRequest("unreadable_file"))
    } else {
        let t = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
        String::from_utf8(t.to_vec()).map_err(|_| ApiError::BadRequest("unsupported_file"))
    }
}

/// The running text of a .docx: every `<w:t>` run, a new line per paragraph.
fn docx_text(bytes: &[u8]) -> Option<String> {
    use std::io::Read;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let mut xml = String::new();
    // A cap, so a zip bomb cannot fill the memory.
    zip.by_name("word/document.xml")
        .ok()?
        .take(20 * 1024 * 1024)
        .read_to_string(&mut xml)
        .ok()?;
    let mut out = String::new();
    let mut rest = xml.as_str();
    while let Some(i) = rest.find('<') {
        let j = i + rest[i..].find('>')?;
        let tag = &rest[i + 1..j];
        rest = &rest[j + 1..];
        match tag.split([' ', '/']).next().unwrap_or("") {
            "w:t" if !tag.ends_with('/') => {
                let end = rest.find("</w:t>")?;
                out.push_str(&xml_unescape(&rest[..end]));
                rest = &rest[end + "</w:t>".len()..];
            }
            "w:tab" => out.push('\t'),
            "w:br" | "w:cr" => out.push('\n'),
            "" if tag == "/w:p" => out.push('\n'),
            _ => {}
        }
    }
    Some(out)
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn facts_request(name: &str, text: &str) -> ToolRequest {
    ToolRequest {
        model: Model::Haiku,
        system_static: EXTRACT_DOC.to_string(),
        system_dynamic: String::new(),
        messages: vec![Msg {
            role: Role::User,
            text: format!("Document name: {name}\n\n{text}"),
        }],
        tool_name: "save_document_facts".into(),
        tool_description: "The facts about her career that this document states, each with a verbatim quote.".into(),
        tool_schema: json!({
            "type": "object",
            "properties": {
                "facts": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "kind": {"type": "string", "enum": ["role", "employer", "education", "skill", "language", "achievement", "other"]},
                            "text": {"type": "string", "description": "One plain English sentence, at most 25 words."},
                            "snippet": {"type": "string", "description": "The shortest passage that proves the fact, copied character for character from the document."}
                        },
                        "required": ["kind", "text", "snippet"]
                    }
                }
            },
            "required": ["facts"]
        }),
        max_tokens: 4_000,
    }
}

#[derive(Deserialize)]
struct FactDecision {
    confirm: bool,
    /// Her correction: her own words, which count as confirmed.
    #[serde(default)]
    text: Option<String>,
}

/// Confirm (optionally corrected) or reject one of her facts.
async fn decide_fact(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<FactDecision>,
) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let text = b.text.as_deref().map(str::trim).filter(|t| !t.is_empty());
    if text.is_some_and(|t| t.chars().count() > 500) {
        return Err(ApiError::BadRequest("too_long"));
    }
    let (status, text) = if b.confirm {
        ("confirmed", text)
    } else {
        ("rejected", None)
    };
    if !cc_db::set_fact(&st.pool, user.id, id, status, text).await? {
        return Err(ApiError::NotFound);
    }
    list_docs(State(st), h).await
}

// ---------- push-to-talk ----------

#[derive(Deserialize)]
struct Clip {
    #[serde(default)]
    mime: String,
    data: String,
}

/// Her spoken answer goes to the speech-to-text service on the server (language detected, not
/// fixed); the text comes back into her text box for her to check. Nothing is stored or logged.
async fn audio(State(st): State<AppState>, h: HeaderMap, Json(b): Json<Clip>) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    if st.test_only && !user.is_test {
        return Err(ApiError::Unavailable("kompas_offline"));
    }
    let url = st.stt_url.as_deref().ok_or(ApiError::Unavailable("stt_offline"))?;
    let clip = STANDARD
        .decode(b.data.trim())
        .map_err(|_| ApiError::BadRequest("bad_audio"))?;
    if clip.is_empty() {
        return Err(ApiError::BadRequest("bad_audio"));
    }
    if clip.len() > MAX_AUDIO_BYTES {
        return Err(ApiError::BadRequest("too_long"));
    }
    let ext = if b.mime.contains("mp4") || b.mime.contains("aac") {
        "m4a"
    } else if b.mime.contains("ogg") {
        "ogg"
    } else {
        "webm"
    };
    let form = reqwest::multipart::Form::new()
        .part(
            "file",
            reqwest::multipart::Part::bytes(clip).file_name(format!("clip.{ext}")),
        )
        .text("model", st.stt_model.clone());
    let req = reqwest::Client::new()
        .post(format!("{}/v1/audio/transcriptions", url.trim_end_matches('/')))
        .multipart(form)
        .timeout(std::time::Duration::from_secs(60));
    let v: Value = async { req.send().await?.error_for_status()?.json().await }
        .await
        .map_err(|e: reqwest::Error| {
            tracing::warn!("speech-to-text failed: {e}");
            ApiError::Unavailable("stt_offline")
        })?;
    Ok(Json(json!({"text": v["text"].as_str().unwrap_or_default().trim()})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_field_has_a_schema() {
        let all = field_schemas();
        for s in SECTIONS {
            for k in s.2 {
                assert!(all.contains_key(*k), "{k}");
            }
        }
        let v = serde_json::to_value(Profile::default()).unwrap();
        assert_eq!(v.as_object().unwrap().len(), all.len());
    }

    #[test]
    fn prompts_are_long_enough_to_cache() {
        // Sonnet 5.5 and Haiku 5.5 cache prompts of 512 tokens and up; ~4 chars per token.
        assert!(KOMPAS.len() > 4 * 512);
        assert!(EXTRACT.len() > 4 * 512);
        assert!(EXTRACT_DOC.len() > 4 * 512);
    }

    #[test]
    fn files_give_their_text() {
        for f in [
            &include_bytes!("../tests/fixtures/cv.pdf")[..],
            include_bytes!("../tests/fixtures/cv.docx"),
        ] {
            let t = file_text(f).unwrap();
            assert!(t.contains("Projectleider bij Voorbeeld BV, 2015-2020."), "{t}");
        }
        assert_eq!(file_text("\u{feff}plain text".as_bytes()).unwrap(), "plain text");
        assert!(file_text(&[0xff, 0xfe, 0x00]).is_err());
        assert!(file_text(b"PK\x03\x04 not a zip").is_err());
        assert!(file_text(b"%PDF-1.4 broken").is_err());
        assert_eq!(xml_unescape("a &amp;lt; b &lt; c"), "a &lt; b < c");
    }
}
