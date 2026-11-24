//! "Praat met Kompas": the intake conversation (spec §3) and her profile (spec §4).
//!
//! Every route needs her device token AND her session; every query is filtered by her user id.
//! Her answer is saved before Kompas is asked, so nothing she typed is ever lost. Every model
//! call is checked against the monthly cap first and logged in `llm_calls` after (spec §10b).
//! Nothing here logs what she wrote.

use crate::{session_from_headers, ApiError, ApiResult, AppState};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use cc_core::profile::{self, Profile, Progress, PHASES, SECTIONS};
use cc_core::{may_spend, Lane};
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

/// One answer may be long (a story), but not a pasted book.
pub const MAX_ANSWER_CHARS: usize = 6_000;
pub const MAX_DOC_CHARS: usize = 100_000;
/// How much of her confirmed documents Kompas sees in the conversation.
const DOCS_IN_CHAT: usize = 12_000;
/// Every model call is metered under this purpose (Hugo's ruling 2026-11-24).
const PURPOSE: &str = "intake";

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
        .route("/intake/documents", get(list_docs).post(add_doc))
        .route("/intake/documents/{id}", post(decide_doc))
}

async fn who(st: &AppState, h: &HeaderMap) -> ApiResult<cc_db::User> {
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
    let pending_docs = cc_db::documents(&st.pool, user.id)
        .await?
        .iter()
        .filter(|d| d.status == "pending")
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
        "pending_docs": pending_docs,
        "is_test": user.is_test,
        "online": st.llm.is_some(),
    })))
}

async fn view_route(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    view(&st, &user).await
}

// ---------- model calls, metered ----------

async fn ask(st: &AppState, req: &ToolRequest) -> ApiResult<Value> {
    let llm = st.llm.as_ref().ok_or(ApiError::Unavailable("kompas_offline"))?;
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
                purpose: PURPOSE,
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
            let who = if t.speaker == "seeker" { "Zij" } else { "Kompas" };
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

/// Her confirmed documents as `[D1] name:` blocks, cut at `limit` characters in total.
async fn confirmed_docs(st: &AppState, user: Uuid, limit: usize) -> ApiResult<(String, Vec<String>)> {
    let docs = cc_db::documents(&st.pool, user).await?;
    let mut out = String::new();
    let mut ids = vec![];
    for d in docs.iter().filter(|d| d.status == "confirmed") {
        let room = limit.saturating_sub(out.len());
        if room < 200 {
            break;
        }
        let body: String = d.body.chars().take(room).collect();
        out.push_str(&format!("[D{}] {}:\n{}\n\n", d.seq, d.name, body));
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
        "nog geen".to_string()
    } else {
        p.done.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(", ")
    };
    let docs = if docs.is_empty() {
        "Ze heeft (nog) geen documenten gedeeld.".to_string()
    } else {
        format!("## Documenten die zij bevestigde\n{docs}")
    };
    ToolRequest {
        model: Model::Sonnet,
        system_static: KOMPAS.to_string(),
        system_dynamic: format!(
            "## Waar het gesprek nu is\nHuidige fase: {} ({}). Gedekte fases: {done}.\n\n{docs}",
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
        tool_description: "Je volgende bericht aan haar, en of de huidige fase nu genoeg gedekt is.".into(),
        tool_schema: json!({
            "type": "object",
            "properties": {
                "message": {"type": "string", "description": "Je bericht aan haar: kort, warm, één vraag."},
                "phase_complete": {"type": "boolean", "description": "true als de huidige fase met dit antwoord genoeg gedekt is."}
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
        let v = ask(&st, &kompas_request(&[], &progress_of(&s), &docs)).await?;
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
    let v = ask(&st, &kompas_request(&turns, &p, &docs)).await?;
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
    json!({"type": "array", "items": {"type": "string"}, "description": "Bronnen: \"T12\", \"D1\"."})
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
            "start": {"type": "string", "description": "jaar of maand-jaar zoals zij het zei"},
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
            "skill": {"type": "string"}, "rank": {"type": "integer", "minimum": 1, "maximum": 5, "description": "5 = sterkst bewezen, 1 = zwak bewezen"},
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
        json!({"type": "array", "items": text_item(), "description": "3-5 letterlijke zinnen van haar"}),
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
        "## Gesprek
{}

## Documenten
{}",
        transcript(&turns),
        if docs.is_empty() { "(geen)" } else { &docs }
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
        tool_description: "Sla het profiel op dat uit het gesprek volgt.".into(),
        tool_schema: object_schema(keys),
        max_tokens: 8_000,
    };
    let (stories, prefs) = (req(&EXTRACT_STORIES), req(&EXTRACT_PREFS));
    let (mut v, more) = tokio::try_join!(ask(&st, &stories), ask(&st, &prefs))?;
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
                text: format!("## Gesprek\n{}\n\n## Profiel\n{facts}{note}", transcript(&turns)),
            }],
            tool_name: "write_pitch".into(),
            tool_description: "Haar pitch en haar stemproef.".into(),
            tool_schema: object_schema(&["pitch", "voice_sample"]),
            max_tokens: 1_500,
        };
        let v = ask(&st, &req).await?;
        p.pitch = serde_json::from_value(v["pitch"].clone()).unwrap_or_default();
        p.voice_sample = serde_json::from_value(v["voice_sample"].clone()).unwrap_or_default();
        let words = profile::word_count(&p.pitch.text);
        if words <= profile::PITCH_MAX_WORDS {
            break;
        }
        note = format!("\n\nLet op: je vorige pitch was {words} woorden. Blijf ruim onder de 150.");
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
        "## Onderdeel: {title}\n{}\n\n## Haar correctie\n[T{seq}] Zij: {text}",
        Value::Object(part)
    );
    if matches!(b.section.as_str(), "pitch" | "voice") {
        let hers: Vec<String> = turns
            .iter()
            .filter(|t| t.speaker == "seeker")
            .map(|t| format!("[T{}] Zij: {}", t.seq, t.text))
            .collect();
        msg.push_str(&format!("\n\n## Haar beurten\n{}", hers.join("\n")));
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
        tool_description: "Het aangepaste onderdeel.".into(),
        tool_schema: json!({"type": "object", "properties": {"updated": object_schema(keys)}, "required": ["updated"]}),
        max_tokens: 3_000,
    };
    let v = ask(&st, &req).await?;
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

/// "Dit ben ik": locks the draft as an approved version (spec §4).
async fn approve(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let mut s = cc_db::current_session(&st.pool, user.id)
        .await?
        .filter(|s| s.status == "playback")
        .ok_or(ApiError::Conflict("not_in_playback"))?;
    cc_db::approve_profile(&st.pool, user.id)
        .await?
        .ok_or(ApiError::Conflict("no_draft"))?;
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

// ---------- her documents (stub: pasted text or a text file) ----------

async fn list_docs(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let docs = cc_db::documents(&st.pool, user.id).await?;
    Ok(Json(json!(docs
        .iter()
        .map(
            |d| json!({"id": d.id, "label": format!("D{}", d.seq), "name": d.name, "body": d.body, "status": d.status})
        )
        .collect::<Vec<_>>())))
}

#[derive(Deserialize)]
struct NewDoc {
    name: String,
    body: String,
}

async fn add_doc(State(st): State<AppState>, h: HeaderMap, Json(b): Json<NewDoc>) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let name = b.name.trim();
    let body = b.body.trim();
    if body.is_empty() {
        return Err(ApiError::BadRequest("empty"));
    }
    if body.chars().count() > MAX_DOC_CHARS || name.chars().count() > 200 {
        return Err(ApiError::BadRequest("too_long"));
    }
    let name = if name.is_empty() { "Document" } else { name };
    cc_db::add_document(&st.pool, user.id, name, body).await?;
    list_docs(State(st), h).await
}

#[derive(Deserialize)]
struct DocDecision {
    confirm: bool,
}

/// Nothing from a document is used until she confirms it.
async fn decide_doc(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<DocDecision>,
) -> ApiResult<Json<Value>> {
    let user = who(&st, &h).await?;
    let status = if b.confirm { "confirmed" } else { "rejected" };
    if !cc_db::set_document_status(&st.pool, user.id, id, status).await? {
        return Err(ApiError::NotFound);
    }
    list_docs(State(st), h).await
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
    }
}
