//! Native side of the app: the device key and token in the Keychain, and every call to the server.
//! Errors are returned as the server's short codes (`unauthorized`, `expired`...) or `offline`;
//! the window turns them into Dutch sentences.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::Duration;

/// Server address, fixed at build time (`CC_API_URL`); the tunnel hostname is set in the release build.
const API: &str = match option_env!("CC_API_URL") {
    Some(u) => u,
    None => "http://localhost:8080",
};
const SERVICE: &str = "nl.careercompass.app";
const POLL: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Session(Mutex<Option<String>>);

// ---------- keychain ----------

fn entry(name: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, name).map_err(|e| format!("keychain: {e}"))
}

fn get(name: &str) -> Result<Option<String>, String> {
    match entry(name)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("keychain: {e}")),
    }
}

fn put(name: &str, value: &str) -> Result<(), String> {
    entry(name)?.set_password(value).map_err(|e| format!("keychain: {e}"))
}

fn forget(name: &str) {
    if let Ok(e) = entry(name) {
        let _ = e.delete_credential();
    }
}

/// This Mac's ed25519 key; made once, the private half never leaves the Keychain.
fn signing_key() -> Result<SigningKey, String> {
    if let Some(s) = get("device-key")? {
        let b: [u8; 32] = STANDARD
            .decode(s)
            .ok()
            .and_then(|v| v.try_into().ok())
            .ok_or("keychain: bad device key")?;
        return Ok(SigningKey::from_bytes(&b));
    }
    let mut b = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
    put("device-key", &STANDARD.encode(b))?;
    Ok(SigningKey::from_bytes(&b))
}

// ---------- http ----------

async fn send(req: reqwest::RequestBuilder) -> Result<(u16, Value), String> {
    send_within(req, Duration::from_secs(20)).await
}

async fn send_within(req: reqwest::RequestBuilder, limit: Duration) -> Result<(u16, Value), String> {
    let res = req.timeout(limit).send().await.map_err(|_| "offline".to_string())?;
    let code = res.status().as_u16();
    let body = res.json::<Value>().await.unwrap_or(Value::Null);
    Ok((code, body))
}

fn server_error(body: &Value) -> String {
    body["error"].as_str().unwrap_or("server").to_string()
}

fn device_token() -> Result<String, String> {
    get("device-token")?.ok_or_else(|| "unpaired".to_string())
}

/// `device_rejected` means Hugo revoked this Mac: drop the token so it pairs again.
fn revoked_if_401(_code: u16, body: &Value) -> String {
    if body["error"] == "device_rejected" {
        forget("device-token");
        return "revoked".into();
    }
    server_error(body)
}

// ---------- commands ----------

/// One look at the pairing state: `paired`, `unpaired`, or `pending` with its code.
#[tauri::command]
async fn pair_check() -> Result<Value, String> {
    if get("device-token")?.is_some() {
        return Ok(json!({ "status": "paired" }));
    }
    let Some(id) = get("pairing-id")? else {
        return Ok(json!({ "status": "unpaired" }));
    };
    claim_once(&id).await
}

async fn claim_once(id: &str) -> Result<Value, String> {
    let sig = STANDARD.encode(signing_key()?.sign(format!("cc-pair:{id}").as_bytes()).to_bytes());
    let (code, body) = send(
        reqwest::Client::new()
            .post(format!("{API}/pair/{id}/claim"))
            .json(&json!({ "signature": sig })),
    )
    .await?;
    match code {
        202 => Ok(json!({ "status": "pending", "code": body["code"] })),
        200 => {
            let token = body["device_token"].as_str().ok_or("server")?;
            put("device-token", token)?;
            forget("pairing-id");
            Ok(json!({ "status": "approved" }))
        }
        404 | 409 | 410 => {
            forget("pairing-id");
            Ok(json!({ "status": if code == 410 { server_error(&body) } else { "unpaired".into() } }))
        }
        _ => Err(server_error(&body)),
    }
}

#[tauri::command]
async fn pair_start(device_name: String) -> Result<Value, String> {
    let pk = STANDARD.encode(signing_key()?.verifying_key().to_bytes());
    let req = reqwest::Client::new()
        .post(format!("{API}/pair/request"))
        .json(&json!({ "device_name": device_name, "public_key": pk }));
    let (code, body) = send(req).await?;
    if code != 201 {
        return Err(server_error(&body));
    }
    put("pairing-id", body["id"].as_str().ok_or("server")?)?;
    Ok(json!({ "code": body["code"], "expires_at": body["expires_at"] }))
}

/// Waits until Hugo decides or the 15 minutes run out; returns the final status.
#[tauri::command]
async fn pair_wait() -> Result<Value, String> {
    loop {
        let Some(id) = get("pairing-id")? else {
            return Ok(json!({ "status": if get("device-token")?.is_some() { "approved" } else { "unpaired" } }));
        };
        let v = match claim_once(&id).await {
            Err(e) if e == "offline" => json!({ "status": "pending" }),
            other => other?,
        };
        if v["status"] != "pending" {
            return Ok(v);
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn start_session(path: &str, email: String, password: String, session: &Session) -> Result<Value, String> {
    let req = reqwest::Client::new()
        .post(format!("{API}{path}"))
        .header("x-device-token", device_token()?)
        .json(&json!({ "email": email, "password": password }));
    let (code, body) = send(req).await?;
    if code != 200 {
        return Err(revoked_if_401(code, &body));
    }
    *session.0.lock().map_err(|_| "state")? = body["session"].as_str().map(String::from);
    Ok(json!({ "ok": true }))
}

#[tauri::command]
async fn set_password(email: String, password: String, session: tauri::State<'_, Session>) -> Result<Value, String> {
    start_session("/auth/set-password", email, password, &session).await
}

#[tauri::command]
async fn login(email: String, password: String, session: tauri::State<'_, Session>) -> Result<Value, String> {
    start_session("/auth/login", email, password, &session).await
}

/// Does this account still need its first password? Asked on this approved device only, so
/// the login screen can offer "Choose a password" before she types a wrong one.
#[tauri::command]
async fn account_check(email: String) -> Result<Value, String> {
    let req = reqwest::Client::new()
        .post(format!("{API}/auth/check"))
        .header("x-device-token", device_token()?)
        .json(&json!({ "email": email }));
    let (code, body) = send(req).await?;
    if code != 200 {
        return Err(revoked_if_401(code, &body));
    }
    Ok(body)
}

/// A macOS notification that this week's companies are ready. The text is fixed here, so the
/// window cannot put anything else in a notification.
#[tauri::command]
fn notify_digest(app: tauri::AppHandle) -> Result<Value, String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title("Your week is ready")
        .body("New companies to look at are waiting in Career Compass.")
        .show()
        .map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true }))
}

#[tauri::command]
async fn me(session: tauri::State<'_, Session>) -> Result<Value, String> {
    let jwt = session.0.lock().map_err(|_| "state")?.clone().ok_or("unauthorized")?;
    let req = reqwest::Client::new()
        .get(format!("{API}/me"))
        .header("x-device-token", device_token()?)
        .bearer_auth(jwt);
    let (code, body) = send(req).await?;
    if code != 200 {
        return Err(revoked_if_401(code, &body));
    }
    Ok(body)
}

/// The only paths the window may reach through `api`: her intake, who she is, the
/// companies, her matches, outreach and pipeline (the server keeps the admin views to
/// the admin role), and her display settings.
fn allowed(method: &str, path: &str) -> bool {
    const EXACT: [&str; 8] = [
        "/me",
        "/settings",
        "/intake",
        "/companies",
        "/admin/companies",
        "/matching",
        "/pipeline",
        "/contacts",
    ];
    const PREFIX: [&str; 7] = [
        "/intake/",
        "/admin/companies/",
        "/matching/",
        "/outreach/",
        "/pipeline/",
        "/suggestions/",
        "/settings/",
    ];
    matches!(method, "GET" | "POST")
        && !path.contains("..")
        && !path.contains('?')
        && (EXACT.contains(&path) || PREFIX.iter().any(|p| path.starts_with(p)))
}

/// One call to her own data on the server. Making her profile can take a minute, hence 120 s.
#[tauri::command]
async fn api(
    method: String,
    path: String,
    body: Option<Value>,
    session: tauri::State<'_, Session>,
) -> Result<Value, String> {
    if !allowed(&method, &path) {
        return Err("not_allowed".into());
    }
    let jwt = session.0.lock().map_err(|_| "state")?.clone().ok_or("unauthorized")?;
    let c = reqwest::Client::new();
    let mut req = if method == "GET" {
        c.get(format!("{API}{path}"))
    } else {
        c.post(format!("{API}{path}"))
    };
    req = req.header("x-device-token", device_token()?).bearer_auth(jwt);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let (code, body) = send_within(req, Duration::from_secs(120)).await?;
    if !(200..300).contains(&code) {
        return Err(revoked_if_401(code, &body));
    }
    Ok(body)
}

/// Quiet update at launch: a newer PUBLISHED release is downloaded and installed in the
/// background and takes effect the next time she opens the app. No prompt, no nudge.
/// Signed with the updater key; an unsigned or tampered bundle is refused by the plugin.
async fn quiet_update(app: tauri::AppHandle) {
    use tauri_plugin_updater::UpdaterExt;
    let Ok(updater) = app.updater() else { return };
    if let Ok(Some(update)) = updater.check().await {
        let _ = update.download_and_install(|_, _| {}, || {}).await;
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            tauri::async_runtime::spawn(quiet_update(app.handle().clone()));
            Ok(())
        })
        .manage(Session::default())
        .invoke_handler(tauri::generate_handler![
            pair_check,
            pair_start,
            pair_wait,
            set_password,
            login,
            me,
            account_check,
            notify_digest,
            api
        ])
        .run(tauri::generate_context!())
        .expect("Career Compass could not start");
}

#[cfg(test)]
mod tests {
    #[test]
    fn api_reaches_only_her_intake() {
        assert!(super::allowed("POST", "/intake/message"));
        assert!(super::allowed("GET", "/me"));
        assert!(!super::allowed("GET", "/stark/status"));
        assert!(!super::allowed("POST", "/intake/../stark/status"));
        assert!(!super::allowed("DELETE", "/intake"));
        assert!(!super::allowed("GET", "/intakeX"));
        assert!(super::allowed("POST", "/companies"));
        assert!(super::allowed("POST", "/admin/companies/abc/enrich"));
        assert!(!super::allowed("GET", "/admin/users"));
        // Phase 3 + 4 screens.
        assert!(super::allowed("GET", "/matching"));
        assert!(super::allowed("GET", "/matching/companies/abc"));
        assert!(super::allowed("POST", "/matching/feedback"));
        assert!(super::allowed("GET", "/outreach/companies/abc/facts"));
        assert!(super::allowed("POST", "/outreach/drafts"));
        assert!(super::allowed("POST", "/outreach/drafts/abc/sent"));
        assert!(super::allowed("GET", "/pipeline"));
        assert!(super::allowed("POST", "/pipeline/abc/stage"));
        assert!(super::allowed("POST", "/suggestions/abc"));
        assert!(super::allowed("GET", "/contacts"));
        assert!(!super::allowed("GET", "/outreach"));
        assert!(!super::allowed("GET", "/pipelineX"));
        assert!(!super::allowed("GET", "/pipeline/../stark/status"));
        assert!(!super::allowed("GET", "/matching?all=1"));
        assert!(!super::allowed("DELETE", "/outreach/drafts/abc"));
        // Phase 5 screens.
        assert!(super::allowed("POST", "/matching/grid"));
        assert!(super::allowed("POST", "/matching/ranking"));
        assert!(super::allowed("GET", "/matching/stories"));
        assert!(super::allowed("GET", "/settings"));
        assert!(super::allowed("POST", "/settings/digest-seen"));
        assert!(!super::allowed("GET", "/settingsX"));
        assert!(!super::allowed("POST", "/auth/check"));
    }
}
