//! Career Compass window: pairing, login, the menu and "Praat met Kompas" (Phase 1).
//! All network and key work happens behind `invoke`: the Tauri side on the Mac, `shim.js` in
//! the web build. This file never talks to the server itself.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Serialize;
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["window", "__TAURI__", "core"])]
    async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_namespace = window, js_name = scrollTo)]
    fn scroll_to(x: f64, y: f64);
}

async fn call<A: Serialize>(cmd: &str, args: &A) -> Result<Value, String> {
    // json_compatible: objects go over as plain JS objects, not Maps.
    let a = args
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|e| e.to_string())?;
    match invoke(cmd, a).await {
        Ok(v) => serde_wasm_bindgen::from_value(v).map_err(|e| e.to_string()),
        Err(e) => Err(e.as_string().unwrap_or_else(|| "onbekende fout".into())),
    }
}

#[derive(Serialize)]
struct NoArgs {}

#[derive(Serialize)]
struct ApiArgs {
    method: &'static str,
    path: String,
    body: Option<Value>,
}

/// Her own data on the server (`/intake...`, `/me`); the native side allows nothing else.
async fn api(method: &'static str, path: &str, body: Option<Value>) -> Result<Value, String> {
    call(
        "api",
        &ApiArgs {
            method,
            path: path.into(),
            body,
        },
    )
    .await
}

#[derive(Clone, PartialEq)]
enum Screen {
    Loading,
    Unpaired(Option<String>),
    Waiting(String),
    Login,
    Home,
}

const MENU: [&str; 9] = [
    "Deze week",
    "Praat met Kompas",
    "Bedrijven",
    "Kaart",
    "Verhalen & media",
    "Outreach-studio",
    "Pijplijn",
    "Mijn ranglijst",
    "Instellingen",
];

/// Turns a server or app error into a sentence she can act on.
fn nl(e: &str) -> String {
    match e {
        "unauthorized" => "Dat klopt niet. Controleer je e-mailadres en wachtwoord.".into(),
        "password_too_short" => "Kies een wachtwoord van minstens 8 tekens.".into(),
        "password_already_set" => "Er is al een wachtwoord. Log gewoon in.".into(),
        "denied" => "De koppeling is niet goedgekeurd. Vraag Hugo even waarom.".into(),
        "expired" => "De code is verlopen (na 15 minuten). Vraag een nieuwe code aan.".into(),
        "revoked" => "Dit apparaat is ontkoppeld. Koppel het opnieuw.".into(),
        "too_many_pending" => "Er wachten al te veel koppelverzoeken. Probeer het later nog eens.".into(),
        "offline" => "Geen verbinding met Career Compass. Staat het internet aan?".into(),
        "insecure" => "Deze pagina moet via een beveiligde verbinding (https) open.".into(),
        "kompas_offline" | "cap_reached" => "Kompas pauzeert even. Je antwoord is bewaard.".into(),
        "profile_failed" => "Je profiel lukte niet in één keer. Probeer het nog eens.".into(),
        "not_ready_for_playback" => "Het gesprek is nog niet klaar voor je profiel.".into(),
        "no_active_intake" | "not_in_playback" | "no_draft" => {
            "Dit scherm was niet meer actueel. Kijk nog eens.".into()
        }
        "unknown_section" => "Dat deel van je profiel ken ik niet.".into(),
        "too_long" => "Dat is te lang. Maak het wat korter.".into(),
        "empty" => "Er staat nog niets in.".into(),
        "forbidden" | "not_allowed" => "Dat kan hier niet.".into(),
        other => format!("Er ging iets mis ({other})."),
    }
}

fn wait_for_approval(code: String, set_screen: WriteSignal<Screen>) {
    set_screen.set(Screen::Waiting(code));
    spawn_local(async move {
        match call("pair_wait", &NoArgs {}).await {
            Ok(v) if v["status"] == "approved" => set_screen.set(Screen::Login),
            Ok(v) => set_screen.set(Screen::Unpaired(Some(nl(v["status"].as_str().unwrap_or(""))))),
            Err(e) => set_screen.set(Screen::Unpaired(Some(nl(&e)))),
        }
    });
}

#[component]
fn App() -> impl IntoView {
    let (screen, set_screen) = signal(Screen::Loading);

    spawn_local(async move {
        match call("pair_check", &NoArgs {}).await {
            Ok(v) => match v["status"].as_str().unwrap_or("") {
                "paired" => set_screen.set(Screen::Login),
                "pending" => wait_for_approval(v["code"].as_str().unwrap_or("").to_string(), set_screen),
                "unpaired" => set_screen.set(Screen::Unpaired(None)),
                other => set_screen.set(Screen::Unpaired(Some(nl(other)))),
            },
            Err(e) => set_screen.set(Screen::Unpaired(Some(nl(&e)))),
        }
    });

    move || match screen.get() {
        Screen::Loading => view! { <div class="center"><p>"Even geduld..."</p></div> }.into_any(),
        Screen::Unpaired(err) => view! { <Pair err=err set_screen=set_screen/> }.into_any(),
        Screen::Waiting(code) => view! {
            <div class="center"><div class="card">
                <h1>"Wachten op goedkeuring"</h1>
                <p>"Hugo krijgt nu een bericht. Zie je deze code ook bij hem? Dan klopt het."</p>
                <div class="code">{code}</div>
                <p>"Dit scherm gaat vanzelf verder zodra hij akkoord geeft."</p>
            </div></div>
        }
        .into_any(),
        Screen::Login => view! { <Login set_screen=set_screen/> }.into_any(),
        Screen::Home => view! { <Home set_screen=set_screen/> }.into_any(),
    }
}

#[component]
fn Pair(err: Option<String>, set_screen: WriteSignal<Screen>) -> impl IntoView {
    #[derive(Serialize)]
    struct Args {
        #[serde(rename = "deviceName")]
        device_name: String,
    }
    let (busy, set_busy) = signal(false);
    let (error, set_error) = signal(err);
    let start = move |_| {
        set_busy.set(true);
        spawn_local(async move {
            match call(
                "pair_start",
                &Args {
                    device_name: "Mac van mama".into(),
                },
            )
            .await
            {
                Ok(v) => wait_for_approval(v["code"].as_str().unwrap_or("").to_string(), set_screen),
                Err(e) => {
                    set_error.set(Some(nl(&e)));
                    set_busy.set(false);
                }
            }
        });
    };
    view! {
        <div class="center"><div class="card">
            <h1>"Welkom bij Career Compass"</h1>
            <p>"Dit apparaat moet eerst één keer gekoppeld worden. Hugo keurt dat goed op zijn telefoon."</p>
            <button on:click=start disabled=busy>"Koppel dit apparaat"</button>
            {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
        </div></div>
    }
}

#[component]
fn Login(set_screen: WriteSignal<Screen>) -> impl IntoView {
    #[derive(Serialize)]
    struct Args {
        email: String,
        password: String,
    }
    let (first, set_first) = signal(false);
    let (error, set_error) = signal(None::<String>);
    let email = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let submit = move |_| {
        let cmd = if first.get() { "set_password" } else { "login" };
        let args = Args {
            email: email.get(),
            password: password.get(),
        };
        spawn_local(async move {
            match call(cmd, &args).await {
                Ok(_) => set_screen.set(Screen::Home),
                Err(e) if e == "revoked" => set_screen.set(Screen::Unpaired(Some(nl(&e)))),
                Err(e) => set_error.set(Some(nl(&e))),
            }
        });
    };
    view! {
        <div class="center"><div class="card">
            <h1>{move || if first.get() { "Kies je wachtwoord" } else { "Inloggen" }}</h1>
            <label>"E-mailadres"</label>
            <input type="email" bind:value=email/>
            <label>{move || if first.get() { "Nieuw wachtwoord (minstens 8 tekens)" } else { "Wachtwoord" }}</label>
            <input type="password" bind:value=password/>
            <button on:click=submit>{move || if first.get() { "Opslaan en verder" } else { "Inloggen" }}</button>
            <button class="link" on:click=move |_| set_first.update(|f| *f = !*f)>
                {move || if first.get() { "Ik heb al een wachtwoord" } else { "Eerste keer hier? Kies een wachtwoord" }}
            </button>
            {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
        </div></div>
    }
}

#[component]
fn Home(set_screen: WriteSignal<Screen>) -> impl IntoView {
    let (page, set_page) = signal(1usize);
    view! {
        <div class="shell">
            <nav>
                <div class="brand">"Career Compass"</div>
                {MENU
                    .iter()
                    .enumerate()
                    .map(|(i, name)| view! {
                        <a class:active=move || page.get() == i on:click=move |_| set_page.set(i)>{*name}</a>
                    })
                    .collect_view()}
            </nav>
            <main>
                {move || if page.get() == 1 {
                    view! { <Kompas set_screen=set_screen/> }.into_any()
                } else {
                    view! {
                        <h1>{MENU[page.get()]}</h1>
                        <p>"Dit onderdeel komt in een volgende versie. De app werkt zichzelf bij zodra het klaar is."</p>
                    }
                    .into_any()
                }}
            </main>
        </div>
    }
}

/// Shared state of the Kompas page; every handler copies it.
#[derive(Clone, Copy)]
struct Ctx {
    data: RwSignal<Value>,
    docs: RwSignal<Value>,
    busy: RwSignal<Option<&'static str>>,
    error: RwSignal<Option<String>>,
    retry: RwSignal<bool>,
    set_screen: WriteSignal<Screen>,
}

impl Ctx {
    /// A failed call: back to login when the session ran out, else say it and reload what was saved.
    async fn fail(self, e: &str) {
        match e {
            "unauthorized" => self.set_screen.set(Screen::Login),
            "revoked" => self.set_screen.set(Screen::Unpaired(Some(nl(e)))),
            _ => {
                self.error.set(Some(nl(e)));
                if let Ok(v) = api("GET", "/intake", None).await {
                    self.data.set(v);
                }
            }
        }
    }

    /// POSTs in order (stops at the first error); each answer is the fresh page state.
    fn go(self, steps: Vec<(&'static str, Option<Value>)>, busy: &'static str) {
        self.busy.set(Some(busy));
        self.error.set(None);
        spawn_local(async move {
            for (path, body) in steps {
                match api("POST", path, body).await {
                    Ok(v) => {
                        self.data.set(v);
                        self.retry.set(false);
                    }
                    Err(e) => {
                        self.fail(&e).await;
                        break;
                    }
                }
            }
            self.busy.set(None);
        });
    }

    /// Document calls answer with her document list.
    fn docs_call(self, path: String, body: Value) {
        self.error.set(None);
        spawn_local(async move {
            match api("POST", &path, Some(body)).await {
                Ok(v) => self.docs.set(v),
                Err(e) => self.fail(&e).await,
            }
        });
    }
}

const THINKING: &str = "Kompas denkt na…";

#[component]
fn Kompas(set_screen: WriteSignal<Screen>) -> impl IntoView {
    let ctx = Ctx {
        data: RwSignal::new(Value::Null),
        docs: RwSignal::new(json!([])),
        busy: RwSignal::new(None),
        error: RwSignal::new(None),
        retry: RwSignal::new(false),
        set_screen,
    };
    spawn_local(async move {
        match api("GET", "/intake", None).await {
            Ok(v) => ctx.data.set(v),
            Err(e) => ctx.fail(&e).await,
        }
        if let Ok(d) = api("GET", "/intake/documents", None).await {
            ctx.docs.set(d);
        }
    });

    move || {
        let v = ctx.data.get();
        if v.is_null() {
            return view! { <p>"Even geduld..."</p> }.into_any();
        }
        let body = match v["status"].as_str().unwrap_or("none") {
            "none" => view! { <Intro ctx=ctx/> }.into_any(),
            "active" => view! { <Chat ctx=ctx v=v.clone()/> }.into_any(),
            _ => view! { <Playback ctx=ctx v=v.clone()/> }.into_any(),
        };
        view! {
            <div class="kompas">
                <Phases v=v.clone()/>
                {body}
                {move || ctx.busy.get().map(|b| view! { <p class="busy">{b}</p> })}
                {move || ctx.error.get().map(|e| view! { <p class="error">{e}</p> })}
                <Documents ctx=ctx/>
                {v["is_test"].as_bool().unwrap_or(false).then(|| view! { <Reset ctx=ctx/> })}
            </div>
        }
        .into_any()
    }
}

#[component]
fn Phases(v: Value) -> impl IntoView {
    let phase = v["phase"].as_u64().unwrap_or(1);
    let covered = v["covered"].as_u64().unwrap_or(0);
    let total = v["total"].as_u64().unwrap_or(8);
    let approved = v["status"] == "approved";
    let name = v["phase_name"].as_str().unwrap_or("").to_string();
    view! {
        <header class="phases">
            <h1>"Praat met Kompas"</h1>
            <p>{format!("Fase {phase} van {total} · {name}")}</p>
            <div class="dots">
                {(1..=total)
                    .map(|i| {
                        let c = if approved || i <= covered { "dot done" } else if i == phase { "dot now" } else { "dot" };
                        view! { <span class=c></span> }
                    })
                    .collect_view()}
            </div>
        </header>
    }
}

#[component]
fn Intro(ctx: Ctx) -> impl IntoView {
    view! {
        <div class="card intro">
            <p>"Kompas stelt je steeds één korte vraag: over je werk, je verhalen en wat je zoekt. Er zijn geen goede of foute antwoorden."</p>
            <p>"Na elk antwoord wordt alles bewaard. Je kunt stoppen wanneer je wilt en later verdergaan waar je was."</p>
            <button
                on:click=move |_| ctx.go(vec![("/intake/start", None)], THINKING)
                disabled=move || ctx.busy.get().is_some()
            >"Begin het gesprek"</button>
        </div>
    }
}

#[component]
fn Chat(ctx: Ctx, v: Value) -> impl IntoView {
    let input = RwSignal::new(String::new());
    let turns = v["turns"].as_array().cloned().unwrap_or_default();
    let ready = v["phase"].as_u64() == Some(8);
    let send = move |_| {
        let text = input.get();
        if text.trim().is_empty() {
            return;
        }
        ctx.busy.set(Some(THINKING));
        ctx.error.set(None);
        spawn_local(async move {
            match api("POST", "/intake/message", Some(json!({ "text": text }))).await {
                Ok(v) => {
                    input.set(String::new());
                    ctx.retry.set(false);
                    ctx.data.set(v);
                }
                Err(e) => {
                    // Her answer is already saved when only Kompas failed: offer "try again".
                    if e == "kompas_offline" || e == "cap_reached" {
                        input.set(String::new());
                        ctx.retry.set(true);
                    }
                    ctx.fail(&e).await;
                }
            }
            ctx.busy.set(None);
            scroll_to(0.0, 1e7);
        });
    };
    request_animation_frame(|| scroll_to(0.0, 1e7));
    view! {
        <div class="chat">
            {turns
                .iter()
                .map(|t| {
                    let who = if t["speaker"] == "seeker" { "bubble her" } else { "bubble kompas" };
                    view! { <div class=who>{t["text"].as_str().unwrap_or("").to_string()}</div> }
                })
                .collect_view()}
        </div>
        {move || ctx.retry.get().then(|| view! {
            <button class="secondary" on:click=move |_| ctx.go(vec![("/intake/message", Some(json!({ "text": "" })))], THINKING)>
                "Probeer opnieuw"
            </button>
        })}
        {ready.then(|| view! {
            <div class="card ready">
                <p>"Kompas heeft genoeg om je profiel te maken. Je kunt ook eerst nog iets aanvullen."</p>
                <button on:click=move |_| ctx.go(
                    vec![("/intake/playback", None), ("/intake/pitch", None)],
                    "Kompas maakt je profiel. Dat duurt ongeveer een minuut…",
                )>"Maak mijn profiel"</button>
            </div>
        })}
        <div class="composer">
            <textarea
                rows="3"
                placeholder="Typ je antwoord…"
                prop:value=move || input.get()
                on:input=move |ev| input.set(event_target_value(&ev))
            ></textarea>
            <div class="row">
                <button class="secondary" disabled=true title="Spreken komt later">"🎤 Spreken komt later"</button>
                <button on:click=send disabled=move || ctx.busy.get().is_some()>"Verstuur"</button>
            </div>
        </div>
    }
}

/// Dutch label of a profile field inside a section card.
fn field_label(key: &str) -> &'static str {
    match key {
        "work_style" => "Zo werk je graag",
        "anti_prefs" => "Liever niet",
        "strengths" => "Sterke punten",
        "role_types" => "Soort rol",
        "stages" => "Soort organisatie",
        "subsectors" => "Sector",
        "mission_themes" => "Waar je warm voor loopt",
        "home_base" => "Woonplaats",
        "max_travel" => "Reistijd",
        "hours_per_week" => "Uren per week",
        "work_mode" => "Werkvorm",
        "start_date" => "Beschikbaar vanaf",
        "languages" => "Talen",
        _ => "",
    }
}

fn code_nl(c: &str) -> String {
    match c {
        "permanent" => "Vaste baan",
        "interim" => "Interim",
        "part_time" => "Parttime",
        "advisory" => "Advies of toezicht",
        "startup" => "Start-up",
        "scaleup" => "Scale-up",
        "midsize" => "Middelgroot bedrijf",
        "corporate" => "Groot bedrijf",
        "medtech" => "Medische technologie",
        "digital_health" => "Digitale zorg",
        "care_tech" => "Zorgtechnologie",
        "diagnostics" => "Diagnostiek",
        "other" => "Anders",
        other => other,
    }
    .to_string()
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").trim().to_string()
}

/// One readable line per item of a profile field.
fn line(key: &str, it: &Value) -> String {
    match key {
        "timeline" => {
            let when = match (s(&it["start"]), s(&it["end"])) {
                (a, b) if a.is_empty() && b.is_empty() => String::new(),
                (a, b) => format!(" ({a}–{b})"),
            };
            let scope = s(&it["scope"]);
            let scope = if scope.is_empty() { scope } else { format!(". {scope}") };
            format!("{} · {}{when}{scope}", s(&it["role"]), s(&it["org"]))
        }
        "evidence" => {
            let metric = s(&it["metric"]);
            let metric = if metric.is_empty() {
                metric
            } else {
                format!(" ({metric})")
            };
            format!(
                "{}: {} {} {}{metric}",
                s(&it["title"]),
                s(&it["situation"]),
                s(&it["action"]),
                s(&it["result"])
            )
        }
        "strengths" => s(&it["skill"]),
        "role_types" | "stages" | "subsectors" | "mission_themes" => code_nl(&s(&it["value"])),
        "home_base" => s(&it["city"]),
        "max_travel" => {
            let mode = s(&it["mode"]);
            let mode = if mode.is_empty() { mode } else { format!(", {mode}") };
            format!("Hooguit {} minuten{mode}", it["minutes"])
        }
        "hours_per_week" => format!("{} tot {} uur", it["min"], it["max"]),
        "languages" => format!("{} ({})", s(&it["language"]), s(&it["level"])),
        _ => s(&it["text"]),
    }
}

fn items(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Null => vec![],
        other => vec![other.clone()],
    }
}

/// What a source id points at: her own words, Kompas's question, or a document she confirmed.
fn quote(id: &str, turns: &Value, docs: &Value) -> String {
    let find = |list: &Value, key: &str, want: String| {
        list.as_array()
            .and_then(|a| a.iter().find(|t| t[key].to_string().trim_matches('"') == want).cloned())
    };
    if let Some(n) = id.strip_prefix('T') {
        if let Some(t) = find(turns, "seq", n.to_string()) {
            let who = if t["speaker"] == "seeker" { "Jij" } else { "Kompas" };
            return format!("{who}: “{}”", s(&t["text"]));
        }
    } else if let Some(d) = find(docs, "label", id.to_string()) {
        return format!("Uit je document “{}”", s(&d["name"]));
    }
    "Deze bron is niet meer te vinden.".into()
}

#[component]
fn Playback(ctx: Ctx, v: Value) -> impl IntoView {
    let approved = v["status"] == "approved";
    let data = v["profile"]["data"].clone();
    let no_pitch = s(&data["pitch"]["text"]).is_empty();
    let sections = v["sections"].as_array().cloned().unwrap_or_default();
    let turns = v["turns"].clone();
    view! {
        {if approved {
            view! { <div class="card done"><p>"Je profiel is vastgelegd. Dit ben jij, in je eigen woorden."</p></div> }.into_any()
        } else {
            view! { <p class="lead">"Lees elk deel rustig door. Klopt iets niet, schrijf dan in je eigen woorden hoe het wel zit. Jouw woorden gaan altijd voor."</p> }.into_any()
        }}
        {(no_pitch && !approved).then(|| view! {
            <button class="secondary" on:click=move |_| ctx.go(vec![("/intake/pitch", None)], "Kompas schrijft je pitch…")>
                "Schrijf mijn pitch"
            </button>
        })}
        {sections
            .into_iter()
            .map(|sec| view! { <Section ctx=ctx sec=sec data=data.clone() turns=turns.clone() approved=approved/> })
            .collect_view()}
        {(!approved).then(|| view! {
            <button class="approve" on:click=move |_| ctx.go(vec![("/intake/approve", None)], "Even vastleggen…")>
                "Dit ben ik"
            </button>
        })}
    }
}

#[component]
fn Section(ctx: Ctx, sec: Value, data: Value, turns: Value, approved: bool) -> impl IntoView {
    let key = s(&sec["key"]);
    let shown = RwSignal::new(None::<String>);
    let editing = RwSignal::new(false);
    let fix = RwSignal::new(String::new());
    let fields: Vec<String> = sec["fields"]
        .as_array()
        .map(|a| a.iter().map(s).collect())
        .unwrap_or_default();
    let words = s(&data["pitch"]["text"]).split_whitespace().count();
    let body = fields
        .iter()
        .map(|f| {
            let rows = items(&data[f.as_str()]);
            let label = field_label(f);
            let list = if rows.len() == 1 { "one" } else { "" };
            let lines = rows
                .iter()
                .map(|it| {
                    let ids: Vec<String> = it["source_ids"]
                        .as_array()
                        .map(|a| a.iter().map(s).collect())
                        .unwrap_or_default();
                    let text = line(f, it);
                    let turns = turns.clone();
                    view! {
                        <li>
                            {text}
                            {ids
                                .into_iter()
                                .enumerate()
                                .map(|(i, id)| {
                                    let turns = turns.clone();
                                    view! {
                                        <button class="chip" on:click=move |_| {
                                            shown.set(Some(quote(&id, &turns, &ctx.docs.get_untracked())))
                                        }>{format!("bron {}", i + 1)}</button>
                                    }
                                })
                                .collect_view()}
                        </li>
                    }
                })
                .collect_view();
            view! {
                {(!label.is_empty()).then(|| view! { <h3>{label}</h3> })}
                {rows.is_empty().then(|| view! { <p class="empty">"Nog niets."</p> })}
                <ul class=list>{lines}</ul>
            }
        })
        .collect_view();
    let k2 = key.clone();
    view! {
        <section class="card section">
            <h2>{s(&sec["title"])}</h2>
            {(key == "pitch" && words > 0).then(|| view! { <p class="meta">{format!("{words} woorden")}</p> })}
            {body}
            {move || shown.get().map(|q| view! { <blockquote on:click=move |_| shown.set(None)>{q}</blockquote> })}
            {(!approved).then(move || view! {
                {move || if editing.get() {
                    let k = k2.clone();
                    view! {
                        <textarea
                            rows="3"
                            placeholder="Hoe zit het wel?"
                            prop:value=move || fix.get()
                            on:input=move |ev| fix.set(event_target_value(&ev))
                        ></textarea>
                        <div class="row">
                            <button class="secondary" on:click=move |_| editing.set(false)>"Laat maar"</button>
                            <button on:click=move |_| ctx.go(
                                vec![("/intake/correct", Some(json!({ "section": k.clone(), "text": fix.get() })))],
                                "Kompas past dit deel aan…",
                            )>"Pas aan"</button>
                        </div>
                    }
                    .into_any()
                } else {
                    view! { <button class="link" on:click=move |_| editing.set(true)>"Dit klopt niet helemaal"</button> }.into_any()
                }}
            })}
        </section>
    }
}

#[component]
fn Documents(ctx: Ctx) -> impl IntoView {
    let open = RwSignal::new(false);
    let name = RwSignal::new(String::new());
    let body = RwSignal::new(String::new());
    let add = move |_| {
        ctx.docs_call(
            "/intake/documents".into(),
            json!({ "name": name.get(), "body": body.get() }),
        );
        name.set(String::new());
        body.set(String::new());
    };
    view! {
        <details class="card docs" prop:open=move || open.get() on:toggle=move |_| open.update(|o| *o = !*o)>
            <summary>"Een document toevoegen (cv, LinkedIn-tekst)"</summary>
            <p class="meta">"Kompas gebruikt een document pas als jij het bevestigt. Wat jij zelf zegt, gaat altijd voor."</p>
            {move || ctx.docs.get().as_array().cloned().unwrap_or_default().into_iter().map(|d| {
                let id = s(&d["id"]);
                let pending = d["status"] == "pending";
                let state = match s(&d["status"]).as_str() {
                    "confirmed" => "gebruikt",
                    "rejected" => "niet gebruikt",
                    _ => "wacht op jou",
                };
                let (a, b) = (id.clone(), id);
                view! {
                    <div class="doc">
                        <strong>{s(&d["name"])}</strong>" · "{state}
                        {pending.then(|| view! {
                            <div class="row">
                                <button class="secondary" on:click=move |_| ctx.docs_call(format!("/intake/documents/{b}"), json!({ "confirm": false }))>"Nee"</button>
                                <button on:click=move |_| ctx.docs_call(format!("/intake/documents/{a}"), json!({ "confirm": true }))>"Ja, gebruik dit"</button>
                            </div>
                        })}
                    </div>
                }
            }).collect_view()}
            <label>"Naam"</label>
            <input prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev))/>
            <label>"Tekst (plakken)"</label>
            <textarea rows="5" prop:value=move || body.get() on:input=move |ev| body.set(event_target_value(&ev))></textarea>
            <button on:click=add>"Toevoegen"</button>
        </details>
    }
}

/// Test accounts only (the server refuses it for a real account anyway).
#[component]
fn Reset(ctx: Ctx) -> impl IntoView {
    let sure = RwSignal::new(false);
    view! {
        <div class="testbar">
            {move || if sure.get() {
                view! {
                    <button class="danger" on:click=move |_| {
                        sure.set(false);
                        ctx.go(vec![("/intake/reset", None)], "Opnieuw beginnen…");
                    }>"Zeker weten: alles wissen"</button>
                    <button class="link" on:click=move |_| sure.set(false)>"Toch niet"</button>
                }
                .into_any()
            } else {
                view! { <button class="link" on:click=move |_| sure.set(true)>"Testaccount: opnieuw beginnen"</button> }.into_any()
            }}
        </div>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
