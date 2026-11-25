//! The intake ("Talk to Kompas") end to end against real Postgres and a mock model server.
//! All data is made up; no real model is called.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::{routing::post, Json, Router};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

const ADMIN: &str = "test-admin-token-0123456789abcdef0123";

/// Her answers, one per phase. Full sentences, so the voice sample can quote them.
const ANSWERS: [&str; 8] = [
    "I am Anna and I live in Utrecht.",
    "I worked for ten years as a programme manager at a hospital group.",
    "I led a team of twelve people through a merger without anyone leaving.",
    "I get energy from connecting people who did not understand each other at first.",
    "I am looking for a role in care technology where I build something.",
    "At most three quarters of an hour of travel and 32 hours a week.",
    "I spent a year caring for my father.",
    "That is right.",
];

type Seen = Arc<Mutex<Vec<Value>>>;

/// A stand-in for the model API: answers each tool by name and records every request.
async fn mock_model() -> (String, Seen) {
    let seen: Seen = Arc::default();
    let s = seen.clone();
    let app = Router::new().route(
        "/v1/messages",
        post(move |Json(b): Json<Value>| {
            let s = s.clone();
            async move {
                s.lock().unwrap().push(b.clone());
                // Which answer is wanted follows from the requested schema.
                let props = &b["output_config"]["format"]["schema"]["properties"];
                let tool = if props.get("phase_complete").is_some() {
                    "kompas_turn"
                } else if props.get("timeline").is_some() {
                    "save_stories"
                } else if props.get("home_base").is_some() {
                    "save_prefs"
                } else if props.get("voice_sample").is_some() {
                    "write_pitch"
                } else if props.get("updated").is_some() {
                    "revise_section"
                } else if props.get("facts").is_some() {
                    "save_document_facts"
                } else {
                    ""
                };
                let input = match tool {
                    "kompas_turn" => json!({"message": "Tell me, what happened then?", "phase_complete": true}),
                    "save_stories" => json!({
                        "timeline": [{"role": "programme manager", "org": "hospital group", "source_ids": ["T4"]}],
                        "evidence": [{"id": "E1", "title": "Merger", "situation": "merger", "task": "keep the team",
                            "action": "connecting", "result": "nobody left", "skills": ["leadership"], "source_ids": ["T6"]},
                            {"id": "E2", "title": "Made up", "situation": "x", "task": "x", "action": "x",
                            "result": "x", "skills": [], "source_ids": ["T99"]}],
                        "strengths": [{"skill": "connecting people", "rank": 5, "evidence_ids": ["E1"], "source_ids": ["T8"]}],
                        "languages": [], "anti_prefs": [], "work_style": []
                    }),
                    // As the strict schema returns it: every field present, unmentioned ones empty or null.
                    "save_prefs" => json!({
                        "role_types": [], "stages": [], "mission_themes": [],
                        "max_travel": null, "hours_per_week": null, "work_mode": null, "start_date": null,
                        "subsectors": [{"value": "care_tech", "weight": 0.9, "source_ids": ["T10"]}],
                        "home_base": {"city": "Utrecht", "source_ids": ["T2"]},
                        "gap_framing": {"text": "A year caring for my father.", "source_ids": ["T14"]}
                    }),
                    "write_pitch" => json!({
                        "pitch": {"text": "I connect people and build care that works.", "source_ids": ["T6", "T8"]},
                        "voice_sample": [
                            {"text": ANSWERS[2], "source_ids": ["T6"]},
                            {"text": ANSWERS[3], "source_ids": ["T8"]},
                            {"text": ANSWERS[4], "source_ids": ["T10"]},
                            {"text": "I am the best manager in the Netherlands.", "source_ids": ["T6"]}
                        ]
                    }),
                    "revise_section" => json!({"updated": {
                        "gap_framing": {"text": "A year of family caregiving, chosen deliberately.", "source_ids": ["T18"]}
                    }}),
                    // The second quote is not in the document: the snippet guard drops it.
                    "save_document_facts" => json!({"facts": [
                        {"kind": "role", "text": "Programme manager from 2014 to 2024.", "snippet": "Programme manager, 2014-2024."},
                        {"kind": "achievement", "text": "Won a national award.", "snippet": "National award winner 2019"}
                    ]}),
                    _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": {"type": "unknown_tool"}}))),
                };
                (
                    StatusCode::OK,
                    Json(json!({
                        "stop_reason": "end_turn",
                        "content": [{"type": "text", "text": input.to_string()}],
                        "usage": {"input_tokens": 1000, "output_tokens": 200,
                                  "cache_creation_input_tokens": 3000, "cache_read_input_tokens": 0}
                    })),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn app(pool: PgPool, llm: Option<String>) -> Router {
    cc_api::app(cc_api::AppState {
        pool,
        jwt_secret: b"test-jwt-secret-0123456789abcdef0123".to_vec(),
        admin_token: ADMIN.into(),
        llm: llm.map(|u| cc_llm::Client::new(&u, "test-key")),
        eur_per_usd: 1.0,
        web_dir: None,
        test_only: false,
        stt_url: None,
        stt_model: "test".into(),
    })
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    h: &[(String, String)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(uri);
    for (k, v) in h {
        b = b.header(k.as_str(), v.as_str());
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
    (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

/// Pairs a fresh device through the gate, sets the password, logs in: the auth headers.
async fn login(app: &Router, email: &str) -> Vec<(String, String)> {
    let mut b = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
    let k = SigningKey::from_bytes(&b);
    let pk = STANDARD.encode(k.verifying_key().to_bytes());
    let (_, v) = call(
        app,
        "POST",
        "/pair/request",
        &[],
        Some(json!({"device_name": "Test phone", "public_key": pk})),
    )
    .await;
    let id = v["id"].as_str().unwrap().to_string();
    let admin = vec![("authorization".to_string(), format!("Bearer {ADMIN}"))];
    let (c, _) = call(
        app,
        "POST",
        &format!("/stark/pairing/{id}/decision"),
        &admin,
        Some(json!({"approve": true})),
    )
    .await;
    assert_eq!(c, StatusCode::OK);
    let sig = STANDARD.encode(k.sign(format!("cc-pair:{id}").as_bytes()).to_bytes());
    let (_, v) = call(
        app,
        "POST",
        &format!("/pair/{id}/claim"),
        &[],
        Some(json!({"signature": sig})),
    )
    .await;
    let dt = vec![(
        "x-device-token".to_string(),
        v["device_token"].as_str().unwrap().to_string(),
    )];
    let creds = json!({"email": email, "password": "correct horse battery"});
    call(app, "POST", "/auth/set-password", &dt, Some(creds.clone())).await;
    let (c, v) = call(app, "POST", "/auth/login", &dt, Some(creds)).await;
    assert_eq!(c, StatusCode::OK, "{v}");
    let mut h = dt;
    h.push((
        "authorization".into(),
        format!("Bearer {}", v["session"].as_str().unwrap()),
    ));
    h
}

async fn intake_calls(pool: &PgPool) -> (i64, f64, i64) {
    sqlx::query_as::<_, (i64, f64, i64)>(
        "SELECT COUNT(*), COALESCE(SUM(cost_eur), 0)::float8, COALESCE(SUM(cache_write), 0)::bigint \
         FROM llm_calls WHERE purpose = 'intake'",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../db/migrations")]
async fn full_intake_resume_playback_approve(pool: PgPool) {
    let tester = cc_db::create_user(&pool, "tester@example.org", cc_core::Role::Seeker)
        .await
        .unwrap();
    cc_db::mark_test_user(&pool, tester).await.unwrap();
    let (url, seen) = mock_model().await;
    let app = app(pool.clone(), Some(url.clone()));
    let h = login(&app, "tester@example.org").await;

    let (c, v) = call(&app, "GET", "/intake", &h, None).await;
    assert_eq!(c, StatusCode::OK);
    assert_eq!(v["status"], "none");
    assert_eq!(v["is_test"], true);

    // Start: Kompas opens. Starting again resumes, it never opens a second session.
    let (c, v) = call(&app, "POST", "/intake/start", &h, None).await;
    assert_eq!(c, StatusCode::OK, "{v}");
    assert_eq!(v["turns"].as_array().unwrap().len(), 1);
    let (_, v) = call(&app, "POST", "/intake/start", &h, None).await;
    assert_eq!(v["turns"].as_array().unwrap().len(), 1, "start resumes");

    // Too early for playback.
    assert_eq!(
        call(&app, "POST", "/intake/playback", &h, None).await.0,
        StatusCode::CONFLICT
    );

    // Seven answers walk phases 1..7; each phase is marked covered.
    for (i, a) in ANSWERS.iter().take(7).enumerate() {
        let (c, v) = call(&app, "POST", "/intake/message", &h, Some(json!({"text": a}))).await;
        assert_eq!(c, StatusCode::OK, "{v}");
        assert_eq!(v["covered"], i + 1);
    }
    // Resume from a fresh app (a new phone, a restart): the conversation is all there.
    let app2 = self::app(pool.clone(), Some(url));
    let (_, v) = call(&app2, "GET", "/intake", &h, None).await;
    assert_eq!(v["phase"], 8);
    assert_eq!(v["phase_name"], "Looking back");
    assert_eq!(v["turns"].as_array().unwrap().len(), 15);
    assert_eq!(v["turns"][1]["text"], ANSWERS[0]);
    let (c, _) = call(&app2, "POST", "/intake/message", &h, Some(json!({"text": ANSWERS[7]}))).await;
    assert_eq!(c, StatusCode::OK);

    // Playback step 1: extraction drops the item with an invented source.
    let (c, v) = call(&app2, "POST", "/intake/playback", &h, None).await;
    assert_eq!(c, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "playback");
    let d = &v["profile"]["data"];
    assert_eq!(d["evidence"].as_array().unwrap().len(), 1, "T99 does not exist");
    assert_eq!(d["home_base"]["city"], "Utrecht");

    // Step 2: pitch and voice; a sentence she never said is dropped.
    let (c, v) = call(&app2, "POST", "/intake/pitch", &h, None).await;
    assert_eq!(c, StatusCode::OK, "{v}");
    let d = &v["profile"]["data"];
    assert!(cc_core::profile::word_count(d["pitch"]["text"].as_str().unwrap()) <= 150);
    assert_eq!(d["voice_sample"].as_array().unwrap().len(), 3);
    assert_eq!(d["voice_sample"][0]["text"], ANSWERS[2]);

    // Her correction is stored as her words, and the section follows it.
    let (c, v) = call(
        &app2,
        "POST",
        "/intake/correct",
        &h,
        Some(json!({"section": "gap", "text": "Call it family caregiving, that was a deliberate choice."})),
    )
    .await;
    assert_eq!(c, StatusCode::OK, "{v}");
    assert_eq!(
        v["profile"]["data"]["gap_framing"]["text"],
        "A year of family caregiving, chosen deliberately."
    );
    assert_eq!(v["profile"]["data"]["gap_framing"]["source_ids"][0], "T18");
    assert_eq!(
        call(
            &app2,
            "POST",
            "/intake/correct",
            &h,
            Some(json!({"section": "salary", "text": "x"}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );

    let (c, v) = call(&app2, "POST", "/intake/approve", &h, None).await;
    assert_eq!(c, StatusCode::OK, "{v}");
    assert_eq!(v["profile"]["status"], "approved");
    assert_eq!(v["status"], "approved");

    // Metered: every model call is logged as intake, and prompt caching is asked for.
    let (n, eur, cache_write) = intake_calls(&pool).await;
    assert_eq!(n, 1 + 8 + 2 + 1 + 1);
    assert!(eur > 0.0 && cache_write > 0);
    let reqs = seen.lock().unwrap().clone();

    assert!(reqs
        .iter()
        .all(|r| r["system"][0]["cache_control"]["type"] == "ephemeral"));
    assert!(reqs.iter().any(|r| r["model"] == "claude-haiku-5-5"
        && r["output_config"]["format"]["schema"]["properties"]["timeline"].is_object()));
    assert!(reqs
        .iter()
        .filter(|r| r["output_config"]["format"]["schema"]["properties"]["phase_complete"].is_object())
        .all(|r| r["model"] == "claude-sonnet-5-5"));

    // A test account may start over.
    let (c, v) = call(&app2, "POST", "/intake/reset", &h, None).await;
    assert_eq!(c, StatusCode::OK);
    assert_eq!(v["status"], "none");
    assert!(v["profile"].is_null());
}

#[sqlx::test(migrations = "../db/migrations")]
async fn real_account_guards(pool: PgPool) {
    cc_db::create_user(&pool, "seeker@example.org", cc_core::Role::Seeker)
        .await
        .unwrap();
    let tester = cc_db::create_user(&pool, "tester@example.org", cc_core::Role::Seeker)
        .await
        .unwrap();
    cc_db::mark_test_user(&pool, tester).await.unwrap();

    // Offline: no model key, her answer is still kept.
    let offline = app(pool.clone(), None);
    let her = login(&offline, "seeker@example.org").await;
    let (c, v) = call(&offline, "POST", "/intake/start", &her, None).await;
    assert_eq!(c, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(v["error"], "kompas_offline");
    let (_, v) = call(&offline, "GET", "/intake", &her, None).await;
    assert_eq!(v["online"], false);

    // A real account can never reset, and nothing works without login.
    assert_eq!(
        call(&offline, "POST", "/intake/reset", &her, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&offline, "GET", "/intake", &[], None).await.0,
        StatusCode::UNAUTHORIZED
    );

    // Cap 0: the model is never called.
    let (url, seen) = mock_model().await;
    let online = app(pool.clone(), Some(url));
    cc_db::set_cap_eur(&pool, 0.0).await.unwrap();
    let (c, v) = call(&online, "POST", "/intake/start", &her, None).await;
    assert_eq!(c, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(v["error"], "cap_reached");
    assert!(seen.lock().unwrap().is_empty());
    cc_db::set_cap_eur(&pool, 30.0).await.unwrap();

    // Isolation: the tester's conversation is invisible to her, and her documents to the tester.
    let t = login(&online, "tester@example.org").await;
    call(&online, "POST", "/intake/start", &t, None).await;
    call(
        &online,
        "POST",
        "/intake/message",
        &t,
        Some(json!({"text": ANSWERS[0]})),
    )
    .await;
    let (_, v) = call(&online, "GET", "/intake", &her, None).await;
    assert!(v["turns"].as_array().unwrap().iter().all(|x| x["text"] != ANSWERS[0]));

    // Documents: the model lists the facts, the guard drops a fact whose quote is not in the
    // text, and nothing counts until she confirms it. Her word decides, and only hers.
    let (c, d) = call(
        &online,
        "POST",
        "/intake/documents",
        &her,
        Some(json!({"name": "cv.txt", "body": "Programme manager, 2014-2024."})),
    )
    .await;
    assert_eq!(c, StatusCode::OK, "{d}");
    let (_, list) = call(&online, "GET", "/intake/documents", &t, None).await;
    assert!(list.as_array().unwrap().is_empty(), "not visible to another account");
    let (_, list) = call(&online, "GET", "/intake/documents", &her, None).await;
    assert_eq!(list[0]["dropped"], 1, "{list}");
    let facts = list[0]["facts"].as_array().unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0]["status"], "pending");
    assert!(list[0].get("body").is_none());
    let id = facts[0]["id"].as_str().unwrap();
    let (_, v) = call(&online, "GET", "/intake", &her, None).await;
    assert_eq!(v["pending_facts"], 1);
    let (c, _) = call(
        &online,
        "POST",
        &format!("/intake/facts/{id}"),
        &t,
        Some(json!({"confirm": true})),
    )
    .await;
    assert_eq!(c, StatusCode::NOT_FOUND, "the tester cannot confirm her fact");
    let (_, list) = call(
        &online,
        "POST",
        &format!("/intake/facts/{id}"),
        &her,
        Some(json!({"confirm": true, "text": "Programme manager at a hospital group, 2014 to 2024."})),
    )
    .await;
    assert_eq!(list[0]["facts"][0]["status"], "confirmed");
    assert_eq!(
        list[0]["facts"][0]["text"],
        "Programme manager at a hospital group, 2014 to 2024."
    );
    // Kompas now sees what she confirmed, in her own words.
    let (c, v) = call(&online, "POST", "/intake/start", &her, None).await;
    assert_eq!(c, StatusCode::OK, "{v}");
    let last = seen.lock().unwrap().last().unwrap().to_string();
    assert!(last.contains("(facts she confirmed):\\n- Programme manager at a hospital group, 2014 to 2024."));
    let (c, v) = call(
        &online,
        "POST",
        "/intake/documents",
        &her,
        Some(json!({"name": "x.bin", "data": STANDARD.encode([0xffu8, 0xfe, 0x00])})),
    )
    .await;
    assert_eq!(
        (c, v["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("unsupported_file"))
    );
    let docs_calls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM llm_calls WHERE purpose = 'documents'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(docs_calls, 1, "metered under its own purpose");

    // Push-to-talk without a speech-to-text service: a clear 503, nothing else.
    let (c, v) = call(
        &online,
        "POST",
        "/intake/audio",
        &her,
        Some(json!({"mime": "audio/webm", "data": STANDARD.encode(b"not really audio")})),
    )
    .await;
    assert_eq!(
        (c, v["error"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("stt_offline"))
    );
}

/// Made-up seeker for the live run. A cheap model plays her from this sheet, so she answers
/// what Kompas actually asks. None of it is about a real person.
const PERSONA: &str = "You play Anna (52, Utrecht) in a conversation with Kompas, a career conversation in an app. \
Answer as Anna, in English, in one to four natural sentences, only to what is asked. \
Invent no facts beyond this sheet; if Anna does not know something, say so. If Kompas asks you to choose or rank something, do it. \
If Kompas says the conversation is finished or that she can press 'Make my profile', answer: 'Thank you, I am done.'\n\
Facts: nurse 1996-2004 in a regional hospital. Team leader cardiology 2004-2010, team of 20. \
Programme manager at Medivista (maker of patient monitoring) 2010-2018, team of 15, the clients were hospitals. \
Head of implementation at Medivista 2018-2022: introducing new monitoring systems in 12 hospitals. \
From 2022 to 2024 at home caring for her mother; kept up with trade journals and took an online course on the MDR. \
Stories: (1) at one hospital the nursing staff did not want the system; she worked night shifts alongside them for three weeks, had it adjusted, \
and after two months 90 percent used it daily. (2) she cut an implementation that was six months behind into three steps, \
and within four months it was back on schedule. (3) when two implementation teams merged, nobody left in the first year. \
(4) she wrote the training plan that was then used in all 12 hospitals, training time from 2 days to 1. \
Strengths: translating between technology and the work floor, bringing people along, cutting big projects into small steps, staying calm under pressure. \
Energy: connecting people, seeing something work on the ward. Costs energy: meetings without a decision, politics. \
Looking for: an implementation or product role, preferably permanent or part-time, a scale-up or mid-sized company, digital health or care technology; \
patient safety moves her most. At most 45 minutes of travel by train or car, 28 to 32 hours, hybrid, can start in March, \
Dutch and good English. The break sentence she likes: two years of family caregiving, and she kept up with her field.";

/// Anna's next answer to the conversation so far (Kompas speaks last).
async fn dummy_answer(llm: &cc_llm::Client, turns: &Value) -> (String, f64) {
    let messages = turns
        .as_array()
        .unwrap()
        .iter()
        .map(|t| cc_llm::Msg {
            role: if t["speaker"] == "kompas" {
                cc_llm::Role::User
            } else {
                cc_llm::Role::Assistant
            },
            text: t["text"].as_str().unwrap().to_string(),
        })
        .collect();
    let req = cc_llm::ToolRequest {
        model: cc_llm::Model::Haiku,
        system_static: PERSONA.into(),
        system_dynamic: String::new(),
        messages,
        tool_name: "anna".into(),
        tool_description: String::new(),
        tool_schema: json!({"type": "object", "properties": {"answer": {"type": "string"}}, "required": ["answer"]}),
        max_tokens: 1_000,
    };
    let (v, u) = llm.call(&req).await.expect("dummy answer");
    (
        v["answer"].as_str().unwrap().to_string(),
        u.cost_usd(cc_llm::Model::Haiku),
    )
}

/// Live run against the real model (Hugo's verification of Phase 1). Costs real money:
/// `CC_LIVE_ANTHROPIC_KEY=... cargo test -p cc-api --test intake live_dummy -- --ignored`.
/// Writes the transcript, profile and cost to `CC_DUMMY_OUT` (a JSON file) when set.
#[sqlx::test(migrations = "../db/migrations")]
#[ignore = "calls the real model"]
async fn live_dummy(pool: PgPool) {
    let key = std::env::var("CC_LIVE_ANTHROPIC_KEY").expect("CC_LIVE_ANTHROPIC_KEY");
    let _ = tracing_subscriber::fmt().with_env_filter("cc_api=info").try_init();
    let tester = cc_db::create_user(&pool, "dummy@example.org", cc_core::Role::Seeker)
        .await
        .unwrap();
    cc_db::mark_test_user(&pool, tester).await.unwrap();
    let st = cc_api::AppState {
        pool: pool.clone(),
        jwt_secret: b"test-jwt-secret-0123456789abcdef0123".to_vec(),
        admin_token: ADMIN.into(),
        llm: Some(cc_llm::Client::new("https://api.anthropic.com", &key)),
        eur_per_usd: 1.0,
        web_dir: None,
        test_only: false,
        stt_url: None,
        stt_model: "test".into(),
    };
    let app = cc_api::app(st);
    let h = login(&app, "dummy@example.org").await;
    let (c, mut v) = call(&app, "POST", "/intake/start", &h, None).await;
    assert_eq!(c, StatusCode::OK, "{v}");
    let anna = cc_llm::Client::new("https://api.anthropic.com", &key);
    let mut dummy_usd = 0.0;
    for _ in 0..60 {
        if v["phase"] == 8 {
            break;
        }
        let (a, usd) = dummy_answer(&anna, &v["turns"]).await;
        dummy_usd += usd;
        let (mut c, mut nv) = call(&app, "POST", "/intake/message", &h, Some(json!({"text": a}))).await;
        if c == StatusCode::SERVICE_UNAVAILABLE {
            // What her "Try again" does: her answer is saved, ask Kompas again.
            (c, nv) = call(&app, "POST", "/intake/message", &h, Some(json!({"text": ""}))).await;
        }
        assert_eq!(c, StatusCode::OK, "{nv}");
        v = nv;
    }
    assert!(v["phase"] == 8, "reached the last phase: {}", v["phase"]);
    let (c, v) = call(&app, "POST", "/intake/playback", &h, None).await;
    assert_eq!(c, StatusCode::OK, "{v}");
    let (c, v) = call(&app, "POST", "/intake/pitch", &h, None).await;
    assert_eq!(c, StatusCode::OK, "{v}");
    let d = &v["profile"]["data"];
    let words = cc_core::profile::word_count(d["pitch"]["text"].as_str().unwrap_or(""));
    let voice = d["voice_sample"].as_array().map(Vec::len).unwrap_or(0);
    let (n, eur, cache_write) = intake_calls(&pool).await;
    let cache_read: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(cache_read), 0)::bigint FROM llm_calls")
        .fetch_one(&pool)
        .await
        .unwrap();
    if let Ok(out) = std::env::var("CC_DUMMY_OUT") {
        let report = json!({
            "turns": v["turns"], "covered": v["covered"], "profile": d,
            "pitch_words": words, "voice_sentences": voice,
            "calls": n, "cost_eur_at_1_to_1": eur, "dummy_player_usd": dummy_usd, "cache_write_tokens": cache_write, "cache_read_tokens": cache_read,
        });
        std::fs::write(out, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    }
    assert!(words > 0 && words <= 150, "pitch {words} words");
    assert!(
        !d["pitch"]["text"].as_str().unwrap_or("").contains('}'),
        "no JSON debris in the pitch"
    );
    assert!((3..=5).contains(&voice), "voice {voice} sentences");
    assert!(d["evidence"].as_array().is_some_and(|e| !e.is_empty()));
    assert!(eur < 30.0, "cost {eur}");
    assert!(cache_read > 0, "prompt caching is hit");
}

/// With the canned test model on, only a test account gets answers; a real account sees offline.
#[sqlx::test(migrations = "../db/migrations")]
async fn mock_model_answers_test_accounts_only(pool: PgPool) {
    let tester = cc_db::create_user(&pool, "tester@example.org", cc_core::Role::Seeker)
        .await
        .unwrap();
    cc_db::mark_test_user(&pool, tester).await.unwrap();
    cc_db::create_user(&pool, "real@example.org", cc_core::Role::Seeker)
        .await
        .unwrap();
    let (url, seen) = mock_model().await;
    let app = cc_api::app(cc_api::AppState {
        pool: pool.clone(),
        jwt_secret: b"test-jwt-secret-0123456789abcdef0123".to_vec(),
        admin_token: ADMIN.into(),
        llm: Some(cc_llm::Client::new(&url, "mock")),
        eur_per_usd: 1.0,
        web_dir: None,
        test_only: true,
        stt_url: None,
        stt_model: "test".into(),
    });

    let real = login(&app, "real@example.org").await;
    let (_, v) = call(&app, "GET", "/intake", &real, None).await;
    assert_eq!(v["online"], false);
    let (c, v) = call(&app, "POST", "/intake/start", &real, None).await;
    assert_eq!(c, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert!(seen.lock().unwrap().is_empty(), "a real account never reaches the mock");

    let test = login(&app, "tester@example.org").await;
    let (_, v) = call(&app, "GET", "/intake", &test, None).await;
    assert_eq!(v["online"], true);
    let (c, v) = call(&app, "POST", "/intake/start", &test, None).await;
    assert_eq!(c, StatusCode::OK, "{v}");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

/// The company base: a synthetic seed, add-by-URL for anyone signed in, the admin views.
#[sqlx::test(migrations = "../db/migrations")]
async fn company_base_seed_add_admin(pool: PgPool) {
    let seed = "name,website,city,stage,stage_evidence,stage_source_url,headcount_stated,philips_link,philips_source_url\n\
        Testco Health,https://testco.example/about,Testdorp,scale-up,\"Raised a Series A\",https://news.example/testco,40,no,\n\
        ,https://nameless.example,Testdorp,,,,,,\n";
    assert_eq!(cc_db::import_seed(&pool, seed).await.unwrap(), (1, 0, 1));
    assert_eq!(
        cc_db::import_seed(&pool, seed).await.unwrap(),
        (0, 1, 1),
        "import is idempotent"
    );

    cc_db::create_user(&pool, "seeker@example.org", cc_core::Role::Seeker)
        .await
        .unwrap();
    cc_db::create_user(&pool, "admin@example.org", cc_core::Role::Admin)
        .await
        .unwrap();
    let app = app(pool.clone(), None);
    let seeker = login(&app, "seeker@example.org").await;
    let admin = login(&app, "admin@example.org").await;

    let (c, v) = call(
        &app,
        "POST",
        "/companies",
        &seeker,
        Some(json!({"url": "medtech.example/jobs?x=1"})),
    )
    .await;
    assert_eq!(c, StatusCode::OK, "{v}");
    assert_eq!(v["website"], "https://medtech.example/");
    assert_eq!(v["new"], true);
    let (_, again) = call(
        &app,
        "POST",
        "/companies",
        &seeker,
        Some(json!({"url": "https://medtech.example"})),
    )
    .await;
    assert_eq!(
        (again["id"].clone(), again["new"].clone()),
        (v["id"].clone(), json!(false))
    );
    let (c, _) = call(&app, "POST", "/companies", &seeker, Some(json!({"url": "not a url"}))).await;
    assert_eq!(c, StatusCode::BAD_REQUEST);
    assert_eq!(
        call(&app, "GET", "/admin/companies", &seeker, None).await.0,
        StatusCode::FORBIDDEN
    );

    let (c, v) = call(&app, "GET", "/admin/companies", &admin, None).await;
    assert_eq!(c, StatusCode::OK, "{v}");
    let list = v["companies"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    let testco = list.iter().find(|c| c["name"] == "Testco Health").unwrap();
    assert_eq!(
        (testco["status"].clone(), testco["facts"].clone()),
        (json!("queued"), json!(2))
    );
    let id = testco["id"].as_str().unwrap();

    let (c, d) = call(&app, "GET", &format!("/admin/companies/{id}"), &admin, None).await;
    assert_eq!(c, StatusCode::OK, "{d}");
    assert_eq!(d["company"]["city"], "Testdorp");
    assert!(d["facts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["source_url"] == "https://news.example/testco"));
    let (c, v) = call(&app, "POST", &format!("/admin/companies/{id}/enrich"), &admin, None).await;
    assert_eq!((c, v["queued"].clone()), (StatusCode::OK, json!(true)));
    let unknown = format!("/admin/companies/{}", uuid::Uuid::new_v4());
    assert_eq!(call(&app, "GET", &unknown, &admin, None).await.0, StatusCode::NOT_FOUND);
    // One enrich job per company, however often it is asked for.
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'enrich_company'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 2);
}
