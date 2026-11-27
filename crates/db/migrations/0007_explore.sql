-- Phase 5: her display settings, the digest she has seen, and her own company ranking.
CREATE TABLE user_settings (
    user_id        UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    font_step      SMALLINT NOT NULL DEFAULT 0 CHECK (font_step BETWEEN 0 AND 2),
    high_contrast  BOOLEAN NOT NULL DEFAULT false,
    digest_seen_at TIMESTAMPTZ,
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE user_ranking (
    user_id    UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    company_id UUID NOT NULL REFERENCES companies(id) ON DELETE CASCADE,
    position   INT NOT NULL,
    PRIMARY KEY (user_id, company_id)
);
