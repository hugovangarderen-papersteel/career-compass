-- Phase 4: outreach and pipeline. Her contacts (business name and role only, with where she
-- got them), the drafts with her ranking, the pipeline with every stage change dated, and the
-- debrief suggestions she confirms one by one. Nothing here is sent anywhere by the app.

CREATE TABLE contacts (
    id           UUID PRIMARY KEY,
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    company_id   UUID REFERENCES companies(id) ON DELETE SET NULL,
    name         TEXT NOT NULL,
    role         TEXT NOT NULL DEFAULT '',
    company_name TEXT NOT NULL DEFAULT '',
    source       TEXT NOT NULL DEFAULT '',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX contacts_user ON contacts (user_id, company_id);

CREATE TABLE pipeline_items (
    id              UUID PRIMARY KEY,
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    company_id      UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    contact_id      UUID REFERENCES contacts(id) ON DELETE SET NULL,
    stage           TEXT NOT NULL DEFAULT 'interested',
    last_sent_at    TIMESTAMPTZ,       -- the first email or the follow-up, whichever is later
    follow_ups_sent INT NOT NULL DEFAULT 0,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (user_id, company_id)
);

CREATE TABLE pipeline_events (
    id         UUID PRIMARY KEY,
    item_id    UUID NOT NULL REFERENCES pipeline_items(id) ON DELETE CASCADE,
    from_stage TEXT,
    to_stage   TEXT NOT NULL,
    note       TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX pipeline_events_item ON pipeline_events (item_id, created_at);

-- versions: [{tone, subject, body, problems[]}]; ranking: her order of the version indexes.
CREATE TABLE outreach_drafts (
    id           UUID PRIMARY KEY,
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id      UUID NOT NULL REFERENCES pipeline_items(id) ON DELETE CASCADE,
    follow_up    BOOLEAN NOT NULL DEFAULT false,
    contact_role TEXT NOT NULL,
    language     TEXT NOT NULL DEFAULT 'en',
    fact         JSONB NOT NULL,       -- {text, source_url}: the one sourced fact it may use
    times        TEXT NOT NULL DEFAULT '',
    versions     JSONB NOT NULL,
    linkedin     TEXT NOT NULL DEFAULT '',
    phone        TEXT NOT NULL DEFAULT '',
    tweaks       JSONB NOT NULL DEFAULT '[]',
    ranking      JSONB,
    sent_text    TEXT,
    sent_at      TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX outreach_drafts_user ON outreach_drafts (user_id, created_at DESC);

CREATE TABLE suggestions (
    id         UUID PRIMARY KEY,
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    item_id    UUID REFERENCES pipeline_items(id) ON DELETE SET NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('contact', 'company')),
    data       JSONB NOT NULL,
    status     TEXT NOT NULL DEFAULT 'open',   -- open, added, dismissed
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
