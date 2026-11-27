//! Phase 5 screens: the companies grid, the map with her travel ring, her own ranking,
//! stories and media, and her display settings. The server sends travel times only: the
//! home address never reaches this window.

use super::{api, code_nl, items, nl, s, source, Screen};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

// MapLibre GL JS (BSD-3-Clause, vendored) on OpenFreeMap tiles; the OpenStreetMap credit
// stays visible (ODbL). Popups are built with textContent, so no company text becomes HTML.
#[wasm_bindgen(inline_js = r##"
let map = null;
const COLOURS = ["match", ["get", "ring"], "inside", "#2f6f8f", "outside", "#c0663a", "#8a949b"];
function draw(el, data) {
  if (map && map.getContainer() !== el) { map.remove(); map = null; }
  const pts = { type: "FeatureCollection", features: data.map((r) => ({
    type: "Feature", geometry: { type: "Point", coordinates: [r.lon, r.lat] },
    properties: { name: r.name, ring: r.ring, note: r.note } })) };
  const fit = () => {
    if (!data.length) return;
    const b = new maplibregl.LngLatBounds();
    data.forEach((r) => b.extend([r.lon, r.lat]));
    map.fitBounds(b, { padding: 50, maxZoom: 11, duration: 0 });
  };
  if (map) {
    const src = map.getSource("companies");
    if (src) { src.setData(pts); fit(); } else map.once("load", () => draw(el, data));
    return;
  }
  map = new maplibregl.Map({ container: el, style: "https://tiles.openfreemap.org/styles/liberty",
    center: [5.3, 52.2], zoom: 7, attributionControl: { compact: false } });
  map.addControl(new maplibregl.NavigationControl({ showCompass: false }));
  map.on("load", () => {
    map.addSource("companies", { type: "geojson", data: pts });
    map.addLayer({ id: "companies", type: "circle", source: "companies", paint: {
      "circle-radius": 7, "circle-color": COLOURS, "circle-stroke-width": 1.5, "circle-stroke-color": "#ffffff" } });
    map.on("click", "companies", (e) => {
      const p = e.features[0].properties, box = document.createElement("div");
      const b = document.createElement("strong"), m = document.createElement("div");
      b.textContent = p.name; m.textContent = p.note; box.append(b, m);
      new maplibregl.Popup({ closeButton: true }).setLngLat(e.features[0].geometry.coordinates).setDOMContent(box).addTo(map);
    });
    map.on("mouseenter", "companies", () => { map.getCanvas().style.cursor = "pointer"; });
    map.on("mouseleave", "companies", () => { map.getCanvas().style.cursor = ""; });
    fit();
  });
}
export function map_show(id, json) {
  const data = JSON.parse(json);
  let tries = 0;
  const go = () => {
    const el = document.getElementById(id);
    if (el && window.maplibregl) return draw(el, data);
    if (tries++ < 100) setTimeout(go, 50);
  };
  go();
}
export function set_display(size, contrast) {
  const d = document.documentElement.dataset;
  d.size = String(size);
  if (contrast) d.contrast = "high"; else delete d.contrast;
}
export function remembered(k) { try { return localStorage.getItem(k) || ""; } catch (_) { return ""; } }
export function remember(k, v) { try { localStorage.setItem(k, v); } catch (_) {} }
"##)]
extern "C" {
    fn map_show(id: &str, json: &str);
    fn set_display(size: u8, contrast: bool);
    fn remembered(k: &str) -> String;
    fn remember(k: &str, v: &str);
}

/// Applies her text size and contrast from a `/settings` answer.
pub fn apply_display(v: &Value) {
    set_display(
        v["font_step"].as_u64().unwrap_or(0).min(2) as u8,
        v["high_contrast"] == true,
    );
}

/// Shows the Mac notification once per new digest, the first time the app sees it unseen.
pub fn notify_once(v: &Value) {
    let at = s(&v["digest_at"]);
    if v["digest_unseen"] == true && !at.is_empty() && remembered("cc_digest_notified") != at {
        remember("cc_digest_notified", &at);
        spawn_local(async move {
            let _ = super::call("notify_digest", &super::NoArgs {}).await;
        });
    }
}

/// The usual error routing: back to login, back to pairing, or a sentence on the page.
fn failer(set_screen: WriteSignal<Screen>, note: RwSignal<Option<String>>) -> impl Fn(String) + Copy {
    move |e: String| match e.as_str() {
        "unauthorized" => set_screen.set(Screen::Login),
        "revoked" => set_screen.set(Screen::Unpaired(Some(nl(&e)))),
        _ => note.set(Some(nl(&e))),
    }
}

/// The four grid filters; an empty value means "all".
#[derive(Clone, Copy)]
struct Filt {
    size: RwSignal<String>,
    sector: RwSignal<String>,
    ring: RwSignal<String>,
    role: RwSignal<String>,
}

impl Filt {
    fn new() -> Self {
        Filt {
            size: RwSignal::new(String::new()),
            sector: RwSignal::new(String::new()),
            ring: RwSignal::new(String::new()),
            role: RwSignal::new(String::new()),
        }
    }
    fn body(self) -> Value {
        json!({"filter": {"size": self.size.get(), "sector": self.sector.get(),
                          "ring": self.ring.get(), "role": self.role.get()}})
    }
}

/// Loads the grid again whenever a filter changes (or `again` is bumped).
fn grid_data(f: Filt, again: RwSignal<u32>, fail: impl Fn(String) + Copy + 'static) -> RwSignal<Value> {
    let v = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        again.track();
        let body = f.body();
        spawn_local(async move {
            match api("POST", "/matching/grid", Some(body)).await {
                Ok(x) => v.set(x),
                Err(e) => fail(e),
            }
        });
    });
    v
}

fn pick(label: &'static str, sig: RwSignal<String>, opts: Vec<(String, String)>) -> impl IntoView {
    view! {
        <label class="filter">
            <span class="meta">{label}</span>
            <select on:change=move |ev| sig.set(event_target_value(&ev)) prop:value=move || sig.get()>
                {opts.into_iter().map(|(v, l)| view! { <option value=v>{l}</option> }).collect_view()}
            </select>
        </label>
    }
}

fn opts(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect()
}

#[component]
fn Filters(f: Filt, data: RwSignal<Value>) -> impl IntoView {
    let max = move || data.get()["max_travel_min"].as_u64().unwrap_or(90);
    view! {
        <div class="filters">
            {pick("Size", f.size, opts(&[("", "All sizes"), ("small", "Small (under 50)"),
                ("medium", "Medium (50 to 249)"), ("large", "Large (250 or more)"), ("unknown", "Size not known")]))}
            {move || {
                let mut o = vec![(String::new(), "All sectors".to_string())];
                o.extend(items(&data.get()["sectors"]).iter().map(|x| (s(x), code_nl(&s(x)))));
                pick("Sector", f.sector, o)
            }}
            {move || pick("Travel", f.ring, vec![
                (String::new(), "Any travel time".into()),
                ("inside".into(), format!("Within {} minutes", max())),
                ("outside".into(), format!("More than {} minutes", max())),
                ("unknown".into(), "Travel time not known".into()),
            ])}
            {pick("Type of role", f.role, opts(&[("", "Any type of role"), ("permanent", "Permanent job"),
                ("interim", "Interim"), ("part_time", "Part-time"), ("advisory", "Advisory or board role")]))}
        </div>
    }
}

fn travel(r: &Value) -> String {
    r["travel_min"]
        .as_u64()
        .map_or("Not known".into(), |t| format!("{t} min"))
}

fn fit(r: &Value) -> String {
    match (r["score"].as_f64(), s(&r["excluded"])) {
        (_, ex) if !ex.is_empty() => "Left out".into(),
        (Some(sc), _) => format!("{sc:.0}"),
        _ => "Not scored yet".into(),
    }
}

fn forms(r: &Value) -> String {
    let f: Vec<String> = items(&r["forms"]).iter().map(|x| code_nl(&s(x))).collect();
    if f.is_empty() {
        "No signs yet".into()
    } else {
        f.join(", ")
    }
}

fn ranked_ids(v: &Value) -> Vec<String> {
    items(&v["rows"]).iter().map(|r| s(&r["id"])).collect()
}

/// Saves her whole ranking, then reloads what depends on it.
fn save_ranking(ids: Vec<String>, done: impl Fn() + 'static, fail: impl Fn(String) + 'static) {
    spawn_local(async move {
        match api("POST", "/matching/ranking", Some(json!({ "company_ids": ids }))).await {
            Ok(_) => done(),
            Err(e) => fail(e),
        }
    });
}

/// The companies grid under "Companies": filters, her fit, travel and a ranking toggle.
#[component]
pub fn CompanyGrid(set_screen: WriteSignal<Screen>) -> impl IntoView {
    let note = RwSignal::new(None::<String>);
    let fail = failer(set_screen, note);
    let f = Filt::new();
    let again = RwSignal::new(0u32);
    let data = grid_data(f, again, fail);
    let mine = RwSignal::new(Vec::<String>::new());
    let load_mine = move || {
        spawn_local(async move {
            if let Ok(v) = api("GET", "/matching/ranking", None).await {
                mine.set(ranked_ids(&v));
            }
        })
    };
    load_mine();
    let toggle = move |id: String| {
        let mut ids = mine.get();
        match ids.iter().position(|x| *x == id) {
            Some(i) => {
                ids.remove(i);
            }
            None => ids.push(id),
        }
        mine.set(ids.clone());
        save_ranking(ids, move || again.update(|n| *n += 1), fail);
    };
    view! {
        {move || {
            let x = data.get();
            if x.is_null() {
                return view! { <p>"One moment…"</p> }.into_any();
            }
            if x["locked"] == true {
                return view! { <div class="card"><p>{s(&x["message"])}</p></div> }.into_any();
            }
            let c = &x["counts"];
            let summary = format!("Showing {} of {} companies.", c["shown"], c["total"]);
            let rows = items(&x["rows"]);
            view! {
                <div class="card wide">
                    <h2>"All companies"</h2>
                    <Filters f=f data=data/>
                    <p class="meta">{summary}</p>
                    {move || note.get().map(|n| view! { <p class="meta">{n}</p> })}
                    <div class="grid-wrap">
                        <table class="grid">
                            <thead><tr>
                                <th>"Company"</th><th>"Sector"</th><th>"City"</th><th>"Size"</th>
                                <th>"Travel"</th><th>"Type of role"</th><th>"Fit"</th><th></th>
                            </tr></thead>
                            <tbody>
                                {rows.into_iter().map(|r| {
                                    let id = s(&r["id"]);
                                    let id2 = id.clone();
                                    let size = match s(&r["size"]).as_str() {
                                        "small" => "Small", "medium" => "Medium", "large" => "Large", _ => "Not known",
                                    };
                                    view! {
                                        <tr class:ranked=r["rank"].is_u64()>
                                            <td>{s(&r["name"])}</td>
                                            <td>{code_nl(&s(&r["sector"]))}</td>
                                            <td>{s(&r["city"])}</td>
                                            <td>{size}</td>
                                            <td>{travel(&r)}</td>
                                            <td>{forms(&r)}</td>
                                            <td>{fit(&r)}</td>
                                            <td>
                                                <button class="secondary small" on:click=move |_| toggle(id.clone())>
                                                    {move || if mine.get().contains(&id2) { "In my ranking ✓" } else { "Add to my ranking" }}
                                                </button>
                                            </td>
                                        </tr>
                                    }
                                }).collect_view()}
                            </tbody>
                        </table>
                    </div>
                </div>
            }
            .into_any()
        }}
    }
}

/// The map: every company with a known place, coloured by her travel limit.
#[component]
pub fn MapPage(set_screen: WriteSignal<Screen>) -> impl IntoView {
    let note = RwSignal::new(None::<String>);
    let fail = failer(set_screen, note);
    let f = Filt::new();
    let data = grid_data(f, RwSignal::new(0), fail);
    let locked = move || data.get()["locked"] == true;
    Effect::new(move |_| {
        let x = data.get();
        if x.is_null() || x["locked"] == true {
            return;
        }
        let max = x["max_travel_min"].as_u64().unwrap_or(90);
        let pts: Vec<Value> = items(&x["rows"])
            .iter()
            .filter(|r| r["lat"].is_f64() && r["lon"].is_f64())
            .map(|r| {
                let note = match (s(&r["ring"]).as_str(), r["travel_min"].as_u64()) {
                    ("inside", Some(t)) => format!("{t} minutes away, within your {max}-minute limit"),
                    (_, Some(t)) => format!("{t} minutes away, more than your {max}-minute limit"),
                    _ => "Travel time not known yet".into(),
                };
                json!({"name": s(&r["name"]), "ring": r["ring"], "lat": r["lat"], "lon": r["lon"], "note": note})
            })
            .collect();
        map_show("cc-map", &Value::from(pts).to_string());
    });
    view! {
        <h1>"Map"</h1>
        {move || note.get().map(|n| view! { <p class="meta">{n}</p> })}
        {move || {
            let x = data.get();
            if x.is_null() {
                return view! { <p>"One moment…"</p> }.into_any();
            }
            if x["locked"] == true {
                return view! { <div class="card"><p>{s(&x["message"])}</p></div> }.into_any();
            }
            let c = &x["counts"];
            let max = x["max_travel_min"].as_u64().unwrap_or(90);
            let no_place = items(&x["rows"]).iter().filter(|r| !r["lat"].is_f64()).count();
            view! {
                <div class="card">
                    <p>{format!("Within your {max}-minute travel limit: {}. Further away: {}. Travel time not known yet: {}.",
                        c["inside"], c["outside"], c["unknown"])}</p>
                    <p class="legend">
                        <span class="key"><span class="dot inside"></span>{format!("Within {max} minutes")}</span>
                        <span class="key"><span class="dot outside"></span>{format!("More than {max} minutes")}</span>
                        <span class="key"><span class="dot unknown"></span>"Not known"</span>
                    </p>
                    {(no_place > 0).then(|| view! {
                        <p class="meta">{format!("Not on the map because no address is known: {no_place} of the companies shown.")}</p>
                    })}
                </div>
            }
            .into_any()
        }}
        <div class="card wide" style:display=move || if locked() { "none" } else { "block" }>
            <Filters f=f data=data/>
            <div id="cc-map" class="map"></div>
        </div>
    }
}

/// Her own ranking: the companies she picked, in her order.
#[component]
pub fn Ranking(set_screen: WriteSignal<Screen>) -> impl IntoView {
    let note = RwSignal::new(None::<String>);
    let fail = failer(set_screen, note);
    let v = RwSignal::new(Value::Null);
    let load = move || {
        spawn_local(async move {
            match api("GET", "/matching/ranking", None).await {
                Ok(x) => v.set(x),
                Err(e) => fail(e),
            }
        })
    };
    load();
    // Moves one company by `by` places (or removes it with `None`) and saves the whole list.
    let change = move |i: usize, by: Option<isize>| {
        let mut ids = ranked_ids(&v.get());
        match by {
            Some(d) => {
                let j = i as isize + d;
                if j < 0 || j as usize >= ids.len() {
                    return;
                }
                ids.swap(i, j as usize);
            }
            None => {
                ids.remove(i);
            }
        }
        save_ranking(ids, load, fail);
    };
    view! {
        <h1>"My ranking"</h1>
        {move || note.get().map(|n| view! { <p class="meta">{n}</p> })}
        {move || {
            let x = v.get();
            if x.is_null() {
                return view! { <p>"One moment…"</p> }.into_any();
            }
            if x["locked"] == true {
                return view! { <div class="card"><p>{s(&x["message"])}</p></div> }.into_any();
            }
            let rows = items(&x["rows"]);
            if rows.is_empty() {
                return view! { <div class="card"><p>
                    "Your ranking is empty. Under Companies, choose \"Add to my ranking\" for the ones you like most; \
                     they appear here, and you can put them in your own order."
                </p></div> }
                .into_any();
            }
            let n = rows.len();
            view! {
                <div class="card">
                    <p class="meta">"Your own order. It is saved straight away and comes first in the company list."</p>
                    {rows.into_iter().enumerate().map(|(i, r)| view! {
                        <div class="row rank">
                            <span class="pos">{i + 1}</span>
                            <span class="grow">
                                <strong>{s(&r["name"])}</strong>
                                <span class="meta">{format!(" · {} · {} · fit {}", code_nl(&s(&r["sector"])), travel(&r), fit(&r))}</span>
                            </span>
                            <button class="secondary small" disabled=i == 0 on:click=move |_| change(i, Some(-1))
                                aria-label="Move up">"Up"</button>
                            <button class="secondary small" disabled=i + 1 == n on:click=move |_| change(i, Some(1))
                                aria-label="Move down">"Down"</button>
                            <button class="link" on:click=move |_| change(i, None)>"Remove"</button>
                        </div>
                    }).collect_view()}
                </div>
            }
            .into_any()
        }}
    }
}

fn kind_label(k: &str) -> &'static str {
    match k {
        "funding" => "Funding",
        "regulatory" => "Approval",
        "contract" => "Contract",
        "leadership" => "New leadership",
        "new_country" => "New country",
        "product" => "Product",
        "partnership" => "Partnership",
        "award" => "Award",
        _ => "News",
    }
}

/// News and media the company reading found, newest first, each with its source.
#[component]
pub fn Stories(set_screen: WriteSignal<Screen>) -> impl IntoView {
    let note = RwSignal::new(None::<String>);
    let fail = failer(set_screen, note);
    let v = RwSignal::new(Value::Null);
    spawn_local(async move {
        match api("GET", "/matching/stories", None).await {
            Ok(x) => v.set(x),
            Err(e) => fail(e),
        }
    });
    view! {
        <h1>"Stories & media"</h1>
        {move || note.get().map(|n| view! { <p class="meta">{n}</p> })}
        {move || {
            let x = v.get();
            if x.is_null() {
                return view! { <p>"One moment…"</p> }.into_any();
            }
            if x["locked"] == true {
                return view! { <div class="card"><p>{s(&x["message"])}</p></div> }.into_any();
            }
            let all = items(&x["items"]);
            if all.is_empty() {
                return view! { <div class="card"><p>
                    "No news yet. As soon as the companies' pages have been read, their news appears here."
                </p></div> }
                .into_any();
            }
            view! {
                <div class="card">
                    <p class="meta">"What the companies have been up to lately, from their own sites and the news."</p>
                    {all.into_iter().map(|it| view! {
                        <div class="fact">
                            <p class="meta">{format!("{} · {} · {}", s(&it["date"]), s(&it["name"]), kind_label(&s(&it["kind"])))}</p>
                            <p><strong>{s(&it["title"])}</strong>{source(&it["source_url"])}</p>
                            {(!s(&it["snippet"]).is_empty()).then(|| view! { <p>{s(&it["snippet"])}</p> })}
                        </div>
                    }).collect_view()}
                </div>
            }
            .into_any()
        }}
    }
}

/// Text size in three steps and high contrast, saved with her account.
#[component]
pub fn Settings(set_screen: WriteSignal<Screen>) -> impl IntoView {
    let note = RwSignal::new(None::<String>);
    let fail = failer(set_screen, note);
    let v = RwSignal::new(Value::Null);
    let save = move |body: Value| {
        spawn_local(async move {
            match api("POST", "/settings", Some(body)).await {
                Ok(x) => {
                    apply_display(&x);
                    v.set(x);
                    note.set(Some("Saved.".into()));
                }
                Err(e) => fail(e),
            }
        })
    };
    spawn_local(async move {
        match api("GET", "/settings", None).await {
            Ok(x) => v.set(x),
            Err(e) => fail(e),
        }
    });
    let step = move || v.get()["font_step"].as_u64().unwrap_or(0);
    view! {
        <h1>"Settings"</h1>
        <div class="card">
            <h2>"Text size"</h2>
            <p class="meta">"Makes all text in Career Compass bigger or smaller."</p>
            <div class="row start">
                {[(0u64, "Standard"), (1, "Larger"), (2, "Largest")].into_iter().map(|(n, l)| view! {
                    <button class:secondary=move || step() != n aria-pressed=move || (step() == n).to_string()
                        on:click=move |_| save(json!({"font_step": n}))>{l}</button>
                }).collect_view()}
            </div>
            <h2>"Contrast"</h2>
            <label class="check">
                <input type="checkbox" prop:checked=move || v.get()["high_contrast"] == true
                    on:change=move |ev| save(json!({"high_contrast": event_target_checked(&ev)}))/>
                " High contrast: darker text and stronger lines"
            </label>
            {move || note.get().map(|n| view! { <p class="meta">{n}</p> })}
        </div>
    }
}
