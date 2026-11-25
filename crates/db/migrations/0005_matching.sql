-- Phase 3: the profile engine. Config per seeker, model judgements cached per
-- (profile version, company text), one fit score per company, the weekly five, her thumbs,
-- and road travel times from the home base (the base itself lives only in server config).

CREATE TABLE match_configs (
    user_id UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    profile_version INT NOT NULL,
    config JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE feedback (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    company_id UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    up BOOLEAN NOT NULL,
    axis TEXT NOT NULL,
    reason TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX feedback_user ON feedback (user_id, created_at);

CREATE TABLE axis_cache (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    profile_version INT NOT NULL,
    company_id UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    company_hash TEXT NOT NULL,
    answer JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, profile_version, company_id, company_hash)
);

CREATE TABLE fit_scores (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    company_id UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    profile_version INT NOT NULL,
    score DOUBLE PRECISION,            -- NULL when a hard filter excluded it
    excluded TEXT,
    axes JSONB NOT NULL,
    travel_min INT,                    -- NULL = UNKNOWN
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, company_id)
);

CREATE TABLE digests (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    items JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX digests_user ON digests (user_id, created_at DESC);

CREATE TABLE travel_times (
    company_id UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    home_key TEXT NOT NULL,            -- sha256 of the home coordinate, never the coordinate
    minutes INT,                       -- NULL = the router gave no route
    source_url TEXT NOT NULL,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (company_id, home_key)
);
