//! Outreach and pipeline (Phase 4): three ranked email drafts built from ONE sourced company
//! fact, a deterministic claim check on every version, copy/mailto + a "sent" tap (the app
//! never sends), the pipeline board with dated stage changes, follow-up and parking, and the
//! spoken debrief that only ever proposes new contacts. Locked until the profile is approved.

use crate::intake::ask_as;
use crate::matching::{locked, unlocked};
use crate::{ApiError, ApiResult, AppState};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use cc_core::outreach::{claim_problems, due, linkedin_note, Due};
use cc_llm::{Model, Msg, Role, ToolRequest};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

const OUTREACH: &str = include_str!("prompts/outreach.md");
const DEBRIEF: &str = include_str!("prompts/debrief.md");

pub const STAGES: &[&str] = &[
    "interested",
    "drafted",
    "sent",
    "replied",
    "conversation_planned",
    "conversation_held",
    "follow_up",
    "offer",
    "closed",
    "parked",
];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/outreach/companies/{id}/facts", get(facts))
        .route("/outreach/drafts", post(draft))
        .route("/outreach/drafts/{id}", get(get_draft))
        .route("/outreach/drafts/{id}/tweak", post(tweak))
        .route("/outreach/drafts/{id}/rank", post(rank))
        .route("/outreach/drafts/{id}/sent", post(sent))
        .route("/contacts", get(contacts).post(add_contact))
        .route("/pipeline", get(pipeline).post(add_item))
        .route("/pipeline/{id}/stage", post(stage))
        .route("/pipeline/{id}/debrief", post(debrief))
        .route("/suggestions/{id}", post(suggestion))
}

async fn approved_profile(st: &AppState, user: Uuid) -> ApiResult<Value> {
    Ok(sqlx::query_scalar(
        "SELECT data FROM profiles WHERE user_id = $1 AND status = 'approved' ORDER BY version DESC LIMIT 1",
    )
    .bind(user)
    .fetch_one(&st.pool)
    .await?)
}

/// The company's sourced signals and facts, with ids, so she picks the ONE the email may use.
async fn facts(State(st): State<AppState>, h: HeaderMap, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    if unlocked(&st, &h).await?.is_none() {
        return Ok(locked());
    }
    let rows = sqlx::query(
        "SELECT id, kind || ' ' || dated || ': ' || detail AS text, source_url FROM signals WHERE company_id = $1 \
         UNION ALL SELECT id, field || ': ' || value AS text, source_url FROM company_facts WHERE company_id = $1",
    )
    .bind(id)
    .fetch_all(&st.pool)
    .await?;
    let name: String = sqlx::query_scalar("SELECT name FROM companies WHERE id = $1")
        .bind(id)
        .fetch_optional(&st.pool)
        .await?
        .ok_or(ApiError::NotFound)?;
    let facts: Vec<Value> = rows
        .iter()
        .map(|r| json!({"id": r.get::<Uuid,_>("id"), "text": r.get::<String,_>("text"), "source_url": r.get::<String,_>("source_url")}))
        .collect();
    Ok(Json(json!({"locked": false, "company": name, "facts": facts})))
}

async fn event(st: &AppState, item: Uuid, from: Option<&str>, to: &str, note: &str) -> ApiResult<()> {
    sqlx::query("INSERT INTO pipeline_events (id, item_id, from_stage, to_stage, note) VALUES ($1, $2, $3, $4, $5)")
        .bind(Uuid::new_v4())
        .bind(item)
        .bind(from)
        .bind(to)
        .bind(note)
        .execute(&st.pool)
        .await?;
    sqlx::query("UPDATE pipeline_items SET stage = $2, updated_at = now() WHERE id = $1")
        .bind(item)
        .bind(to)
        .execute(&st.pool)
        .await?;
    Ok(())
}

/// Her pipeline item for this company, created as "interested" the first time.
async fn item_for(st: &AppState, user: Uuid, company: Uuid) -> ApiResult<(Uuid, String)> {
    if let Some(r) = sqlx::query("SELECT id, stage FROM pipeline_items WHERE user_id = $1 AND company_id = $2")
        .bind(user)
        .bind(company)
        .fetch_optional(&st.pool)
        .await?
    {
        return Ok((r.get("id"), r.get("stage")));
    }
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO pipeline_items (id, user_id, company_id) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(user)
        .bind(company)
        .execute(&st.pool)
        .await?;
    event(st, id, None, "interested", "Added to the pipeline").await?;
    Ok((id, "interested".into()))
}

#[derive(Deserialize)]
struct Item {
    company_id: Uuid,
}

async fn add_item(State(st): State<AppState>, h: HeaderMap, Json(b): Json<Item>) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    let (id, stage) = item_for(&st, user, b.company_id).await?;
    Ok(Json(json!({"id": id, "stage": stage})))
}

#[derive(Deserialize)]
struct DraftReq {
    company_id: Uuid,
    /// A signal or company_facts id; ignored for a follow-up (it reuses the first email's fact).
    fact_id: Option<Uuid>,
    #[serde(default)]
    contact_role: String,
    #[serde(default)]
    contact_name: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    times: String,
    #[serde(default)]
    follow_up: bool,
}

/// Everything a draft is built from, as stored on the row.
struct Basis {
    user: Uuid,
    company: String,
    role: String,
    language: String,
    fact: Value,
    times: String,
    follow_up: bool,
    previous: Option<String>,
    tweaks: Vec<String>,
}

impl Basis {
    fn sourced(&self) -> String {
        format!(
            "{} {} {}",
            self.fact["text"].as_str().unwrap_or(""),
            self.company,
            self.role
        )
    }
}

async fn generate(st: &AppState, h: &HeaderMap, b: &Basis) -> ApiResult<Value> {
    let user = crate::intake::who(st, h).await?;
    let profile = approved_profile(st, b.user).await?;
    // Her ranking feeds later drafts: the tones she put first on her last five ranked drafts.
    let liked: Vec<String> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT versions -> (ranking ->> 0)::int ->> 'tone' FROM outreach_drafts \
         WHERE user_id = $1 AND ranking IS NOT NULL ORDER BY created_at DESC LIMIT 5",
    )
    .bind(b.user)
    .fetch_all(&st.pool)
    .await?
    .into_iter()
    .flatten()
    .collect();
    let lang = if b.language == "nl" { "Dutch" } else { "English" };
    let mut ask =
        format!(
        "Language: {lang}.\nCompany: {}\nContact: {}\nThe ONE fact you may use about the company: {} (source: {})\n\
         Her free times: {}\nTones she ranked first before: {}\n",
        b.company,
        if b.role.is_empty() { "unknown role" } else { &b.role },
        b.fact["text"].as_str().unwrap_or(""),
        b.fact["source_url"].as_str().unwrap_or(""),
        if b.times.is_empty() { "not given" } else { &b.times },
        if liked.is_empty() { "none yet".to_string() } else { liked.join(", ") },
    );
    if b.follow_up {
        ask += &format!(
            "\nThis is a FOLLOW-UP. Her first email was:\n{}\n",
            b.previous.as_deref().unwrap_or("")
        );
    }
    if !b.tweaks.is_empty() {
        ask += &format!("\nShe asked for these changes: {}\n", b.tweaks.join("; "));
    }
    let req = ToolRequest {
        model: Model::Sonnet,
        system_static: OUTREACH.to_string(),
        system_dynamic: format!("## Her approved profile\n{profile}"),
        messages: vec![Msg {
            role: Role::User,
            text: ask,
        }],
        tool_name: "drafts".into(),
        tool_description: "Three ranked email versions, a LinkedIn note and a phone opener.".into(),
        tool_schema: json!({"type": "object", "properties": {
            "versions": {"type": "array", "items": {"type": "object", "properties": {
                "tone": {"type": "string"}, "subject": {"type": "string"}, "body": {"type": "string"}}}},
            "linkedin": {"type": "string"},
            "phone": {"type": "string"}}}),
        max_tokens: 3_000,
    };
    let v = ask_as(st, &user, &req, "outreach").await?;
    let hers = format!("{profile} {}", b.times);
    let versions: Vec<Value> = v["versions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(3)
        .map(|x| {
            let text = format!(
                "{}\n{}",
                x["subject"].as_str().unwrap_or(""),
                x["body"].as_str().unwrap_or("")
            );
            json!({"tone": x["tone"], "subject": x["subject"], "body": x["body"],
                   "problems": claim_problems(&text, &b.sourced(), &hers)})
        })
        .collect();
    Ok(
        json!({"versions": versions, "linkedin": linkedin_note(v["linkedin"].as_str().unwrap_or("")),
              "phone": v["phone"]}),
    )
}

async fn draft(State(st): State<AppState>, h: HeaderMap, Json(r): Json<DraftReq>) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    let company: String = sqlx::query_scalar("SELECT name FROM companies WHERE id = $1")
        .bind(r.company_id)
        .fetch_optional(&st.pool)
        .await?
        .ok_or(ApiError::NotFound)?;
    let (item, stage) = item_for(&st, user, r.company_id).await?;
    let mut b = Basis {
        user,
        company,
        role: format!("{} {}", r.contact_name, r.contact_role).trim().to_string(),
        language: r.language.unwrap_or_else(|| "en".into()),
        fact: Value::Null,
        times: r.times,
        follow_up: r.follow_up,
        previous: None,
        tweaks: vec![],
    };
    if r.follow_up {
        let last = sqlx::query(
            "SELECT contact_role, language, fact, times, sent_text FROM outreach_drafts \
             WHERE item_id = $1 AND sent_at IS NOT NULL ORDER BY sent_at DESC LIMIT 1",
        )
        .bind(item)
        .fetch_optional(&st.pool)
        .await?
        .ok_or(ApiError::BadRequest("nothing_sent_yet"))?;
        b.role = last.get("contact_role");
        b.language = last.get("language");
        b.fact = last.get("fact");
        b.times = last.get("times");
        b.previous = last.get("sent_text");
    } else {
        let id = r.fact_id.ok_or(ApiError::BadRequest("pick_a_fact"))?;
        let f = sqlx::query(
            "SELECT kind || ' ' || dated || ': ' || detail AS text, source_url FROM signals WHERE id = $1 AND company_id = $2 \
             UNION ALL SELECT field || ': ' || value, source_url FROM company_facts WHERE id = $1 AND company_id = $2",
        )
        .bind(id)
        .bind(r.company_id)
        .fetch_optional(&st.pool)
        .await?
        .ok_or(ApiError::BadRequest("fact_not_sourced"))?;
        b.fact = json!({"text": f.get::<String,_>("text"), "source_url": f.get::<String,_>("source_url")});
    }
    let out = generate(&st, &h, &b).await?;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO outreach_drafts (id, user_id, item_id, follow_up, contact_role, language, fact, times, versions, linkedin, phone) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(id)
    .bind(user)
    .bind(item)
    .bind(b.follow_up)
    .bind(&b.role)
    .bind(&b.language)
    .bind(&b.fact)
    .bind(&b.times)
    .bind(&out["versions"])
    .bind(out["linkedin"].as_str().unwrap_or(""))
    .bind(out["phone"].as_str().unwrap_or(""))
    .execute(&st.pool)
    .await?;
    if stage == "interested" {
        event(&st, item, Some(&stage), "drafted", "First email drafted").await?;
    }
    get_draft(State(st), h, Path(id)).await
}

async fn draft_row(st: &AppState, user: Uuid, id: Uuid) -> ApiResult<sqlx::postgres::PgRow> {
    sqlx::query(
        "SELECT d.*, c.name AS company FROM outreach_drafts d JOIN pipeline_items p ON p.id = d.item_id \
         JOIN companies c ON c.id = p.company_id WHERE d.id = $1 AND d.user_id = $2",
    )
    .bind(id)
    .bind(user)
    .fetch_optional(&st.pool)
    .await?
    .ok_or(ApiError::NotFound)
}

async fn get_draft(State(st): State<AppState>, h: HeaderMap, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    let r = draft_row(&st, user, id).await?;
    Ok(Json(json!({
        "locked": false, "id": id, "item_id": r.get::<Uuid,_>("item_id"), "company": r.get::<String,_>("company"),
        "follow_up": r.get::<bool,_>("follow_up"), "language": r.get::<String,_>("language"),
        "fact": r.get::<Value,_>("fact"), "versions": r.get::<Value,_>("versions"),
        "linkedin": r.get::<String,_>("linkedin"), "phone": r.get::<String,_>("phone"),
        "tweaks": r.get::<Value,_>("tweaks"), "ranking": r.get::<Option<Value>,_>("ranking"),
        "sent_at": r.get::<Option<DateTime<Utc>>,_>("sent_at"),
    })))
}

#[derive(Deserialize)]
struct Tweak {
    ask: String,
}

/// "Warmer", "shorter": regenerate all three versions with every change she asked for so far.
async fn tweak(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(t): Json<Tweak>,
) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    let r = draft_row(&st, user, id).await?;
    let mut tweaks: Vec<String> = serde_json::from_value(r.get("tweaks")).unwrap_or_default();
    tweaks.push(t.ask.trim().chars().take(200).collect());
    let b = Basis {
        user,
        company: r.get("company"),
        role: r.get("contact_role"),
        language: r.get("language"),
        fact: r.get("fact"),
        times: r.get("times"),
        follow_up: r.get("follow_up"),
        previous: None,
        tweaks: tweaks.clone(),
    };
    let out = generate(&st, &h, &b).await?;
    sqlx::query("UPDATE outreach_drafts SET versions = $2, linkedin = $3, phone = $4, tweaks = $5, ranking = NULL WHERE id = $1")
        .bind(id)
        .bind(&out["versions"])
        .bind(out["linkedin"].as_str().unwrap_or(""))
        .bind(out["phone"].as_str().unwrap_or(""))
        .bind(json!(tweaks))
        .execute(&st.pool)
        .await?;
    get_draft(State(st), h, Path(id)).await
}

#[derive(Deserialize)]
struct Rank {
    order: Vec<usize>,
}

async fn rank(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<Rank>,
) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    let n = draft_row(&st, user, id)
        .await?
        .get::<Value, _>("versions")
        .as_array()
        .map_or(0, Vec::len);
    if b.order.is_empty() || b.order.iter().any(|&i| i >= n) {
        return Err(ApiError::BadRequest("ranking"));
    }
    sqlx::query("UPDATE outreach_drafts SET ranking = $2 WHERE id = $1")
        .bind(id)
        .bind(json!(b.order))
        .execute(&st.pool)
        .await?;
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct Sent {
    text: String,
}

/// Her "I sent it" tap. The final text (as she edited it) is checked again; a text that names
/// something the sources don't is not recorded, she gets the problems back to fix first.
async fn sent(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(s): Json<Sent>,
) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    let r = draft_row(&st, user, id).await?;
    if r.get::<Option<DateTime<Utc>>, _>("sent_at").is_some() {
        return Err(ApiError::Conflict("already_sent"));
    }
    let fact: Value = r.get("fact");
    let sourced = format!(
        "{} {} {}",
        fact["text"].as_str().unwrap_or(""),
        r.get::<String, _>("company"),
        r.get::<String, _>("contact_role")
    );
    let hers = format!("{} {}", approved_profile(&st, user).await?, r.get::<String, _>("times"));
    let problems = claim_problems(&s.text, &sourced, &hers);
    if !problems.is_empty() {
        return Ok(Json(json!({"ok": false, "problems": problems})));
    }
    let follow_up: bool = r.get("follow_up");
    let item: Uuid = r.get("item_id");
    sqlx::query("UPDATE outreach_drafts SET sent_text = $2, sent_at = now() WHERE id = $1")
        .bind(id)
        .bind(&s.text)
        .execute(&st.pool)
        .await?;
    let from: String = sqlx::query_scalar(
        "UPDATE pipeline_items p SET last_sent_at = now(), follow_ups_sent = p.follow_ups_sent + $2 \
         FROM pipeline_items o WHERE p.id = o.id AND p.id = $1 RETURNING o.stage",
    )
    .bind(item)
    .bind(follow_up as i32)
    .fetch_one(&st.pool)
    .await?;
    let (to, note) = if follow_up {
        ("follow_up", "Follow-up sent")
    } else {
        ("sent", "First email sent")
    };
    event(&st, item, Some(&from), to, note).await?;
    Ok(Json(json!({"ok": true, "stage": to})))
}

async fn contacts(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    let rows = sqlx::query(
        "SELECT id, company_id, name, role, company_name, source FROM contacts WHERE user_id = $1 ORDER BY created_at DESC",
    )
    .bind(user)
    .fetch_all(&st.pool)
    .await?;
    let list: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({"id": r.get::<Uuid,_>("id"), "company_id": r.get::<Option<Uuid>,_>("company_id"),
            "name": r.get::<String,_>("name"), "role": r.get::<String,_>("role"),
            "company": r.get::<String,_>("company_name"), "source": r.get::<String,_>("source")})
        })
        .collect();
    Ok(Json(json!({"locked": false, "contacts": list})))
}

#[derive(Deserialize)]
struct NewContact {
    name: String,
    #[serde(default)]
    role: String,
    company_id: Option<Uuid>,
    #[serde(default)]
    company: String,
    #[serde(default)]
    source: String,
}

async fn insert_contact(st: &AppState, user: Uuid, c: &NewContact) -> ApiResult<Uuid> {
    if c.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name"));
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO contacts (id, user_id, company_id, name, role, company_name, source) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(user)
    .bind(c.company_id)
    .bind(c.name.trim())
    .bind(c.role.trim())
    .bind(c.company.trim())
    .bind(c.source.trim())
    .execute(&st.pool)
    .await?;
    Ok(id)
}

async fn add_contact(State(st): State<AppState>, h: HeaderMap, Json(c): Json<NewContact>) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    Ok(Json(json!({"id": insert_contact(&st, user, &c).await?})))
}

/// The board. Applies the clock on the way: 10 working days without a reply suggests one
/// follow-up; no reply to that either parks the item.
async fn pipeline(State(st): State<AppState>, h: HeaderMap) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    let today = Utc::now().date_naive();
    let rows = sqlx::query(
        "SELECT p.id, p.company_id, c.name, p.stage, p.last_sent_at, p.follow_ups_sent, p.updated_at FROM pipeline_items p \
         JOIN companies c ON c.id = p.company_id WHERE p.user_id = $1 ORDER BY p.updated_at DESC",
    )
    .bind(user)
    .fetch_all(&st.pool)
    .await?;
    let mut items = Vec::new();
    for r in &rows {
        let id: Uuid = r.get("id");
        let mut stage: String = r.get("stage");
        let mut follow_up_due = false;
        if let (Some(last), true) = (
            r.get::<Option<DateTime<Utc>>, _>("last_sent_at"),
            stage == "sent" || stage == "follow_up",
        ) {
            match due(last.date_naive(), r.get("follow_ups_sent"), today) {
                Due::Park => {
                    event(&st, id, Some(&stage), "parked", "No reply twice; parked").await?;
                    stage = "parked".into();
                }
                Due::FollowUp => follow_up_due = true,
                Due::Wait => {}
            }
        }
        let history = sqlx::query(
            "SELECT from_stage, to_stage, note, created_at FROM pipeline_events WHERE item_id = $1 ORDER BY created_at",
        )
        .bind(id)
        .fetch_all(&st.pool)
        .await?;
        let history: Vec<Value> = history
            .iter()
            .map(|e| {
                json!({"from": e.get::<Option<String>,_>("from_stage"), "to": e.get::<String,_>("to_stage"),
                "note": e.get::<String,_>("note"), "at": e.get::<DateTime<Utc>,_>("created_at")})
            })
            .collect();
        items.push(
            json!({"id": id, "company_id": r.get::<Uuid,_>("company_id"), "company": r.get::<String,_>("name"),
            "stage": stage, "follow_up_due": follow_up_due, "history": history}),
        );
    }
    let sugg = sqlx::query(
        "SELECT id, item_id, kind, data FROM suggestions WHERE user_id = $1 AND status = 'open' ORDER BY created_at",
    )
    .bind(user)
    .fetch_all(&st.pool)
    .await?;
    let sugg: Vec<Value> = sugg
        .iter()
        .map(|s| {
            json!({"id": s.get::<Uuid,_>("id"), "item_id": s.get::<Option<Uuid>,_>("item_id"),
            "kind": s.get::<String,_>("kind"), "data": s.get::<Value,_>("data")})
        })
        .collect();
    Ok(Json(
        json!({"locked": false, "stages": STAGES, "items": items, "suggestions": sugg}),
    ))
}

async fn own_item(st: &AppState, user: Uuid, id: Uuid) -> ApiResult<String> {
    sqlx::query_scalar("SELECT stage FROM pipeline_items WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user)
        .fetch_optional(&st.pool)
        .await?
        .ok_or(ApiError::NotFound)
}

#[derive(Deserialize)]
struct StageReq {
    stage: String,
    note: String,
}

async fn stage(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<StageReq>,
) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    if !STAGES.contains(&b.stage.as_str()) {
        return Err(ApiError::BadRequest("stage"));
    }
    if b.note.trim().is_empty() {
        return Err(ApiError::BadRequest("note_required"));
    }
    let from = own_item(&st, user, id).await?;
    event(&st, id, Some(&from), &b.stage, b.note.trim()).await?;
    Ok(Json(json!({"ok": true, "stage": b.stage})))
}

#[derive(Deserialize)]
struct Debrief {
    text: String,
}

/// After a conversation: her spoken account (already text via /intake/audio) goes to Haiku,
/// which proposes new people and companies. Nothing is added until she confirms each one.
async fn debrief(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(d): Json<Debrief>,
) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    own_item(&st, user, id).await?;
    if d.text.trim().is_empty() {
        return Err(ApiError::BadRequest("empty"));
    }
    let me = crate::intake::who(&st, &h).await?;
    let req = ToolRequest {
        model: Model::Haiku,
        system_static: DEBRIEF.to_string(),
        system_dynamic: String::new(),
        messages: vec![Msg {
            role: Role::User,
            text: d.text.chars().take(8_000).collect(),
        }],
        tool_name: "debrief".into(),
        tool_description: "What she said happened, and the new people and companies she mentioned.".into(),
        tool_schema: json!({"type": "object", "properties": {
            "summary": {"type": "string"}, "next_step": {"type": "string"},
            "suggestions": {"type": "array", "items": {"type": "object", "properties": {
                "kind": {"type": "string", "enum": ["contact", "company"]}, "name": {"type": "string"},
                "role": {"type": "string"}, "company": {"type": "string"}, "website": {"type": "string"},
                "note": {"type": "string"}}}}}}),
        max_tokens: 1_500,
    };
    let v = ask_as(&st, &me, &req, "debrief").await?;
    let mut out = Vec::new();
    for s in v["suggestions"].as_array().cloned().unwrap_or_default() {
        let kind = if s["kind"] == "company" { "company" } else { "contact" };
        if s["name"].as_str().unwrap_or("").trim().is_empty() {
            continue;
        }
        let sid = Uuid::new_v4();
        sqlx::query("INSERT INTO suggestions (id, user_id, item_id, kind, data) VALUES ($1, $2, $3, $4, $5)")
            .bind(sid)
            .bind(user)
            .bind(id)
            .bind(kind)
            .bind(&s)
            .execute(&st.pool)
            .await?;
        out.push(json!({"id": sid, "kind": kind, "data": s}));
    }
    Ok(Json(
        json!({"summary": v["summary"], "next_step": v["next_step"], "suggestions": out}),
    ))
}

#[derive(Deserialize)]
struct Confirm {
    add: bool,
    website: Option<String>,
}

async fn suggestion(
    State(st): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(c): Json<Confirm>,
) -> ApiResult<Json<Value>> {
    let Some(user) = unlocked(&st, &h).await? else {
        return Ok(locked());
    };
    let r = sqlx::query("SELECT kind, data FROM suggestions WHERE id = $1 AND user_id = $2 AND status = 'open'")
        .bind(id)
        .bind(user)
        .fetch_optional(&st.pool)
        .await?
        .ok_or(ApiError::NotFound)?;
    let (kind, data): (String, Value) = (r.get("kind"), r.get("data"));
    let text = |k: &str| data[k].as_str().unwrap_or("").to_string();
    let mut added = Value::Null;
    if c.add {
        if kind == "company" {
            let site = c
                .website
                .filter(|w| !w.trim().is_empty())
                .unwrap_or_else(|| text("website"));
            if site.trim().is_empty() {
                return Err(ApiError::BadRequest("website_needed"));
            }
            let (cid, new) = cc_db::add_company(&st.pool, &text("name"), site.trim(), Some(user)).await?;
            if new {
                cc_db::enqueue_enrich(&st.pool, cid, false).await?;
            }
            added = json!(cid);
        } else {
            let c = NewContact {
                name: text("name"),
                role: text("role"),
                company_id: None,
                company: text("company"),
                source: format!("debrief: {}", text("note")),
            };
            added = json!(insert_contact(&st, user, &c).await?);
        }
    }
    sqlx::query("UPDATE suggestions SET status = $2 WHERE id = $1")
        .bind(id)
        .bind(if c.add { "added" } else { "dismissed" })
        .execute(&st.pool)
        .await?;
    Ok(Json(json!({"ok": true, "added": added})))
}
