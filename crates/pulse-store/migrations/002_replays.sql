-- Replay runs requested through the API (see story_processor::replay).
CREATE TABLE replays (
    id           text PRIMARY KEY,
    status       text        NOT NULL CHECK (status IN ('queued', 'running', 'done', 'failed')),
    from_offset  bigint      NOT NULL,
    to_offset    bigint,
    output_topic text,
    requested_at timestamptz NOT NULL DEFAULT now(),
    started_at   timestamptz,
    finished_at  timestamptz,
    identical    boolean,
    report       jsonb,
    error        text
);
CREATE INDEX replays_recent ON replays (requested_at DESC);
