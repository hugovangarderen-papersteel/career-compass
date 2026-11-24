// Web build only: stands in for the Tauri commands (src-tauri/src/lib.rs) so the same UI runs in a
// phone browser, served by cc-api at /app/. Same server, same pairing gate, same allowlist.
// The device key is a WebCrypto Ed25519 key in localStorage (weaker than the Mac Keychain; test
// track only); the session token lives in sessionStorage and dies with the tab.
(() => {
  const POLL_MS = 3000;
  const DEVICE_NAME = "Telefoon (web)";
  const ls = window.localStorage, ss = window.sessionStorage;
  const b64 = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf)));
  const unb64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

  async function send(method, path, body, headers = {}, ms = 20000) {
    const ctl = new AbortController();
    const t = setTimeout(() => ctl.abort(), ms);
    try {
      const res = await fetch(path, {
        method,
        headers: body === undefined ? headers : { ...headers, "content-type": "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: ctl.signal,
      });
      let json = null;
      try { json = await res.json(); } catch (_) { /* empty body */ }
      return [res.status, json || {}];
    } catch (_) {
      throw "offline";
    } finally {
      clearTimeout(t);
    }
  }
  const serverError = (b) => (b && typeof b.error === "string" ? b.error : "server");
  function revokedIf401(b) {
    if (b && b.error === "device_rejected") { ls.removeItem("cc-device-token"); return "revoked"; }
    return serverError(b);
  }
  function deviceToken() {
    const t = ls.getItem("cc-device-token");
    if (!t) throw "unpaired";
    return t;
  }

  async function key() {
    if (!ls.getItem("cc-device-key")) {
      const k = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
      ls.setItem("cc-device-key", b64(await crypto.subtle.exportKey("pkcs8", k.privateKey)));
      ls.setItem("cc-device-pub", b64(await crypto.subtle.exportKey("raw", k.publicKey)));
    }
    const priv = await crypto.subtle.importKey("pkcs8", unb64(ls.getItem("cc-device-key")), { name: "Ed25519" }, false, ["sign"]);
    return { priv, pub: ls.getItem("cc-device-pub") };
  }

  async function claimOnce(id) {
    const { priv } = await key();
    const sig = b64(await crypto.subtle.sign({ name: "Ed25519" }, priv, new TextEncoder().encode(`cc-pair:${id}`)));
    const [code, body] = await send("POST", `/pair/${id}/claim`, { signature: sig });
    if (code === 202) return { status: "pending", code: body.code };
    if (code === 200) {
      if (typeof body.device_token !== "string") throw "server";
      ls.setItem("cc-device-token", body.device_token);
      ls.removeItem("cc-pairing-id");
      return { status: "approved" };
    }
    if ([404, 409, 410].includes(code)) {
      ls.removeItem("cc-pairing-id");
      return { status: code === 410 ? serverError(body) : "unpaired" };
    }
    throw serverError(body);
  }

  async function startSession(path, a) {
    const [code, body] = await send("POST", path, { email: a.email, password: a.password }, { "x-device-token": deviceToken() });
    if (code !== 200) throw revokedIf401(body);
    ss.setItem("cc-session", body.session);
    return { ok: true };
  }

  // Same rule as `allowed` in src-tauri: her intake and who she is, nothing else.
  const allowed = (m, p) =>
    (m === "GET" || m === "POST") && !p.includes("..") && !p.includes("?") &&
    (p === "/me" || p === "/intake" || p.startsWith("/intake/"));

  async function authed(method, path, body, ms) {
    const jwt = ss.getItem("cc-session");
    if (!jwt) throw "unauthorized";
    const [code, res] = await send(method, path, body, { "x-device-token": deviceToken(), authorization: `Bearer ${jwt}` }, ms);
    if (code < 200 || code > 299) throw revokedIf401(res);
    return res;
  }

  const commands = {
    async pair_check() {
      if (ls.getItem("cc-device-token")) return { status: "paired" };
      const id = ls.getItem("cc-pairing-id");
      return id ? claimOnce(id) : { status: "unpaired" };
    },
    async pair_start() {
      if (!window.isSecureContext || !crypto.subtle) throw "insecure";
      const { pub } = await key();
      const [code, body] = await send("POST", "/pair/request", { device_name: DEVICE_NAME, public_key: pub });
      if (code !== 201) throw serverError(body);
      ls.setItem("cc-pairing-id", body.id);
      return { code: body.code, expires_at: body.expires_at };
    },
    async pair_wait() {
      for (;;) {
        const id = ls.getItem("cc-pairing-id");
        if (!id) return { status: ls.getItem("cc-device-token") ? "approved" : "unpaired" };
        let v;
        try { v = await claimOnce(id); } catch (e) { if (e === "offline") v = { status: "pending" }; else throw e; }
        if (v.status !== "pending") return v;
        await sleep(POLL_MS);
      }
    },
    set_password: (a) => startSession("/auth/set-password", a),
    login: (a) => startSession("/auth/login", a),
    me: () => authed("GET", "/me"),
    async api(a) {
      if (!allowed(a.method, a.path)) throw "not_allowed";
      return authed(a.method, a.path, a.body === null ? undefined : a.body, 120000);
    },
  };

  window.__TAURI__ = {
    core: {
      async invoke(cmd, args) {
        const f = commands[cmd];
        if (!f) throw "not_allowed";
        try { return await f(args || {}); } catch (e) { throw typeof e === "string" ? e : "offline"; }
      },
    },
  };
})();
