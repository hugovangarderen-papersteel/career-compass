-- Phase 1: the intake conversation with Kompas (spec §3, §4, §10).

-- Test accounts (Hugo's dry runs) may reset their intake; real accounts never can.
ALTER TABLE users ADD COLUMN is_test BOOLEAN NOT NULL DEFAULT false;

-- Prompt-cache tokens, so the meter shows what caching saved.
ALTER TABLE llm_calls ADD COLUMN cache_read INTEGER NOT NULL DEFAULT 0;
ALTER TABLE llm_calls ADD COLUMN cache_write INTEGER NOT NULL DEFAULT 0;

-- One intake per user at a time; saved after every answer so she can stop and resume.
CREATE TABLE intake_sessions (
    id          UUID PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    phase       SMALLINT NOT NULL DEFAULT 1 CHECK (phase BETWEEN 1 AND 8),
    phases_done SMALLINT[] NOT NULL DEFAULT '{}',
    status      TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'playback', 'approved')),
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at    TIMESTAMPTZ
);
CREATE INDEX intake_sessions_user ON intake_sessions (user_id, started_at DESC);

-- Every turn of the conversation; profile fields point here as T<seq>.
CREATE TABLE transcript_turns (
    id         UUID PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES intake_sessions (id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    speaker    TEXT NOT NULL CHECK (speaker IN ('kompas', 'seeker')),
    phase      SMALLINT NOT NULL,
    text       TEXT NOT NULL,
    audio_ref  TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (session_id, seq)
);

-- Profile versions; approving one locks it (spec §4).
CREATE TABLE profiles (
    id          UUID PRIMARY KEY,
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    session_id  UUID REFERENCES intake_sessions (id) ON DELETE SET NULL,
    version     INTEGER NOT NULL,
    status      TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'approved')),
    data        JSONB NOT NULL,
    approved_at TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, version)
);

-- Documents she pasted or uploaded; only confirmed ones feed Kompas (her words win).
CREATE TABLE documents (
    id         UUID PRIMARY KEY,
    user_id    UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    name       TEXT NOT NULL,
    body       TEXT NOT NULL,
    status     TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'confirmed', 'rejected')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, seq)
);
