-- Phase 1b: facts Kompas reads in an uploaded document, each one hers to confirm (spec §3, §10b).

-- How many facts the snippet guard dropped because their quote was not in the text.
ALTER TABLE documents ADD COLUMN dropped INTEGER NOT NULL DEFAULT 0;

CREATE TABLE document_facts (
    id          UUID PRIMARY KEY,
    document_id UUID NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    text        TEXT NOT NULL,
    snippet     TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'confirmed', 'rejected')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (document_id, seq)
);
CREATE INDEX document_facts_user ON document_facts (user_id, status);
