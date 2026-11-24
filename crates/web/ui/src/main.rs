//! Career Compass window: pairing, login, and the Phase 0 menu (pages are placeholders).
//! All network and keychain work happens in the Tauri side; this only calls its commands.

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::Serialize;
use serde_json::Value;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["window", "__TAURI__", "core"])]
    async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;
}

async fn call<A: Serialize>(cmd: &str, args: &A) -> Result<Value, String> {
    let a = serde_wasm_bindgen::to_value(args).map_err(|e| e.to_string())?;
    match invoke(cmd, a).await {
        Ok(v) => serde_wasm_bindgen::from_value(v).map_err(|e| e.to_string()),
        Err(e) => Err(e.as_string().unwrap_or_else(|| "onbekende fout".into())),
    }
}

#[derive(Serialize)]
struct NoArgs {}

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
    "Praat met Career Compass",
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
        "revoked" => "Deze Mac is ontkoppeld. Koppel hem opnieuw.".into(),
        "too_many_pending" => "Er wachten al te veel koppelverzoeken. Probeer het later nog eens.".into(),
        "offline" => "Geen verbinding met Career Compass. Staat het internet aan?".into(),
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
        Screen::Home => view! { <Home/> }.into_any(),
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
            <p>"Deze Mac moet eerst één keer gekoppeld worden. Hugo keurt dat goed op zijn telefoon."</p>
            <button on:click=start disabled=busy>"Koppel deze Mac"</button>
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
fn Home() -> impl IntoView {
    let (page, set_page) = signal(0usize);
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
                <h1>{move || MENU[page.get()]}</h1>
                <p>"Dit onderdeel komt in een volgende versie. De app werkt zichzelf bij zodra het klaar is."</p>
            </main>
        </div>
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
