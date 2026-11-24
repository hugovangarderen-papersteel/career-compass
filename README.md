# Career Compass

A private, single-user macOS app that helps one person run a calm, well-organised job search.
Rust end to end: a Tauri 2 desktop shell with a Leptos window, an axum api, and Postgres.

This repository holds code only. No personal data, transcripts, keys or account details are ever
committed. Configuration lives in the server's `.env`, which is git-ignored.

## Layout

| Path | What |
|---|---|
| `crates/core` | Shared types: pairing codes, usage pill, status payload |
| `crates/db` | Postgres queries and migrations (`crates/db/migrations`) |
| `crates/api` | `cc-api` server and `cc-admin` CLI |
| `crates/worker`, `enrich`, `llm`, `embed` | Empty in Phase 0; filled in later phases |
| `crates/web/ui` | Leptos window (Dutch), built to wasm by trunk |
| `crates/web/src-tauri` | Tauri shell: Keychain, pairing, login |
| `deploy/` | Docker Compose for the server host (draft) |

## How a Mac gets in

1. The app makes an ed25519 key pair; the private key stays in the macOS Keychain.
2. It asks the server to pair and shows a code such as `KDF-427`.
3. The owner sees the same code on his phone and approves or denies. The request expires after 15 minutes.
4. The app proves it holds the key (a signature) and receives a device token once. The server keeps only its SHA-256.
5. She sets her password on that device, then logs in. Every request needs the device token and a 12-hour session.
6. Revoking the device ends access immediately.

## Develop

```sh
# server: needs a Postgres 16 to test against
export DATABASE_URL=postgres://postgres:<pw>@localhost:5432/postgres
cargo test
cargo clippy --all-targets -- -D warnings

# desktop (macOS, or Windows for a compile check)
rustup target add wasm32-unknown-unknown
cargo install trunk tauri-cli --locked
cd crates/web/src-tauri && cargo tauri dev
```

Server environment: `DATABASE_URL`, `CC_JWT_SECRET` and `CC_ADMIN_TOKEN` (each at least 32 characters),
optional `CC_BIND` (default `0.0.0.0:8080`). The desktop build reads `CC_API_URL` at compile time.

## Releases

A `v*` tag builds a universal `.dmg` on GitHub's macOS runner and attaches it to a draft release.
The app is ad-hoc signed (no Apple Developer account), so the first open needs
System Settings → Privacy & Security → Open Anyway. The release job records what Gatekeeper reports.

## Licences

The code is MIT (see `LICENSE`). `deny.toml` keeps every Rust dependency permissive (MPL-2.0 only as an
unmodified library). Two GPL components were approved by the owner on 2026-11-24; both run as separate
programs and are never linked into or copied into this code:

| Component | Licence | How it is used | Phase |
|---|---|---|---|
| PostGIS | GPL-2.0-or-later | Postgres extension inside the database container | 2 |
| Piper (piper1-gpl) | GPL-3.0 | Separate text-to-speech process on the server | 3 |
| pgvector | PostgreSQL License | Postgres extension | 1 |
