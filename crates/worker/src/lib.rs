//! Background worker (Phase 2): enriches the company base from each company's own pages.
//! A tokio task inside the api. Jobs: `enrich_company {company_id, force}` and the weekly
//! `refresh_signals`. Every stored fact has passed the snippet guard; the model call runs in
//! the background lane, so it pauses first when the monthly cap is reached.

use cc_core::company::{event_signal, headcount_growth, headcount_number, new_ops_roles, parse_date, GROWTH_SIGNAL};
use cc_core::{may_spend, quote_in, Lane};
use cc_enrich::{discover, page_text, FetchError, Fetcher, Url};
use cc_llm::{LlmError, Model, Msg, Role, ToolRequest};
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

const PROMPT: &str = include_str!("extract_company.md");
const PURPOSE: &str = "enrichment";
const MAX_ATTEMPTS: i32 = 3;
const NOMINATIM: &str = "https://nominatim.openstreetmap.org/search";
const FACT_FIELDS: [&str; 9] = [
    "legal_name",
    "kvk_number",
    "mission",
    "customer",
    "headcount",
    "address",
    "product",
    "founded",
    "other",
];
const EVENT_KINDS: [&str; 9] = [
    "funding",
    "regulatory",
    "contract",
    "leadership",
    "new_country",
    "product",
    "partnership",
    "award",
    "other",
];

pub struct Worker {
    pub pool: PgPool,
    pub llm: cc_llm::Client,
    pub eur_per_usd: f64,
    pub fetch: Fetcher,
}

enum Outcome {
    Done,
    Retry(String),
    Fail(String),
    /// The monthly cap: try again later without using up an attempt.
    Paused,
}

/// Runs forever: takes due jobs, sleeps 30 s when there are none.
pub async fn run(w: Worker) {
    let _ = cc_db::requeue_running(&w.pool).await;
    let _ = cc_db::enqueue(&w.pool, "refresh_signals", json!({}), Utc::now() + Duration::days(7)).await;
    loop {
        match cc_db::claim_job(&w.pool).await {
            Ok(Some(job)) => {
                if let Err(e) = w.handle(&job).await {
                    tracing::warn!("job {} ({}) failed on the database: {e}", job.id, job.kind);
                    let _ = cc_db::end_job(
                        &w.pool,
                        job.id,
                        Some("database error"),
                        Some(Utc::now() + Duration::minutes(10)),
                        false,
                    )
                    .await;
                }
            }
            Ok(None) => tokio::time::sleep(std::time::Duration::from_secs(30)).await,
            Err(e) => {
                tracing::warn!("job queue unreachable: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        }
    }
}

impl Worker {
    async fn handle(&self, job: &cc_db::Job) -> Result<(), sqlx::Error> {
        if job.kind == "refresh_signals" {
            // Weekly: re-read every site (unchanged pages cost nothing); a full re-extraction monthly.
            for (id, last) in cc_db::company_ids(&self.pool).await? {
                let stale = last.is_none_or(|t| t < Utc::now() - Duration::days(30));
                cc_db::enqueue_enrich(&self.pool, id, stale).await?;
            }
            cc_db::end_job(&self.pool, job.id, None, None, false).await?;
            return cc_db::enqueue(&self.pool, "refresh_signals", json!({}), Utc::now() + Duration::days(7)).await;
        }
        let id = job.payload["company_id"].as_str().and_then(|s| s.parse::<Uuid>().ok());
        let (Some(id), "enrich_company") = (id, job.kind.as_str()) else {
            return cc_db::end_job(&self.pool, job.id, Some("unknown job"), None, false).await;
        };
        sqlx::query("UPDATE companies SET status = 'running' WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        let outcome = self.enrich(id, job.payload["force"].as_bool().unwrap_or(false)).await?;
        let (status, error, retry, refund) = match &outcome {
            Outcome::Done => ("enriched", None, None, false),
            Outcome::Paused => (
                "paused_cap",
                Some("monthly cap reached"),
                Some(Utc::now() + Duration::hours(6)),
                true,
            ),
            Outcome::Retry(e) if job.attempts < MAX_ATTEMPTS => (
                "retrying",
                Some(e.as_str()),
                Some(Utc::now() + Duration::minutes(15 << job.attempts)),
                false,
            ),
            Outcome::Retry(e) | Outcome::Fail(e) => ("failed", Some(e.as_str()), None, false),
        };
        sqlx::query("UPDATE companies SET status = $2, last_error = $3 WHERE id = $1")
            .bind(id)
            .bind(status)
            .bind(error)
            .execute(&self.pool)
            .await?;
        cc_db::end_job(&self.pool, job.id, error, retry, refund).await
    }

    async fn enrich(&self, id: Uuid, force: bool) -> Result<Outcome, sqlx::Error> {
        let c = sqlx::query("SELECT name, website, city, content_hash, lat FROM companies WHERE id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        let (name, website): (String, String) = (c.get("name"), c.get("website"));
        let Ok(home) = Url::parse(&website) else {
            return Ok(Outcome::Fail("bad website".into()));
        };

        // 1. The pages, from the site's own links.
        let html = match self.fetch.page(&home).await {
            Ok(h) => h,
            Err(e @ (FetchError::Robots | FetchError::Banned)) => return Ok(Outcome::Fail(e.to_string())),
            Err(e) => return Ok(Outcome::Retry(e.to_string())),
        };
        let mut pages = vec![("home", home.clone(), page_text(&html))];
        for (kind, url) in discover(&home, &html) {
            match self.fetch.page(&url).await {
                Ok(h) => pages.push((kind, url, page_text(&h))),
                Err(e) => tracing::info!("{url}: {e}"),
            }
        }

        // 2. Snapshots; a new ops role on the careers page is a signal, free of any model call.
        let mut hashes = String::new();
        for (kind, url, text) in &pages {
            let sha = format!("{:x}", Sha256::digest(text.as_bytes()));
            hashes.push_str(&format!("{url} {sha}\n"));
            let old: Option<String> =
                sqlx::query_scalar("SELECT text FROM page_snapshots WHERE company_id = $1 AND url = $2")
                    .bind(id)
                    .bind(url.as_str())
                    .fetch_optional(&self.pool)
                    .await?;
            if *kind == "careers" {
                for role in new_ops_roles(old.as_deref().unwrap_or(""), text) {
                    let detail = format!("New {role} role on the careers page");
                    insert_signal(&self.pool, id, "ops_roles", &today(), &detail, url.as_str(), None).await?;
                }
            }
            sqlx::query(
                "INSERT INTO page_snapshots (company_id, url, sha256, text) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (company_id, url) DO UPDATE SET sha256 = $3, text = $4, fetched_at = now()",
            )
            .bind(id)
            .bind(url.as_str())
            .bind(&sha)
            .bind(text)
            .execute(&self.pool)
            .await?;
        }
        let content_hash = format!("{:x}", Sha256::digest(hashes.as_bytes()));
        sqlx::query("UPDATE companies SET last_fetched_at = now() WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if !force && c.get::<Option<String>, _>("content_hash").as_deref() == Some(content_hash.as_str()) {
            return Ok(Outcome::Done);
        }

        // 3. One Haiku call, background lane.
        let req = request(&name, &website, &pages);
        let spent = cc_db::month_spend(&self.pool).await?;
        let cap = cc_db::cap_eur(&self.pool).await?;
        if !may_spend(spent, req.estimate_usd() * self.eur_per_usd, cap, Lane::Background) {
            return Ok(Outcome::Paused);
        }
        let res = self.llm.call(&req).await;
        if let Ok((_, u)) | Err(LlmError::BadAnswer(_, u)) = &res {
            cc_db::log_llm_call(
                &self.pool,
                &cc_db::LlmCall {
                    purpose: PURPOSE,
                    model: req.model.id(),
                    tokens_in: u.prompt_tokens() as i32,
                    tokens_out: u.output as i32,
                    cache_read: u.cache_read as i32,
                    cache_write: u.cache_write as i32,
                    cost_eur: u.cost_usd(req.model) * self.eur_per_usd,
                },
            )
            .await?;
        }
        let answer = match res {
            Ok((v, _)) => v,
            Err(e) => return Ok(Outcome::Retry(e.to_string())),
        };
        let texts: Vec<(String, String)> = pages.iter().map(|(_, u, t)| (u.to_string(), t.clone())).collect();
        let s = sift(&answer, &texts);

        // 4. Store it in one go; seed facts and the headcount history stay.
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM company_facts WHERE company_id = $1 AND origin = 'site' AND field <> 'headcount'")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for f in &s.facts {
            sqlx::query(
                "INSERT INTO company_facts (id, company_id, field, value, source_url, snippet) \
                 VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
            )
            .bind(Uuid::new_v4())
            .bind(id)
            .bind(&f.field)
            .bind(&f.value)
            .bind(&f.url)
            .bind(&f.quote)
            .execute(&mut *tx)
            .await?;
        }
        for e in &s.events {
            let ev: Option<Uuid> = sqlx::query_scalar(
                "INSERT INTO company_events (id, company_id, date, kind, title, source_url, snippet) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(id)
            .bind(&e.date)
            .bind(&e.kind)
            .bind(&e.value)
            .bind(&e.url)
            .bind(&e.quote)
            .fetch_optional(&mut *tx)
            .await?;
            if let (Some(ev), Some(kind)) = (ev, event_signal(&e.kind, &e.value)) {
                insert_signal(&mut *tx, id, kind, &e.date, &e.value, &e.url, Some(ev)).await?;
            }
        }
        sqlx::query("DELETE FROM people WHERE company_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for p in &s.people {
            let role = p.value.to_lowercase();
            let contact = ["ceo", "coo", "operations", "founder", "managing director"]
                .iter()
                .any(|k| role.contains(k));
            sqlx::query(
                "INSERT INTO people (id, company_id, name, role, source_url, snippet, suggested_contact) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(Uuid::new_v4())
            .bind(id)
            .bind(&p.field)
            .bind(&p.value)
            .bind(&p.url)
            .bind(&p.quote)
            .bind(contact)
            .execute(&mut *tx)
            .await?;
        }
        // Headcount growth over the stated history.
        let rows = sqlx::query("SELECT value, source_url, fetched_at FROM company_facts WHERE company_id = $1 AND field = 'headcount' ORDER BY fetched_at")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
        let history: Vec<_> = rows
            .iter()
            .filter_map(|r| {
                Some((
                    r.get::<chrono::DateTime<Utc>, _>("fetched_at").date_naive(),
                    headcount_number(r.get("value"))?,
                ))
            })
            .collect();
        if let (Some(g), Some(last)) = (headcount_growth(&history), rows.last()) {
            if g > GROWTH_SIGNAL {
                let detail = format!("Headcount up {:.0} % a year", g * 100.0);
                insert_signal(
                    &mut *tx,
                    id,
                    "headcount_growth",
                    &today(),
                    &detail,
                    last.get("source_url"),
                    None,
                )
                .await?;
            }
        }
        let field = |k: &str| s.facts.iter().find(|f| f.field == k).map(|f| f.value.clone());
        // A company added by URL is named after its host until its pages name it.
        let rename = s.name.clone().filter(|_| home.host_str() == Some(name.as_str()));
        sqlx::query(
            "UPDATE companies SET name = COALESCE($2, name), kvk_number = $3, mission_summary = $4, content_hash = $5, \
             dropped = $6, last_enriched_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(rename)
        .bind(field("kvk_number"))
        .bind(field("mission"))
        .bind(&content_hash)
        .bind(s.dropped as i32)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        // 5. Where it is: OpenStreetMap's Nominatim, once.
        if c.get::<Option<f64>, _>("lat").is_none() {
            let city: Option<String> = c.get("city");
            if let Some(q) = field("address").or(city.map(|c| format!("{c}, Netherlands"))) {
                self.geocode(id, &q).await?;
            }
        }
        Ok(Outcome::Done)
    }

    async fn geocode(&self, id: Uuid, q: &str) -> Result<(), sqlx::Error> {
        let Ok(url) = Url::parse_with_params(
            NOMINATIM,
            [("format", "jsonv2"), ("limit", "1"), ("countrycodes", "nl"), ("q", q)],
        ) else {
            return Ok(());
        };
        let body = match self.fetch.api(&url, std::time::Duration::from_secs(1)).await {
            Ok(b) => b,
            Err(e) => {
                tracing::info!("geocode failed: {e}");
                return Ok(());
            }
        };
        let hit: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let num = |k: &str| hit[0][k].as_str().and_then(|s| s.parse::<f64>().ok());
        if let (Some(lat), Some(lon)) = (num("lat"), num("lon")) {
            sqlx::query("UPDATE companies SET lat = $2, lon = $3, geo_source_url = $4 WHERE id = $1")
                .bind(id)
                .bind(lat)
                .bind(lon)
                .bind(url.as_str())
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }
}

async fn insert_signal<'e, E: sqlx::PgExecutor<'e>>(
    db: E,
    id: Uuid,
    kind: &str,
    dated: &str,
    detail: &str,
    url: &str,
    event: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO signals (id, company_id, kind, dated, detail, source_url, event_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .bind(kind)
    .bind(dated)
    .bind(detail)
    .bind(url)
    .bind(event)
    .execute(db)
    .await?;
    Ok(())
}

fn today() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

fn request(name: &str, website: &str, pages: &[(&str, Url, String)]) -> ToolRequest {
    let text = pages
        .iter()
        .enumerate()
        .map(|(i, (_, u, t))| format!("[P{}] {u}\n{t}\n", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let item = |extra: Value| {
        let mut o = json!({"type": "object", "properties": {"page": {"type": "integer"}, "quote": {"type": "string"}}});
        let props = o["properties"].as_object_mut().expect("object");
        for (k, v) in extra.as_object().expect("object") {
            props.insert(k.clone(), v.clone());
        }
        let keys: Vec<&String> = props.keys().collect();
        o["required"] = json!(keys);
        json!({"type": "array", "items": o})
    };
    ToolRequest {
        model: Model::Haiku,
        system_static: PROMPT.into(),
        system_dynamic: format!("Company: {name}\nSite: {website}"),
        messages: vec![Msg { role: Role::User, text }],
        tool_name: "company".into(),
        tool_description: "What the company's own pages state, each item with its page and verbatim quote.".into(),
        tool_schema: json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "facts": item(json!({"field": {"type": "string", "enum": FACT_FIELDS}, "value": {"type": "string"}})),
                "events": item(json!({"date": {"type": "string"}, "kind": {"type": "string", "enum": EVENT_KINDS}, "title": {"type": "string"}})),
                "people": item(json!({"name": {"type": "string"}, "role": {"type": "string"}})),
            },
            "required": ["name", "facts", "events", "people"],
        }),
        max_tokens: 4000,
    }
}

/// One kept item: `field` is the fact field (or a person's name), `value` the value (event
/// title, person's role), `url` the page it is quoted from.
#[derive(Debug, PartialEq)]
struct Item {
    field: String,
    value: String,
    date: String,
    kind: String,
    url: String,
    quote: String,
}

#[derive(Debug, Default)]
struct Sifted {
    name: Option<String>,
    facts: Vec<Item>,
    events: Vec<Item>,
    people: Vec<Item>,
    dropped: usize,
}

/// The honesty guard: an item stays only if its quote is on the page it names (and a person's
/// name is there too). Everything else is dropped and counted.
fn sift(answer: &Value, pages: &[(String, String)]) -> Sifted {
    let mut s = Sifted::default();
    let page = |v: &Value| {
        let n = v["page"].as_u64()? as usize;
        let (url, text) = pages.get(n.checked_sub(1)?)?;
        let quote = v["quote"].as_str()?.trim().to_string();
        quote_in(text, &quote).then(|| (url.clone(), quote, text))
    };
    let str_of = |v: &Value, k: &str| v[k].as_str().unwrap_or("").trim().to_string();
    let list = |k: &str| answer[k].as_array().cloned().unwrap_or_default();
    for v in list("facts") {
        let (field, value) = (str_of(&v, "field"), str_of(&v, "value"));
        let valid = !value.is_empty()
            && FACT_FIELDS.contains(&field.as_str())
            && (field != "kvk_number" || (value.len() == 8 && value.bytes().all(|b| b.is_ascii_digit())))
            && (field != "headcount" || headcount_number(&value).is_some());
        match page(&v).filter(|_| valid) {
            Some((url, quote, _)) => s.facts.push(Item {
                field,
                value,
                date: String::new(),
                kind: String::new(),
                url,
                quote,
            }),
            None => s.dropped += 1,
        }
    }
    for v in list("events") {
        let (kind, title, date) = (str_of(&v, "kind"), str_of(&v, "title"), norm_date(&str_of(&v, "date")));
        match (page(&v), date) {
            (Some((url, quote, _)), Some(date)) if !title.is_empty() && EVENT_KINDS.contains(&kind.as_str()) => {
                s.events.push(Item {
                    field: String::new(),
                    value: title,
                    date,
                    kind,
                    url,
                    quote,
                })
            }
            _ => s.dropped += 1,
        }
    }
    for v in list("people") {
        let (name, role) = (str_of(&v, "name"), str_of(&v, "role"));
        match page(&v) {
            Some((url, quote, text)) if !role.is_empty() && quote_in(text, &name) => s.people.push(Item {
                field: name,
                value: role,
                date: String::new(),
                kind: String::new(),
                url,
                quote,
            }),
            _ => s.dropped += 1,
        }
    }
    let name = str_of(answer, "name");
    s.name = pages.iter().any(|(_, t)| quote_in(t, &name)).then_some(name);
    s
}

/// A stated date in the form the timeline keeps: `YYYY`, `YYYY-MM` or `YYYY-MM-DD`.
fn norm_date(s: &str) -> Option<String> {
    let d = parse_date(s)?;
    Some(
        match s.split('-').count() {
            1 => d.format("%Y"),
            2 => d.format("%Y-%m"),
            _ => d.format("%Y-%m-%d"),
        }
        .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sift_keeps_only_what_the_page_says() {
        let pages = vec![
            (
                "https://ex.nl/".to_string(),
                "Example Health B.V. KvK 12345678. We have 120 employees.".to_string(),
            ),
            (
                "https://ex.nl/team".to_string(),
                "Anna de Vries, CEO. In March 2025 we raised EUR 5M.".to_string(),
            ),
        ];
        let answer = json!({
            "name": "Example Health",
            "facts": [
                {"field": "kvk_number", "value": "12345678", "page": 1, "quote": "KvK 12345678"},
                {"field": "headcount", "value": "120 employees", "page": 1, "quote": "We have 120   employees."},
                {"field": "customer", "value": "Philips", "page": 1, "quote": "Philips is our customer"},
                {"field": "kvk_number", "value": "1234", "page": 1, "quote": "KvK 12345678"},
                {"field": "mission", "value": "Care", "page": 9, "quote": "KvK"}
            ],
            "events": [
                {"date": "2025-3", "kind": "funding", "title": "Raised EUR 5M", "page": 2, "quote": "we raised EUR 5M"},
                {"date": "soon", "kind": "funding", "title": "More", "page": 2, "quote": "we raised EUR 5M"}
            ],
            "people": [
                {"name": "Anna de Vries", "role": "CEO", "page": 2, "quote": "Anna de Vries, CEO"},
                {"name": "Jan Smit", "role": "COO", "page": 2, "quote": "Anna de Vries, CEO"}
            ]
        });
        let s = sift(&answer, &pages);
        let kept: Vec<&str> = s.facts.iter().map(|f| f.field.as_str()).collect();
        assert_eq!(kept, ["kvk_number", "headcount"]);
        assert_eq!(s.facts[1].url, "https://ex.nl/");
        assert_eq!(s.events.len(), 1);
        assert_eq!(
            (s.events[0].date.as_str(), s.events[0].url.as_str()),
            ("2025-03", "https://ex.nl/team")
        );
        assert_eq!(s.people.len(), 1);
        assert_eq!(s.dropped, 5);
        assert_eq!(s.name.as_deref(), Some("Example Health"));
        assert_eq!(norm_date("2024").as_deref(), Some("2024"));
        assert!(norm_date("2024-13").is_none());
    }
}
