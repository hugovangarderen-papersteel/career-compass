-- Phase 0 tables (spec §10). The other tables arrive with their phases.

CREATE TABLE users (
    id            UUID PRIMARY KEY,
    email         TEXT NOT NULL UNIQUE,
    role          TEXT NOT NULL CHECK (role IN ('seeker', 'admin')),
    password_hash TEXT,                      -- NULL until she sets it on her paired device
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One row per device, from pairing request to revocation (spec §2b).
CREATE TABLE devices (
    id           UUID PRIMARY KEY,
    label        TEXT NOT NULL,
    public_key   BYTEA NOT NULL,             -- ed25519, 32 bytes; private key stays in her keychain
    code         TEXT NOT NULL,              -- pairing code shown on her screen
    status       TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'denied', 'expired', 'revoked')),
    expires_at   TIMESTAMPTZ NOT NULL,       -- pairing window, 15 minutes
    token_hash   BYTEA UNIQUE,               -- sha256 of the device token; the token itself is never stored
    claimed      BOOLEAN NOT NULL DEFAULT false,
    decided_at   TIMESTAMPTZ,
    last_seen    TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX devices_status ON devices (status);

-- Every model call with its cost; the pill sums this per month (spec §10b).
CREATE TABLE llm_calls (
    id         UUID PRIMARY KEY,
    purpose    TEXT NOT NULL,
    model      TEXT NOT NULL,
    tokens_in  INTEGER NOT NULL,
    tokens_out INTEGER NOT NULL,
    cost_eur   DOUBLE PRECISION NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX llm_calls_created ON llm_calls (created_at);

-- Postgres job queue (SELECT ... FOR UPDATE SKIP LOCKED); used from Phase 1.
CREATE TABLE jobs (
    id         UUID PRIMARY KEY,
    kind       TEXT NOT NULL,
    payload    JSONB NOT NULL DEFAULT '{}'::jsonb,
    run_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts   INTEGER NOT NULL DEFAULT 0,
    status     TEXT NOT NULL DEFAULT 'queued',
    last_error TEXT
);
CREATE INDEX jobs_due ON jobs (status, run_at);

-- Single-row settings; the cap lives here so Hugo can raise it without a release.
CREATE TABLE settings (
    id      BOOLEAN PRIMARY KEY DEFAULT true CHECK (id),
    cap_eur DOUBLE PRECISION NOT NULL DEFAULT 30
);
INSERT INTO settings (id) VALUES (true);
