-- "Listen in any language": translation and speech caches, and provider usage.
-- Both caches are content-addressed, so a repeat request costs nothing and a
-- changed briefing never reuses stale audio.

-- One translated text segment. `source_hash` = sha256 of the source text.
CREATE TABLE translations (
    source_hash text        NOT NULL,
    target      text        NOT NULL,
    provider    text        NOT NULL,
    text        text        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (source_hash, target)
);

-- Synthesized briefing audio. `key` = sha256(provider, model, voice, lang, text);
-- the audio itself lives in the object store under `briefings/<key>`.
CREATE TABLE briefing_audio (
    key          text PRIMARY KEY,
    story_id     text        NOT NULL,
    lang         text        NOT NULL,
    provider     text        NOT NULL,
    model        text        NOT NULL,
    voice        text        NOT NULL,
    text         text        NOT NULL,
    characters   integer     NOT NULL,
    bytes        integer     NOT NULL,
    content_type text        NOT NULL,
    -- Word starts for transcript highlighting: [[utf16_offset, seconds], ...].
    words        jsonb       NOT NULL,
    duration     real        NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    plays        integer     NOT NULL DEFAULT 0
);
CREATE INDEX briefing_audio_story ON briefing_audio (story_id, created_at DESC);

-- Characters sent to each paid provider per UTC day, for budgets.
CREATE TABLE listen_usage (
    day        date   NOT NULL,
    provider   text   NOT NULL,
    characters bigint NOT NULL DEFAULT 0,
    requests   integer NOT NULL DEFAULT 0,
    PRIMARY KEY (day, provider)
);
