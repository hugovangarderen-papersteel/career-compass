//! The intake ("Praat met Kompas") end to end against real Postgres and a mock model server.
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
    "Ik ben Anna en ik woon in Utrecht.",
    "Ik werkte tien jaar als programmamanager bij een ziekenhuisgroep.",
    "Ik heb een team van twaalf mensen door een fusie geleid zonder dat er iemand vertrok.",
    "Ik krijg energie van mensen verbinden die elkaar eerst niet verstonden.",
    "Ik zoek een rol in de zorgtechnologie waar ik iets bouw.",
    "Maximaal drie kwartier reizen en 32 uur per week.",
    "Ik heb een jaar voor mijn vader gezorgd.",
    "Dat klopt zo.",
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
                } else {
                    ""
                };
                let input = match tool {
                    "kompas_turn" => json!({"message": "Vertel eens, wat gebeurde er toen?", "phase_complete": true}),
                    "save_stories" => json!({
                        "timeline": [{"role": "programmamanager", "org": "ziekenhuisgroep", "source_ids": ["T4"]}],
                        "evidence": [{"id": "E1", "title": "Fusie", "situation": "fusie", "task": "team houden",
                            "action": "verbinden", "result": "niemand vertrok", "skills": ["leiderschap"], "source_ids": ["T6"]},
                            {"id": "E2", "title": "Verzonnen", "situation": "x", "task": "x", "action": "x",
                            "result": "x", "skills": [], "source_ids": ["T99"]}],
                        "strengths": [{"skill": "verbinden", "rank": 5, "evidence_ids": ["E1"], "source_ids": ["T8"]}],
                        "languages": [], "anti_prefs": [], "work_style": []
                    }),
                    // As the strict schema returns it: every field present, unmentioned ones empty or null.
                    "save_prefs" => json!({
                        "role_types": [], "stages": [], "mission_themes": [],
                        "max_travel": null, "hours_per_week": null, "work_mode": null, "start_date": null,
                        "subsectors": [{"value": "care_tech", "weight": 0.9, "source_ids": ["T10"]}],
                        "home_base": {"city": "Utrecht", "source_ids": ["T2"]},
                        "gap_framing": {"text": "Een jaar zorg voor mijn vader.", "source_ids": ["T14"]}
                    }),
                    "write_pitch" => json!({
                        "pitch": {"text": "Ik verbind mensen en bouw zorg die werkt.", "source_ids": ["T6", "T8"]},
                        "voice_sample": [
                            {"text": ANSWERS[2], "source_ids": ["T6"]},
                            {"text": ANSWERS[3], "source_ids": ["T8"]},
                            {"text": ANSWERS[4], "source_ids": ["T10"]},
                            {"text": "Ik ben de beste manager van Nederland.", "source_ids": ["T6"]}
                        ]
                    }),
                    "revise_section" => json!({"updated": {
                        "gap_framing": {"text": "Een jaar mantelzorg, bewust gekozen.", "source_ids": ["T18"]}
                    }}),
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
        mock_model: false,
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
        Some(json!({"device_name": "Testtelefoon", "public_key": pk})),
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
    assert_eq!(v["phase_name"], "Terugblik");
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
        Some(json!({"section": "gap", "text": "Noem het mantelzorg, dat was een bewuste keuze."})),
    )
    .await;
    assert_eq!(c, StatusCode::OK, "{v}");
    assert_eq!(
        v["profile"]["data"]["gap_framing"]["text"],
        "Een jaar mantelzorg, bewust gekozen."
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

    // Document stub: pending until she confirms, her word decides.
    let (c, d) = call(
        &online,
        "POST",
        "/intake/documents",
        &her,
        Some(json!({"name": "cv.txt", "body": "Programmamanager, 2014-2024."})),
    )
    .await;
    assert_eq!(c, StatusCode::OK, "{d}");
    let (_, list) = call(&online, "GET", "/intake/documents", &t, None).await;
    assert!(list.as_array().unwrap().is_empty(), "not visible to another account");
    let (_, list) = call(&online, "GET", "/intake/documents", &her, None).await;
    assert_eq!(list[0]["status"], "pending");
    let id = list[0]["id"].as_str().unwrap();
    let (c, _) = call(
        &online,
        "POST",
        &format!("/intake/documents/{id}"),
        &t,
        Some(json!({"confirm": true})),
    )
    .await;
    assert_eq!(c, StatusCode::NOT_FOUND, "the tester cannot confirm her document");
    call(
        &online,
        "POST",
        &format!("/intake/documents/{id}"),
        &her,
        Some(json!({"confirm": true})),
    )
    .await;
    let (_, list) = call(&online, "GET", "/intake/documents", &her, None).await;
    assert_eq!(list[0]["status"], "confirmed");
}

/// Made-up seeker for the live run. A cheap model plays her from this sheet, so she answers
/// what Kompas actually asks. None of it is about a real person.
const PERSONA: &str = "Je speelt Anna (52, Utrecht) in een gesprek met Kompas, een loopbaangesprek in een app. \
Antwoord als Anna, in het Nederlands, in één tot vier natuurlijke zinnen, alleen op wat gevraagd wordt. \
Verzin geen feiten buiten dit blad; weet Anna iets niet, zeg dat. Vraagt Kompas iets te kiezen of te rangschikken, doe dat. \
Zegt Kompas dat het gesprek klaar is of dat ze op 'Maak mijn profiel' kan drukken, antwoord dan: 'Dank je, ik ben klaar.'\n\
Feiten: verpleegkundige 1996-2004 in een regionaal ziekenhuis. Teamleider cardiologie 2004-2010, team van 20. \
Programmamanager bij Medivista (fabrikant van patiëntmonitoring) 2010-2018, team van 15, klanten waren ziekenhuizen. \
Hoofd implementatie bij Medivista 2018-2022: nieuwe monitoringsystemen invoeren in 12 ziekenhuizen. \
Van 2022 tot 2024 thuis voor de zorg van haar moeder; hield vakbladen bij en deed een online cursus over de MDR. \
Verhalen: (1) bij één ziekenhuis wilde de verpleging het systeem niet; zij draaide drie weken nachtdiensten mee, liet het aanpassen, \
na twee maanden gebruikte 90 procent het dagelijks. (2) een implementatie die zes maanden achterliep knipte ze in drie stappen, \
binnen vier maanden weer op schema. (3) bij een fusie van twee implementatieteams vertrok niemand in het eerste jaar. \
(4) ze schreef de trainingsopzet die daarna in alle 12 ziekenhuizen werd gebruikt, opleidingstijd van 2 dagen naar 1. \
Sterktes: vertalen tussen techniek en werkvloer, mensen meekrijgen, grote trajecten in kleine stappen knippen, rustig blijven onder druk. \
Energie: mensen verbinden, iets zien werken op de afdeling. Kost energie: vergaderen zonder besluit, politiek. \
Zoekt: implementatie- of productrol, liefst vast of parttime, scale-up of middelgroot bedrijf, digitale zorg of zorgtechnologie, \
patiëntveiligheid raakt haar het meest. Maximaal 45 minuten reizen met trein of auto, 28 tot 32 uur, hybride, kan per maart, \
Nederlands en goed Engels. Pauze-zin die ze goed vindt: twee jaar mantelzorg, en ze bleef bij in haar vak.";

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
        mock_model: false,
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
            // What her "Probeer opnieuw" does: her answer is saved, ask Kompas again.
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
        mock_model: true,
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
