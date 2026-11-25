-- Phase 2: the company base. Every fact, event, signal and person carries the URL it came
-- from. Location is plain lat/lon (no PostGIS); nothing here knows where the seeker lives.
CREATE TABLE companies (
    id               UUID PRIMARY KEY,
    name             TEXT NOT NULL,
    website          TEXT NOT NULL UNIQUE,
    city             TEXT,
    subsector        TEXT,
    stage            TEXT,
    kvk_number       TEXT,
    mission_summary  TEXT,
    lat              DOUBLE PRECISION,
    lon              DOUBLE PRECISION,
    geo_source_url   TEXT,
    -- sha256 over the page hashes: unchanged means nothing to re-read or re-score.
    content_hash     TEXT,
    status           TEXT NOT NULL DEFAULT 'new',
    last_error       TEXT,
    -- Facts the snippet guard refused on the last extraction.
    dropped          INTEGER NOT NULL DEFAULT 0,
    last_fetched_at  TIMESTAMPTZ,
    last_enriched_at TIMESTAMPTZ,
    added_by         UUID REFERENCES users(id),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- origin 'seed' = the researched seed list, 'site' = extracted from the company's own pages.
CREATE TABLE company_facts (
    id         UUID PRIMARY KEY,
    company_id UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    field      TEXT NOT NULL,
    value      TEXT NOT NULL,
    source_url TEXT NOT NULL CHECK (source_url <> ''),
    snippet    TEXT NOT NULL,
    origin     TEXT NOT NULL DEFAULT 'site',
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX company_facts_company ON company_facts (company_id);
-- Headcount is kept as history (growth signal); one row per stated value.
CREATE UNIQUE INDEX company_headcount ON company_facts (company_id, value) WHERE field = 'headcount';

-- The journey timeline. A date is YYYY, YYYY-MM or YYYY-MM-DD, as precise as the page says.
CREATE TABLE company_events (
    id         UUID PRIMARY KEY,
    company_id UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    date       TEXT NOT NULL CHECK (date ~ '^\d{4}(-\d{2}(-\d{2})?)?$'),
    kind       TEXT NOT NULL,
    title      TEXT NOT NULL,
    source_url TEXT NOT NULL CHECK (source_url <> ''),
    snippet    TEXT NOT NULL,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (company_id, kind, date)
);

-- Why-now signals, derived by code. Strength is computed when read (recency decay in cc-core).
CREATE TABLE signals (
    id          UUID PRIMARY KEY,
    company_id  UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    dated       TEXT NOT NULL,
    detail      TEXT NOT NULL,
    source_url  TEXT NOT NULL CHECK (source_url <> ''),
    event_id    UUID REFERENCES company_events(id) ON DELETE CASCADE,
    detected_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (company_id, kind, dated)
);

-- Business names and roles from the company's own pages only.
CREATE TABLE people (
    id                UUID PRIMARY KEY,
    company_id        UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    name              TEXT NOT NULL,
    role              TEXT NOT NULL,
    source_url        TEXT NOT NULL CHECK (source_url <> ''),
    snippet           TEXT NOT NULL,
    suggested_contact BOOLEAN NOT NULL DEFAULT false
);

-- The text of each fetched page, so an unchanged page costs nothing the next time.
CREATE TABLE page_snapshots (
    company_id UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    url        TEXT NOT NULL,
    sha256     TEXT NOT NULL,
    text       TEXT NOT NULL,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (company_id, url)
);
