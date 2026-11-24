# Career Compass — rules for any coding agent in this repo

This is a private app for one person. It is separate from every other project on this machine.

## Separation (hard rules)
- Work only inside this repository. Never read, write or reference PaperSteel paths, Trello,
  Gmail, Drive, or any STARK memory, database or session file.
- Never commit personal data: no names, email addresses, CV text, transcripts, profile facts,
  screenshots of her data, keys or tokens. Tests use made-up data (`seeker@example.org`).
- Secrets live only in the server's `.env` (git-ignored). The model API key never goes into the
  desktop app or the repo.
- The `/stark/*` endpoints return only pairing, device and spend fields. Adding any profile,
  company, conversation or document field to them is forbidden.
- Postgres is never published outside the Docker network.

## Building
- Edit files in place; never `_v2`/`_new` copies. Git is the history.
- Laziest solution that works: an existing dependency or the standard library before new code.
- Before adding a dependency, check its licence. Permissive (MIT, BSD, Apache, ISC, zlib): fine.
  MPL/LGPL: only unmodified as a library. GPL/AGPL/SSPL/EUPL or no licence: do not add; ask the
  owner. `cargo deny check licenses` must pass.
- `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must be green
  before a push. Code is done only when GitHub CI is green for the pushed commit.
- UI copy is Dutch, plain and warm. The word "STARK" never appears in the app.
