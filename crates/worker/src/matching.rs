//! Phase 3 jobs: `derive_config {user_id}` → `rescore_all {user_id, digest}` → the digest,
//! and the weekly `weekly_digest` (Monday 07:00 Amsterdam). One Haiku call per company for the
//! skills and mission axes, cached per (profile version, company text); one OSRM table request
//! for road travel times, cached per home base.

use std::collections::HashMap;

use cc_core::company::{parse_date, strength};
use cc_core::matching::{
    derive_config, fit, hard_filter, next_digest_at, penalty, pick_digest, pref_score, role_score, why_now_score, Axis,
    Candidate, Config, Thumb, AXES,
};
use cc_core::profile::Profile;
use cc_core::{may_spend, Lane};
use cc_enrich::Url;
use cc_llm::{LlmError, Model, Msg, Role, ToolRequest};
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::Worker;

pub const JOBS: [&str; 3] = ["derive_config", "rescore_all", "weekly_digest"];
const PROMPT: &str = include_str!("fit_axes.md");
const PURPOSE: &str = "matching";

/// What one job ended as: `Some(retry_at)` with a reason to try again, refund for the cap.
type End = (Option<String>, Option<chrono::DateTime<Utc>>, bool);

impl Worker {
    pub(crate) async fn handle_match(&self, job: &cc_db::Job) -> Result<(), sqlx::Error> {
        let (err, retry, refund) = self.match_job(job).await?;
        cc_db::end_job(&self.pool, job.id, err.as_deref(), retry, refund).await
    }

    async fn match_job(&self, job: &cc_db::Job) -> Result<End, sqlx::Error> {
        if job.kind == "weekly_digest" {
            let users: Vec<Uuid> =
                sqlx::query_scalar("SELECT DISTINCT user_id FROM profiles WHERE status = 'approved'")
                    .fetch_all(&self.pool)
                    .await?;
            for u in users {
                cc_db::enqueue(
                    &self.pool,
                    "rescore_all",
                    json!({"user_id": u, "digest": true}),
                    Utc::now(),
                )
                .await?;
            }
            cc_db::enqueue(&self.pool, "weekly_digest", json!({}), next_digest_at(Utc::now())).await?;
            return Ok((None, None, false));
        }
        let Some(user) = job.payload["user_id"].as_str().and_then(|s| s.parse::<Uuid>().ok()) else {
            return Ok((Some("no user_id".into()), None, false));
        };
        // The test track: a real account's profile never reaches the model while it is on.
        let is_test = cc_db::user_by_id(&self.pool, user).await?.is_some_and(|u| u.is_test);
        if self.test_only && !is_test {
            return Ok((Some("test track: real account skipped".into()), None, false));
        }
        let Some((version, profile)) = approved_profile(&self.pool, user).await? else {
            return Ok((Some("no approved profile".into()), None, false));
        };
        if job.kind == "derive_config" {
            let rows = sqlx::query("SELECT up, axis, reason FROM feedback WHERE user_id = $1 ORDER BY created_at")
                .bind(user)
                .fetch_all(&self.pool)
                .await?;
            let thumbs: Vec<Thumb> = rows
                .iter()
                .map(|r| Thumb {
                    up: r.get("up"),
                    axis: r.get("axis"),
                    reason: r.get("reason"),
                })
                .collect();
            let cfg = derive_config(&profile, &thumbs);
            sqlx::query(
                "INSERT INTO match_configs (user_id, profile_version, config) VALUES ($1, $2, $3) \
                 ON CONFLICT (user_id) DO UPDATE SET profile_version = $2, config = $3, updated_at = now()",
            )
            .bind(user)
            .bind(version)
            .bind(json!(cfg))
            .execute(&self.pool)
            .await?;
            cc_db::enqueue(&self.pool, "rescore_all", json!({"user_id": user}), Utc::now()).await?;
            return Ok((None, None, false));
        }
        self.rescore(
            user,
            version,
            &profile,
            job.payload["digest"].as_bool().unwrap_or(false),
        )
        .await
    }

    async fn rescore(&self, user: Uuid, version: i32, profile: &Profile, digest: bool) -> Result<End, sqlx::Error> {
        let cfg: Option<Value> = sqlx::query_scalar("SELECT config FROM match_configs WHERE user_id = $1")
            .bind(user)
            .fetch_optional(&self.pool)
            .await?;
        let Some(cfg) = cfg.and_then(|c| serde_json::from_value::<Config>(c).ok()) else {
            cc_db::enqueue(&self.pool, "derive_config", json!({"user_id": user}), Utc::now()).await?;
            return Ok((Some("no config yet: derive_config queued".into()), None, false));
        };
        let travel = self.travel_times().await?;
        let dismissed: Vec<Uuid> =
            sqlx::query_scalar("SELECT DISTINCT company_id FROM feedback WHERE user_id = $1 AND NOT up")
                .bind(user)
                .fetch_all(&self.pool)
                .await?;
        let today = Utc::now().date_naive();
        let her = her_summary(profile);
        let companies =
            sqlx::query("SELECT id, name, website, subsector, stage, mission_summary FROM companies ORDER BY name")
                .fetch_all(&self.pool)
                .await?;
        for c in &companies {
            let id: Uuid = c.get("id");
            let facts =
                sqlx::query("SELECT field, value FROM company_facts WHERE company_id = $1 ORDER BY field, value")
                    .bind(id)
                    .fetch_all(&self.pool)
                    .await?;
            let mut text = format!(
                "Company: {}\nSite: {}\nMission: {}\n",
                c.get::<String, _>("name"),
                c.get::<String, _>("website"),
                c.get::<Option<String>, _>("mission_summary").unwrap_or_default()
            );
            for f in &facts {
                text.push_str(&format!(
                    "- {}: {}\n",
                    f.get::<String, _>("field"),
                    f.get::<String, _>("value")
                ));
            }
            let network = facts.iter().any(|f| {
                f.get::<String, _>("field") == "philips_link"
                    && !f.get::<String, _>("value").trim().eq_ignore_ascii_case("no")
            });
            let hash = format!("{:x}", Sha256::digest(text.as_bytes()));

            // Hard filters first: travel and her own "not for me" need no model call.
            let minutes = travel.get(&id).copied().flatten();
            let pre = hard_filter(&cfg, minutes, "", "", dismissed.contains(&id));
            // The one model judgement, cached.
            let cached: Option<Value> = sqlx::query_scalar(
                "SELECT answer FROM axis_cache WHERE user_id = $1 AND profile_version = $2 AND company_id = $3 AND company_hash = $4",
            )
            .bind(user)
            .bind(version)
            .bind(id)
            .bind(&hash)
            .fetch_optional(&self.pool)
            .await?;
            let model = match cached {
                Some(a) => Some(a),
                None if pre.is_some() => None,
                None => match self.ask(&her, &text).await? {
                    Ok(a) => {
                        sqlx::query(
                            "INSERT INTO axis_cache (user_id, profile_version, company_id, company_hash, answer) \
                             VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
                        )
                        .bind(user)
                        .bind(version)
                        .bind(id)
                        .bind(&hash)
                        .bind(&a)
                        .execute(&self.pool)
                        .await?;
                        Some(a)
                    }
                    Err(None) => {
                        return Ok((
                            Some("monthly cap reached".into()),
                            Some(Utc::now() + Duration::hours(6)),
                            true,
                        ))
                    }
                    Err(Some(e)) => {
                        tracing::info!("fit axes for {id}: {e}");
                        None
                    }
                },
            };
            let judged = |k: &str| -> (f64, String) {
                match &model {
                    Some(a) => (
                        a[k].as_f64().unwrap_or(0.0).clamp(0.0, 1.0),
                        a[format!("{k}_reason")].as_str().unwrap_or("").to_string(),
                    ),
                    None if pre.is_some() => (0.0, "Not judged: filtered out first".into()),
                    None => (0.0, "UNKNOWN: the model could not judge it this time".into()),
                }
            };

            let signals = sqlx::query("SELECT kind, dated, detail FROM signals WHERE company_id = $1")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;
            let decayed: Vec<(String, f64, String)> = signals
                .iter()
                .filter_map(|s| {
                    let kind: String = s.get("kind");
                    let d = parse_date(s.get("dated"))?;
                    Some((kind.clone(), strength(&kind, d, today), s.get("detail")))
                })
                .collect();
            let ops = decayed
                .iter()
                .filter(|s| s.0 == "ops_roles")
                .map(|s| s.1)
                .fold(0.0, f64::max);
            let careers: String = sqlx::query_scalar(
                "SELECT COALESCE(string_agg(text, ' '), '') FROM page_snapshots WHERE company_id = $1 AND (url ILIKE '%career%' OR url ILIKE '%job%' OR url ILIKE '%vacature%')",
            )
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
            let scores = [
                judged("skills"),
                why_now_score(&decayed.iter().map(|s| (s.1, s.2.clone())).collect::<Vec<_>>()),
                judged("mission"),
                pref_score(
                    &cfg.subsectors,
                    c.get::<Option<String>, _>("subsector").as_deref(),
                    "sub-sector",
                ),
                pref_score(&cfg.stages, c.get::<Option<String>, _>("stage").as_deref(), "stage"),
                role_score(&cfg.role_types, ops, &careers),
                if network {
                    (
                        1.0,
                        "Ex-Philips or IBM people in its leadership (source on the facts)".to_string(),
                    )
                } else {
                    (0.0, "No known ex-Philips or IBM link".to_string())
                },
            ];
            let axes: Vec<Axis> = AXES
                .iter()
                .zip(scores)
                .zip(cfg.weights)
                .map(|((a, (score, reason)), weight)| Axis {
                    axis: a.to_string(),
                    score,
                    weight,
                    reason,
                })
                .collect();
            let ans = model.clone().unwrap_or(Value::Null);
            let excluded = pre.or_else(|| {
                hard_filter(
                    &cfg,
                    minutes,
                    ans["anti_pref_hit"].as_str().unwrap_or(""),
                    ans["working_language"].as_str().unwrap_or(""),
                    false,
                )
            });
            let (points, hits) = penalty(&cfg.penalties, &text);
            let score = excluded.is_none().then(|| fit(&axes, points));
            let travel_note = match (self.home.is_some(), minutes) {
                (_, Some(m)) => format!("{m} min by car"),
                (true, None) => "Travel time UNKNOWN".to_string(),
                (false, None) => "Travel time UNKNOWN (no home base set on the server)".to_string(),
            };
            sqlx::query(
                "INSERT INTO fit_scores (user_id, company_id, profile_version, score, excluded, axes, travel_min) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (user_id, company_id) DO UPDATE SET \
                 profile_version = $3, score = $4, excluded = $5, axes = $6, travel_min = $7, updated_at = now()",
            )
            .bind(user)
            .bind(id)
            .bind(version)
            .bind(score)
            .bind(&excluded)
            .bind(json!({"axes": axes, "penalty": points, "penalty_reasons": hits, "travel": travel_note}))
            .bind(minutes.map(|m| m as i32))
            .execute(&self.pool)
            .await?;
        }
        let has_digest: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM digests WHERE user_id = $1)")
            .bind(user)
            .fetch_one(&self.pool)
            .await?;
        if digest || !has_digest {
            self.build_digest(user).await?;
        }
        Ok((None, None, false))
    }

    /// Spec §6: five companies, each with its strongest fresh signal and its source.
    async fn build_digest(&self, user: Uuid) -> Result<(), sqlx::Error> {
        let rows = sqlx::query(
            "SELECT f.company_id, f.score, f.axes, c.name, c.subsector FROM fit_scores f JOIN companies c ON c.id = f.company_id \
             WHERE f.user_id = $1 AND NOT EXISTS (SELECT 1 FROM pipeline_items p \
             WHERE p.user_id = f.user_id AND p.company_id = f.company_id AND p.last_sent_at IS NOT NULL)",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await?;
        let today = Utc::now().date_naive();
        let mut cands = Vec::new();
        let mut best: HashMap<Uuid, Value> = HashMap::new();
        for r in &rows {
            let id: Uuid = r.get("company_id");
            let sig = sqlx::query("SELECT kind, dated, detail, source_url FROM signals WHERE company_id = $1")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;
            let strongest = sig
                .iter()
                .filter_map(|s| {
                    let d = parse_date(s.get("dated"))?;
                    Some((strength(s.get("kind"), d, today), d, s))
                })
                .max_by(|a, b| a.0.total_cmp(&b.0));
            let newest = sig.iter().filter_map(|s| parse_date(s.get("dated"))).max();
            let score: Option<f64> = r.get("score");
            cands.push(Candidate {
                id,
                score: score.unwrap_or(0.0),
                subsector: r.get("subsector"),
                newest_signal: newest,
                out: score.is_none(),
            });
            if let Some((_, _, s)) = strongest {
                let axes: Value = r.get("axes");
                let reason = |k: &str| {
                    axes["axes"]
                        .as_array()
                        .and_then(|a| a.iter().find(|x| x["axis"] == k))
                        .map_or(Value::Null, |x| x["reason"].clone())
                };
                let detail: String = s.get("detail");
                best.insert(
                    id,
                    json!({
                        "company_id": id,
                        "name": r.get::<String, _>("name"),
                        "score": score.map(|x| x.round()),
                        "why_her": reason("skills"),
                        "why_them": reason("mission"),
                        "why_now": detail,
                        "signal": {"kind": s.get::<String, _>("kind"), "dated": s.get::<String, _>("dated"),
                                   "detail": detail, "source_url": s.get::<String, _>("source_url")},
                    }),
                );
            }
        }
        let items: Vec<Value> = pick_digest(&cands, today)
            .into_iter()
            .filter_map(|id| best.remove(&id))
            .collect();
        sqlx::query("INSERT INTO digests (id, user_id, items) VALUES ($1, $2, $3)")
            .bind(Uuid::new_v4())
            .bind(user)
            .bind(json!(items))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// One Haiku call: skills and mission, each 0–1 with one sentence. `Err(None)` is the cap.
    async fn ask(&self, her: &str, company: &str) -> Result<Result<Value, Option<String>>, sqlx::Error> {
        let req = ToolRequest {
            model: Model::Haiku,
            system_static: PROMPT.into(),
            system_dynamic: her.into(),
            messages: vec![Msg {
                role: Role::User,
                text: company.into(),
            }],
            tool_name: "fit_axes".into(),
            tool_description: "How well this company fits her skills and her mission, each 0 to 1 with one sentence."
                .into(),
            tool_schema: json!({
                "type": "object",
                "properties": {
                    "skills": {"type": "number"},
                    "skills_reason": {"type": "string"},
                    "mission": {"type": "number"},
                    "mission_reason": {"type": "string"},
                    "anti_pref_hit": {"type": "string"},
                    "working_language": {"type": "string"},
                },
                "required": ["skills", "skills_reason", "mission", "mission_reason", "anti_pref_hit", "working_language"],
            }),
            max_tokens: 400,
        };
        let spent = cc_db::month_spend(&self.pool).await?;
        let cap = cc_db::cap_eur(&self.pool).await?;
        if !may_spend(spent, req.estimate_usd() * self.eur_per_usd, cap, Lane::Background) {
            return Ok(Err(None));
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
        Ok(res.map(|(v, _)| v).map_err(|e| Some(e.to_string())))
    }

    /// Road minutes from the home base per company (None = UNKNOWN). Companies without a
    /// cached time get ONE OSRM table request together; a failed request leaves them UNKNOWN
    /// and is tried again on a later rescore (a no-route answer after a day).
    async fn travel_times(&self) -> Result<HashMap<Uuid, Option<u32>>, sqlx::Error> {
        let Some((lat, lon)) = self.home else {
            return Ok(HashMap::new());
        };
        let key = format!("{:x}", Sha256::digest(format!("{lat:.5},{lon:.5}").as_bytes()));
        let todo = sqlx::query(
            "SELECT c.id, c.lat, c.lon FROM companies c LEFT JOIN travel_times t ON t.company_id = c.id AND t.home_key = $1 \
             WHERE c.lat IS NOT NULL AND c.lon IS NOT NULL AND (t.company_id IS NULL OR t.minutes IS NULL AND t.fetched_at < now() - interval '1 day')",
        )
        .bind(&key)
        .fetch_all(&self.pool)
        .await?;
        if !todo.is_empty() {
            let mut coords = vec![format!("{lon:.5},{lat:.5}")];
            coords.extend(
                todo.iter()
                    .map(|r| format!("{:.5},{:.5}", r.get::<f64, _>("lon"), r.get::<f64, _>("lat"))),
            );
            let base = format!("{}/table/v1/driving", self.osrm_url.trim_end_matches('/'));
            let url = Url::parse(&format!("{base}/{}?sources=0&annotations=duration", coords.join(";")));
            let body = match url {
                Ok(u) => self
                    .fetch
                    .api(&u, std::time::Duration::from_secs(1))
                    .await
                    .map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            let table: Option<Vec<Value>> = body
                .ok()
                .and_then(|b| serde_json::from_str::<Value>(&b).ok())
                .filter(|v| v["code"] == "Ok")
                .and_then(|v| v["durations"][0].as_array().cloned());
            match table {
                Some(row) if row.len() == coords.len() => {
                    for (r, d) in todo.iter().zip(&row[1..]) {
                        let minutes = d.as_f64().map(|s| (s / 60.0).round() as i32);
                        sqlx::query(
                            "INSERT INTO travel_times (company_id, home_key, minutes, source_url) VALUES ($1, $2, $3, $4) \
                             ON CONFLICT (company_id, home_key) DO UPDATE SET minutes = $3, source_url = $4, fetched_at = now()",
                        )
                        .bind(r.get::<Uuid, _>("id"))
                        .bind(&key)
                        .bind(minutes)
                        .bind(&base)
                        .execute(&self.pool)
                        .await?;
                    }
                }
                _ => tracing::warn!("OSRM table request failed: travel times stay UNKNOWN"),
            }
        }
        let rows = sqlx::query("SELECT company_id, minutes FROM travel_times WHERE home_key = $1")
            .bind(&key)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get("company_id"),
                    r.get::<Option<i32>, _>("minutes").map(|m| m.max(0) as u32),
                )
            })
            .collect())
    }
}

async fn approved_profile(pool: &sqlx::PgPool, user: Uuid) -> Result<Option<(i32, Profile)>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT version, data FROM profiles WHERE user_id = $1 AND status = 'approved' ORDER BY version DESC LIMIT 1",
    )
    .bind(user)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|r| {
        let p = serde_json::from_value::<Profile>(r.get("data")).ok()?;
        Some((r.get("version"), p))
    }))
}

/// What the model needs of her: strengths, the stories behind them, mission, rather-nots,
/// languages. No contact details, no home base.
fn her_summary(p: &Profile) -> String {
    let mut s = format!("Her pitch: {}\nStrengths (rank 1-5):\n", p.pitch.text);
    for x in &p.strengths {
        s.push_str(&format!("- {} ({})\n", x.skill, x.rank));
    }
    s.push_str("Stories:\n");
    for e in &p.evidence {
        s.push_str(&format!(
            "- {}: {} {} Skills: {}\n",
            e.title,
            e.action,
            e.result,
            e.skills.join(", ")
        ));
    }
    let list = |w: &[cc_core::profile::Weighted]| w.iter().map(|x| x.value.clone()).collect::<Vec<_>>().join(", ");
    s.push_str(&format!("Mission themes: {}\n", list(&p.mission_themes)));
    s.push_str(&format!("Role types: {}\n", list(&p.role_types)));
    let anti: Vec<&str> = p.anti_prefs.iter().map(|t| t.text.as_str()).collect();
    s.push_str(&format!(
        "Rather not (copy one exactly into anti_pref_hit if the company clearly hits it): {}\n",
        anti.join(" | ")
    ));
    s
}
