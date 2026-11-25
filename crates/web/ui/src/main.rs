//! Career Compass window: pairing, login, the menu and "Talk to Kompas" (Phase 1).
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

// The microphone and file picker, in plain browser JS (no web-sys): each hands back
// `{mime|name, data}` with the bytes as base64, because the bridge to the server carries JSON only.
#[wasm_bindgen(inline_js = r#"
let rec = null, chunks = [];
async function b64(blob) {
  const b = new Uint8Array(await blob.arrayBuffer());
  let s = "";
  for (let i = 0; i < b.length; i += 32768) s += String.fromCharCode(...b.subarray(i, i + 32768));
  return btoa(s);
}
export async function mic_start() {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  chunks = [];
  rec = new MediaRecorder(stream);
  rec.ondataavailable = (e) => { if (e.data.size) chunks.push(e.data); };
  rec.start();
}
export function mic_stop() {
  return new Promise((ok, no) => {
    const r = rec;
    rec = null;
    if (!r) return no("bad_audio");
    r.onstop = async () => {
      r.stream.getTracks().forEach((t) => t.stop());
      ok({ mime: r.mimeType, data: await b64(new Blob(chunks, { type: r.mimeType })) });
    };
    r.stop();
  });
}
export async function read_file(ev) {
  const input = ev.target, f = input.files && input.files[0];
  input.value = "";
  return f ? { name: f.name, data: await b64(f) } : null;
}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    async fn mic_start() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch)]
    async fn mic_stop() -> Result<JsValue, JsValue>;
    #[wasm_bindgen(catch)]
    async fn read_file(ev: JsValue) -> Result<JsValue, JsValue>;
}

/// A JS helper's answer as JSON; `None` when it failed or gave nothing.
fn js_json(r: Result<JsValue, JsValue>) -> Option<Value> {
    r.ok()
        .and_then(|v| serde_wasm_bindgen::from_value::<Value>(v).ok())
        .filter(|v| !v.is_null())
}

async fn call<A: Serialize>(cmd: &str, args: &A) -> Result<Value, String> {
    // json_compatible: objects go over as plain JS objects, not Maps.
    let a = args
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(|e| e.to_string())?;
    match invoke(cmd, a).await {
        Ok(v) => serde_wasm_bindgen::from_value(v).map_err(|e| e.to_string()),
        Err(e) => Err(e.as_string().unwrap_or_else(|| "unknown error".into())),
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
    "This week",
    "Talk to Kompas",
    "Companies",
    "Map",
    "Stories & media",
    "Outreach studio",
    "Pipeline",
    "My ranking",
    "Settings",
];

/// Turns a server or app error into a sentence she can act on.
fn nl(e: &str) -> String {
    match e {
        "unauthorized" => "That didn't work. Please check your email address and password.".into(),
        "password_too_short" => "Please choose a password of at least 8 characters.".into(),
        "password_already_set" => "You already have a password. Just log in.".into(),
        "denied" => "This device wasn't approved. Please ask Hugo why.".into(),
        "expired" => "The code has expired (it lasts 15 minutes). Please ask for a new code.".into(),
        "revoked" => "This device has been disconnected. Please connect it again.".into(),
        "too_many_pending" => "Too many connection requests are waiting. Please try again later.".into(),
        "offline" => "Can't reach Career Compass. Is the internet on?".into(),
        "insecure" => "This page has to be opened over a secure connection (https).".into(),
        "kompas_offline" | "cap_reached" => "Kompas is taking a short break. Your answer has been saved.".into(),
        "profile_failed" => "Your profile didn't come together in one go. Please try again.".into(),
        "not_ready_for_playback" => "The conversation isn't ready for your profile yet.".into(),
        "no_active_intake" | "not_in_playback" | "no_draft" => {
            "This screen was out of date. Please have another look.".into()
        }
        "unknown_section" => "I don't recognise that part of your profile.".into(),
        "too_long" => "That's too long. Please make it a little shorter.".into(),
        "empty" => "There's nothing in it yet.".into(),
        "no_password_yet" => "Welcome! Please choose your password first.".into(),
        "stt_offline" => "Speaking doesn't work right now. Please type your answer instead.".into(),
        "no_mic" => "Career Compass may not use the microphone. You can allow it in System Settings, under Privacy & Security, Microphone.".into(),
        "bad_audio" => "That recording didn't come through. Please try again.".into(),
        "no_speech" => "I didn't hear any words. Please try again, a little closer to the microphone.".into(),
        "unsupported_file" => "I can read PDF, Word (.docx) and text files. Please choose one of those.".into(),
        "unreadable_file" => "I couldn't open that file. Is it perhaps damaged or protected?".into(),
        "no_text" => "I couldn't find any text in that file. Is it perhaps a scan? Then please paste the text instead.".into(),
        "bad_url" => "That doesn't look like a website address. Please paste the company's site, e.g. https://example.com.".into(),
        "forbidden" | "not_allowed" => "That isn't possible here.".into(),
        other => format!("Something went wrong ({other})."),
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
        Screen::Loading => view! { <div class="center"><p>"One moment..."</p></div> }.into_any(),
        Screen::Unpaired(err) => view! { <Pair err=err set_screen=set_screen/> }.into_any(),
        Screen::Waiting(code) => view! {
            <div class="center"><div class="card">
                <h1>"Waiting for approval"</h1>
                <p>"Hugo is getting a message now. Does he see this same code? Then all is well."</p>
                <div class="code">{code}</div>
                <p>"This screen moves on by itself as soon as he says yes."</p>
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
            <h1>"Welcome to Career Compass"</h1>
            <p>"First, this device needs to be connected, just once. Hugo approves it on his phone."</p>
            <button on:click=start disabled=busy>"Connect this device"</button>
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
                Err(e) if e == "no_password_yet" => {
                    set_first.set(true);
                    set_error.set(Some(nl(&e)));
                }
                Err(e) => set_error.set(Some(nl(&e))),
            }
        });
    };
    view! {
        <div class="center"><div class="card">
            <h1>{move || if first.get() { "Choose your password" } else { "Log in" }}</h1>
            <label>"Email address"</label>
            <input type="email" bind:value=email/>
            <label>{move || if first.get() { "New password (at least 8 characters)" } else { "Password" }}</label>
            <input type="password" bind:value=password/>
            <button on:click=submit>{move || if first.get() { "Save and continue" } else { "Log in" }}</button>
            <button class="link" on:click=move |_| set_first.update(|f| *f = !*f)>
                {move || if first.get() { "I already have a password" } else { "First time here? Choose a password" }}
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
                } else if page.get() == 2 {
                    view! { <Companies set_screen=set_screen/> }.into_any()
                } else {
                    view! {
                        <h1>{MENU[page.get()]}</h1>
                        <p>"This part is coming in a later version. The app will update itself as soon as it's ready."</p>
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
    fn docs_call(self, path: String, body: Value, busy: Option<&'static str>) {
        self.error.set(None);
        self.busy.set(busy);
        spawn_local(async move {
            match api("POST", &path, Some(body)).await {
                Ok(v) => self.docs.set(v),
                Err(e) => self.fail(&e).await,
            }
            self.busy.set(None);
        });
    }
}

const THINKING: &str = "Kompas is thinking…";

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
            return view! { <p>"One moment..."</p> }.into_any();
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
            <h1>"Talk to Kompas"</h1>
            <p>{format!("Step {phase} of {total} · {name}")}</p>
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
            <p>"Kompas asks you one short question at a time: about your work, your stories and what you are looking for. There are no right or wrong answers."</p>
            <p>"Everything is saved after each answer. You can stop whenever you like and carry on later where you left off."</p>
            <button
                on:click=move |_| ctx.go(vec![("/intake/start", None)], THINKING)
                disabled=move || ctx.busy.get().is_some()
            >"Start the conversation"</button>
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
    // Push to talk: press to record, press again to stop; the text lands in her box to check.
    let rec = RwSignal::new(false);
    let talk = move |_| {
        ctx.error.set(None);
        if !rec.get() {
            spawn_local(async move {
                match mic_start().await {
                    Ok(_) => rec.set(true),
                    Err(_) => ctx.error.set(Some(nl("no_mic"))),
                }
            });
            return;
        }
        rec.set(false);
        ctx.busy.set(Some("Turning your words into text…"));
        spawn_local(async move {
            let said = match js_json(mic_stop().await) {
                None => Err("bad_audio".to_string()),
                Some(clip) => api("POST", "/intake/audio", Some(clip)).await.map(|v| s(&v["text"])),
            };
            match said {
                Ok(t) if t.is_empty() => ctx.error.set(Some(nl("no_speech"))),
                Ok(t) => input.update(|i| {
                    if !i.trim().is_empty() {
                        i.push(' ');
                    }
                    i.push_str(&t);
                }),
                // Not `fail`: that reloads the page and would wipe what she typed.
                Err(e) if e == "unauthorized" || e == "revoked" => ctx.fail(&e).await,
                Err(e) => ctx.error.set(Some(nl(&e))),
            }
            ctx.busy.set(None);
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
                "Try again"
            </button>
        })}
        {ready.then(|| view! {
            <div class="card ready">
                <p>"Kompas has enough to make your profile. You can also add something first."</p>
                <button on:click=move |_| ctx.go(
                    vec![("/intake/playback", None), ("/intake/pitch", None)],
                    "Kompas is making your profile. This takes about a minute…",
                )>"Make my profile"</button>
            </div>
        })}
        <div class="composer">
            <textarea
                rows="3"
                placeholder="Type your answer…"
                prop:value=move || input.get()
                on:input=move |ev| input.set(event_target_value(&ev))
            ></textarea>
            <div class="row">
                <button class="secondary" on:click=talk disabled=move || ctx.busy.get().is_some()>
                    {move || if rec.get() { "⏹ Stop, I'm done talking" } else { "🎤 Speak your answer" }}
                </button>
                <button on:click=send disabled=move || ctx.busy.get().is_some() || rec.get()>"Send"</button>
            </div>
        </div>
    }
}

/// Status of a company's background lookup, in words.
fn status_label(s: &str) -> &'static str {
    match s {
        "new" | "queued" => "Waiting",
        "running" => "Reading now",
        "enriched" => "Done",
        "retrying" => "Trying again later",
        "paused_cap" => "Paused (monthly budget reached)",
        "failed" => "Could not be read",
        _ => "",
    }
}

/// A source link; only web addresses become links.
fn source(url: &Value) -> impl IntoView {
    let u = s(url);
    u.starts_with("http")
        .then(|| view! { <a href=u target="_blank" rel="noopener noreferrer">" (source)"</a> })
}

/// Add a company by its website; the admin also sees the list, one company's facts,
/// timeline and signals, and can have it read again.
#[component]
fn Companies(set_screen: WriteSignal<Screen>) -> impl IntoView {
    let url = RwSignal::new(String::new());
    let note = RwSignal::new(None::<String>);
    let list = RwSignal::new(Value::Null);
    let detail = RwSignal::new(Value::Null);
    // Only the admin gets the list; for anyone else it stays empty.
    let load = move || {
        spawn_local(async move {
            if let Ok(v) = api("GET", "/admin/companies", None).await {
                list.set(v);
            }
        })
    };
    load();
    let fail = move |e: String| match e.as_str() {
        "unauthorized" => set_screen.set(Screen::Login),
        "revoked" => set_screen.set(Screen::Unpaired(Some(nl(&e)))),
        _ => note.set(Some(nl(&e))),
    };
    let add = move |_| {
        let u = url.get();
        spawn_local(async move {
            match api("POST", "/companies", Some(json!({ "url": u }))).await {
                Ok(v) => {
                    url.set(String::new());
                    note.set(Some(if v["new"] == true {
                        format!(
                            "Added {}. Its public pages are read in the background.",
                            s(&v["website"])
                        )
                    } else {
                        format!("{} is already in the list.", s(&v["website"]))
                    }));
                    load();
                }
                Err(e) => fail(e),
            }
        });
    };
    let open = move |id: String| {
        spawn_local(async move {
            match api("GET", &format!("/admin/companies/{id}"), None).await {
                Ok(v) => detail.set(v),
                Err(e) => fail(e),
            }
        })
    };
    let again = move |id: String| {
        spawn_local(async move {
            match api("POST", &format!("/admin/companies/{id}/enrich"), None).await {
                Ok(_) => {
                    note.set(Some("Queued: it will be read again shortly.".into()));
                    load();
                }
                Err(e) => fail(e),
            }
        })
    };
    view! {
        <h1>"Companies"</h1>
        <div class="card">
            <p>"Know a company that could fit? Paste its website and it is looked up for you."</p>
            <div class="row">
                <input type="url" placeholder="https://example.com" bind:value=url/>
                <button on:click=add>"Add"</button>
            </div>
            {move || note.get().map(|n| view! { <p class="meta">{n}</p> })}
        </div>
        {move || {
            let v = list.get();
            let cs = v["companies"].as_array()?.clone();
            let spend = format!(
                "Spent on reading company pages: €{:.2} in the last 7 days, €{:.2} in the last 30 days.",
                v["spend_7d_eur"].as_f64().unwrap_or(0.0),
                v["spend_30d_eur"].as_f64().unwrap_or(0.0)
            );
            Some(view! {
                <div class="card">
                    <p class="meta">{spend}</p>
                    {cs.into_iter().map(|c| {
                        let id = s(&c["id"]);
                        let line = format!(
                            "{} · {} · {} facts, {} signals",
                            s(&c["city"]),
                            status_label(&s(&c["status"])),
                            c["facts"],
                            c["signals"]
                        );
                        view! {
                            <div class="row">
                                <button class="link" on:click=move |_| open(id.clone())>{s(&c["name"])}</button>
                                <span class="meta">{line}</span>
                            </div>
                        }
                    }).collect_view()}
                </div>
            })
        }}
        {move || {
            let d = detail.get();
            let c = d["company"].clone();
            let id = s(&c["id"]);
            (!id.is_empty()).then(|| view! {
                <div class="card">
                    <h2>{s(&c["name"])}</h2>
                    <p><a href=s(&c["website"]) target="_blank" rel="noopener noreferrer">{s(&c["website"])}</a></p>
                    <p class="meta">{format!("{} · {}", status_label(&s(&c["status"])), s(&c["last_error"]))}</p>
                    <p>{s(&c["mission_summary"])}</p>
                    <h3>"Facts"</h3>
                    {items(&d["facts"]).into_iter().map(|f| view! {
                        <p class="fact">{format!("{}: {}", s(&f["field"]), s(&f["value"]))}{source(&f["source_url"])}</p>
                    }).collect_view()}
                    <h3>"Timeline"</h3>
                    {items(&d["events"]).into_iter().map(|e| view! {
                        <p class="fact">{format!("{} · {}", s(&e["date"]), s(&e["title"]))}{source(&e["source_url"])}</p>
                    }).collect_view()}
                    <h3>"Signals"</h3>
                    {items(&d["signals"]).into_iter().map(|g| view! {
                        <p class="fact">
                            {format!("{} · {} · strength {:.2} · {}", s(&g["dated"]), s(&g["kind"]),
                                g["strength"].as_f64().unwrap_or(0.0), s(&g["detail"]))}
                            {source(&g["source_url"])}
                        </p>
                    }).collect_view()}
                    <h3>"People"</h3>
                    {items(&d["people"]).into_iter().map(|p| view! {
                        <p class="fact">{format!("{}, {}", s(&p["name"]), s(&p["role"]))}{source(&p["source_url"])}</p>
                    }).collect_view()}
                    <button class="secondary" on:click=move |_| again(id.clone())>"Read its pages again"</button>
                </div>
            })
        }}
    }
}

/// Label of a profile field inside a section card.
fn field_label(key: &str) -> &'static str {
    match key {
        "work_style" => "How you like to work",
        "anti_prefs" => "Rather not",
        "strengths" => "Strengths",
        "role_types" => "Type of role",
        "stages" => "Type of organisation",
        "subsectors" => "Sector",
        "mission_themes" => "What you care about",
        "home_base" => "Where you live",
        "max_travel" => "Travel time",
        "hours_per_week" => "Hours per week",
        "work_mode" => "Way of working",
        "start_date" => "Available from",
        "languages" => "Languages",
        _ => "",
    }
}

fn code_nl(c: &str) -> String {
    match c {
        "permanent" => "Permanent job",
        "interim" => "Interim",
        "part_time" => "Part-time",
        "advisory" => "Advisory or board role",
        "startup" => "Start-up",
        "scaleup" => "Scale-up",
        "midsize" => "Medium-sized company",
        "corporate" => "Large company",
        "medtech" => "Medical technology",
        "digital_health" => "Digital health",
        "care_tech" => "Care technology",
        "diagnostics" => "Diagnostics",
        "other" => "Other",
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
            format!("At most {} minutes{mode}", it["minutes"])
        }
        "hours_per_week" => format!("{} to {} hours", it["min"], it["max"]),
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
            let who = if t["speaker"] == "seeker" { "You" } else { "Kompas" };
            return format!("{who}: “{}”", s(&t["text"]));
        }
    } else if let Some(d) = find(docs, "label", id.to_string()) {
        return format!("From your document “{}”", s(&d["name"]));
    }
    "This source can no longer be found.".into()
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
            view! { <div class="card done"><p>"Your profile is saved. This is you, in your own words."</p></div> }.into_any()
        } else {
            view! { <p class="lead">"Take your time reading each part. If something is not right, write in your own words how it really is. Your words always come first."</p> }.into_any()
        }}
        {(no_pitch && !approved).then(|| view! {
            <button class="secondary" on:click=move |_| ctx.go(vec![("/intake/pitch", None)], "Kompas is writing your pitch…")>
                "Write my pitch"
            </button>
        })}
        {sections
            .into_iter()
            .map(|sec| view! { <Section ctx=ctx sec=sec data=data.clone() turns=turns.clone() approved=approved/> })
            .collect_view()}
        {(!approved).then(|| view! {
            <button class="approve" on:click=move |_| ctx.go(vec![("/intake/approve", None)], "Saving…")>
                "This is me"
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
                                        }>{format!("source {}", i + 1)}</button>
                                    }
                                })
                                .collect_view()}
                        </li>
                    }
                })
                .collect_view();
            view! {
                {(!label.is_empty()).then(|| view! { <h3>{label}</h3> })}
                {rows.is_empty().then(|| view! { <p class="empty">"Nothing yet."</p> })}
                <ul class=list>{lines}</ul>
            }
        })
        .collect_view();
    let k2 = key.clone();
    view! {
        <section class="card section">
            <h2>{s(&sec["title"])}</h2>
            {(key == "pitch" && words > 0).then(|| view! { <p class="meta">{format!("{words} words")}</p> })}
            {body}
            {move || shown.get().map(|q| view! { <blockquote on:click=move |_| shown.set(None)>{q}</blockquote> })}
            {(!approved).then(move || view! {
                {move || if editing.get() {
                    let k = k2.clone();
                    view! {
                        <textarea
                            rows="3"
                            placeholder="What should it say?"
                            prop:value=move || fix.get()
                            on:input=move |ev| fix.set(event_target_value(&ev))
                        ></textarea>
                        <div class="row">
                            <button class="secondary" on:click=move |_| editing.set(false)>"Never mind"</button>
                            <button on:click=move |_| ctx.go(
                                vec![("/intake/correct", Some(json!({ "section": k.clone(), "text": fix.get() })))],
                                "Kompas is updating this part…",
                            )>"Update it"</button>
                        </div>
                    }
                    .into_any()
                } else {
                    view! { <button class="link" on:click=move |_| editing.set(true)>"This is not quite right"</button> }.into_any()
                }}
            })}
        </section>
    }
}

#[component]
fn Documents(ctx: Ctx) -> impl IntoView {
    const READING: &str = "Kompas is reading your document…";
    let open = RwSignal::new(false);
    let name = RwSignal::new(String::new());
    let body = RwSignal::new(String::new());
    let add = move |_| {
        ctx.docs_call(
            "/intake/documents".into(),
            json!({ "name": name.get(), "body": body.get() }),
            Some(READING),
        );
        name.set(String::new());
        body.set(String::new());
    };
    let upload = move |ev: leptos::ev::Event| {
        let ev: JsValue = ev.into();
        spawn_local(async move {
            match js_json(read_file(ev).await) {
                Some(file) => ctx.docs_call("/intake/documents".into(), file, Some(READING)),
                None => ctx.error.set(Some(nl("unreadable_file"))),
            }
        });
    };
    view! {
        <details class="card docs" prop:open=move || open.get() on:toggle=move |_| open.update(|o| *o = !*o)>
            <summary>"Add a document (CV, LinkedIn text)"</summary>
            <p class="meta">"Kompas reads the facts from your document and asks you about each one. Only the facts you confirm are used, and Kompas won't ask you about them again. What you say yourself always comes first."</p>
            {move || ctx.docs.get().as_array().cloned().unwrap_or_default().into_iter().map(|d| {
                let facts = d["facts"].as_array().cloned().unwrap_or_default();
                let waiting = facts.iter().filter(|f| f["status"] == "pending").count();
                let dropped = d["dropped"].as_i64().unwrap_or(0);
                // Documents from before the fact check have no facts: used whole or not at all.
                let state = match (facts.len(), s(&d["status"]).as_str()) {
                    (0, "confirmed") => "used".to_string(),
                    (0, _) => "not used".to_string(),
                    (n, _) if waiting > 0 => format!("{n} facts, {waiting} waiting for you"),
                    (n, _) => format!("{n} facts, all checked"),
                };
                view! {
                    <div class="doc">
                        <strong>{s(&d["name"])}</strong>" · "{state}
                        {(dropped > 0).then(|| view! {
                            <p class="meta">{format!("{dropped} more left out: Kompas couldn't find them word for word in the document.")}</p>
                        })}
                        {facts.into_iter().map(|f| view! { <Fact ctx=ctx f=f/> }).collect_view()}
                    </div>
                }
            }).collect_view()}
            <label>"Upload a file (PDF, Word or text)"</label>
            <input type="file" accept=".pdf,.docx,.txt" on:change=upload disabled=move || ctx.busy.get().is_some()/>
            <label>"Or paste the text here, with a name"</label>
            <input prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev))/>
            <textarea rows="5" prop:value=move || body.get() on:input=move |ev| body.set(event_target_value(&ev))></textarea>
            <button on:click=add disabled=move || ctx.busy.get().is_some()>"Add"</button>
        </details>
    }
}

/// One fact read from her document, with the quote it came from; pending ones ask her.
#[component]
fn Fact(ctx: Ctx, f: Value) -> impl IntoView {
    let path = format!("/intake/facts/{}", s(&f["id"]));
    let status = s(&f["status"]);
    let editing = RwSignal::new(false);
    let fix = RwSignal::new(s(&f["text"]));
    let decide = move |body: Value| ctx.docs_call(path.clone(), body, None);
    view! {
        <div class="fact">
            <p>{s(&f["text"])}{match status.as_str() {
                "confirmed" => " ✓",
                "rejected" => " (not used)",
                _ => "",
            }}</p>
            <blockquote>{s(&f["snippet"])}</blockquote>
            {(status == "pending").then(move || {
                let decide = decide.clone();
                move || {
                    let decide = decide.clone();
                    if editing.get() {
                        view! {
                            <textarea rows="2" prop:value=move || fix.get() on:input=move |ev| fix.set(event_target_value(&ev))></textarea>
                            <div class="row">
                                <button class="secondary" on:click=move |_| editing.set(false)>"Never mind"</button>
                                <button on:click=move |_| decide(json!({ "confirm": true, "text": fix.get() }))>"Save my version"</button>
                            </div>
                        }
                        .into_any()
                    } else {
                        let (yes, no) = (decide.clone(), decide);
                        view! {
                            <div class="row">
                                <button on:click=move |_| yes(json!({ "confirm": true }))>"Yes, that's right"</button>
                                <button class="secondary" on:click=move |_| editing.set(true)>"Correct it"</button>
                                <button class="secondary" on:click=move |_| no(json!({ "confirm": false }))>"Not right"</button>
                            </div>
                        }
                        .into_any()
                    }
                }
            })}
        </div>
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
                        ctx.go(vec![("/intake/reset", None)], "Starting again…");
                    }>"Are you sure? Delete everything"</button>
                    <button class="link" on:click=move |_| sure.set(false)>"No, keep it"</button>
                }
                .into_any()
            } else {
                view! { <button class="link" on:click=move |_| sure.set(true)>"Test account: start again"</button> }.into_any()
            }}
        </div>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
