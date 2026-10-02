-- Pulse read model. Written only by story-sink, read by query-api.
--
-- Positions are offsets in `articles.embedded`, the Story Processor's input
-- log: every story event carries the offset of the input that caused it. They
-- are monotonic in processing order (event times are not: late articles), so
-- history is versioned by offset and time travel maps a time to an offset.

CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE articles (
    id                   text PRIMARY KEY,
    input_offset         bigint      NOT NULL,
    source_id            text        NOT NULL,
    source_kind          smallint    NOT NULL,
    url                  text        NOT NULL,
    title                text        NOT NULL,
    summary              text        NOT NULL,
    lang                 text        NOT NULL,
    published_at         timestamptz NOT NULL,
    fetched_at           timestamptz NOT NULL,
    event_time_corrected boolean     NOT NULL,
    model_version        text        NOT NULL,
    -- Raw e5 passage vector, for semantic search with e5 query vectors.
    -- NULL when the model's dimension differs (e.g. synthetic test data).
    embedding            vector(384)
);
CREATE INDEX articles_input_offset ON articles (input_offset);
CREATE INDEX articles_fetched_at ON articles (fetched_at);
CREATE INDEX articles_embedding ON articles USING hnsw (embedding vector_cosine_ops);

CREATE TABLE stories (
    id                  text PRIMARY KEY,
    seed_article_id     text        NOT NULL,
    created_offset      bigint      NOT NULL,
    created_at          timestamptz NOT NULL,
    updated_offset      bigint      NOT NULL,
    updated_at          timestamptz NOT NULL,
    closed_offset       bigint,
    closed_at           timestamptz,
    close_reason        text,
    headline            text        NOT NULL,
    headline_article_id text        NOT NULL,
    lang                text        NOT NULL,
    article_count       integer     NOT NULL DEFAULT 1,
    source_count        integer     NOT NULL DEFAULT 1,
    langs               text[]      NOT NULL DEFAULT '{}',
    -- Centered-space centroid (the processor's clustering space).
    centroid            vector(384),
    parent_ids          text[]      NOT NULL DEFAULT '{}',
    merged_from         text[]      NOT NULL DEFAULT '{}',
    merged_into         text
);
CREATE INDEX stories_open_recent ON stories (updated_offset DESC) WHERE closed_offset IS NULL;
CREATE INDEX stories_created_offset ON stories (created_offset);

-- Which story an article belongs to, valid for input offsets [from_offset, to_offset).
-- Splits and merges close rows and open new ones, so any past state is a query.
CREATE TABLE memberships (
    article_id   text        NOT NULL,
    story_id     text        NOT NULL,
    from_offset  bigint      NOT NULL,
    to_offset    bigint,
    added_at     timestamptz NOT NULL,
    score        real        NOT NULL DEFAULT 0,
    is_duplicate boolean     NOT NULL DEFAULT false,
    duplicate_of text,
    late         boolean     NOT NULL DEFAULT false,
    PRIMARY KEY (article_id, from_offset)
);
CREATE INDEX memberships_current ON memberships (story_id) WHERE to_offset IS NULL;
CREATE INDEX memberships_history ON memberships (story_id, from_offset);

-- Every story event, in processing order, as JSON for the live stream.
CREATE TABLE story_events (
    input_offset bigint      NOT NULL,
    seq          integer     NOT NULL,
    event_id     text        NOT NULL UNIQUE,
    event_time   timestamptz NOT NULL,
    watermark    timestamptz,
    kind         text        NOT NULL,
    story_id     text        NOT NULL,
    payload      jsonb       NOT NULL,
    PRIMARY KEY (input_offset, seq)
);
CREATE INDEX story_events_story ON story_events (story_id, input_offset DESC, seq DESC);

-- The sink's position in each topic, committed atomically with the rows it wrote.
CREATE TABLE sink_offsets (
    topic       text    NOT NULL,
    partition   integer NOT NULL,
    next_offset bigint  NOT NULL,
    PRIMARY KEY (topic, partition)
);
