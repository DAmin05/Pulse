SHELL := /bin/bash
COMPOSE := docker compose -f deploy/docker-compose.yml --env-file .env
BUF := docker run --rm -v "$(CURDIR):/workspace" -w /workspace bufbuild/buf:1.57.0
PY := embedder/.venv/bin/python

.DEFAULT_GOAL := help

help: ## Show targets
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-14s\033[0m %s\n", $$1, $$2}'

# --- Stack -------------------------------------------------------------------

.env:
	cp .env.example .env

env: .env ## Create .env from .env.example if missing

up: .env ## Start the local stack and wait until healthy
	$(COMPOSE) up -d --wait --wait-timeout 180
	@$(COMPOSE) run --rm --no-deps redpanda-init
	@$(COMPOSE) run --rm --no-deps seaweedfs-init 2>&1 | grep -E 'created|exist|error' || true

down: ## Stop the stack (keeps data)
	$(COMPOSE) down

nuke: ## Stop the stack and delete all data volumes
	$(COMPOSE) down -v

ps: ## Show stack status
	$(COMPOSE) ps

logs: ## Follow stack logs
	$(COMPOSE) logs -f --tail=100

topics: ## List Kafka topics
	$(COMPOSE) exec redpanda rpk topic list

doctor: ## Verify the stack from the host
	cargo run -q -p pulse-cli -- doctor

# --- Ingestor ---------------------------------------------------------------

ingest: ## Run the ingestor (polls config/sources.toml → articles.raw)
	cargo run -p ingestor --release -- run

sources-check: ## Poll every source once and report health (publishes nothing)
	cargo run -q -p ingestor --release -- check

FIXTURE ?= data/fixtures/raw-$(shell date -u +%Y%m%d).pulsefx
fixture-record: ## Record the last 24h of articles.raw into $(FIXTURE)
	cargo run -q -p pulse-cli --release -- fixture record --since 24h --out $(FIXTURE)
	cargo run -q -p pulse-cli --release -- fixture stats $(FIXTURE)

# --- Embedding ---------------------------------------------------------------

model: py-setup ## Download multilingual-e5-small (pinned, checksummed) and build int8
	$(PY) -m pip install -q -e 'embedder[tools]'
	$(PY) embedder/scripts/fetch_model.py

embedder: py-proto ## Run the Embedder gRPC service (:50061, metrics :9102)
	cd embedder && set -a && [ -f ../.env ] && . ../.env; set +a; PYTHONPATH=src .venv/bin/python -m pulse_embedder

relay: ## Run the embed relay (articles.raw → articles.embedded)
	cargo run -p embed-relay --release

bench: py-proto ## Benchmark dynamic batching (needs a fixture; FIXTURE=...)
	cd embedder && PYTHONPATH=src .venv/bin/python bench/bench.py --fixture $(abspath $(FIXTURE))

chaos-relay: ## kill -9 the relay repeatedly; verify exactly-once output
	scripts/chaos/relay.sh 5

# --- Story Processor ---------------------------------------------------------

process: ## Run the Story Processor live (articles.embedded → stories.events)
	cargo run -p story-processor --release -- run

reset-processor: ## Delete processor state + outputs (snapshots, offsets, topics, read model); everything reprocesses from the start
	rm -rf data/checkpoints/story-processor
	cargo run -q -p story-sink --release -- reset
	-$(COMPOSE) exec -T redpanda rpk group delete story-processor
	-$(COMPOSE) exec -T redpanda rpk topic delete stories.events articles.late
	@$(COMPOSE) run --rm --no-deps redpanda-init

chaos-processor: ## kill -9 the processor repeatedly; output must be byte-identical to a clean run
	scripts/chaos/processor.sh $(or $(KILLS),10) $(or $(FIXTURE),data/fixtures/synth.pulseem)

# --- Read model & API --------------------------------------------------------

sink: ## Run the Story Sink (articles.embedded + stories.events → Postgres)
	cargo run -p story-sink --release

api: ## Run the Query API (REST + SSE on :9105)
	cargo run -p query-api --release

pipeline: ## Run every service (ingest → embed → process → sink → API); Ctrl-C stops all
	scripts/pipeline.sh

test-db: ## Read-model integration tests against the local Postgres
	PULSE_TEST_DATABASE_URL=$${PULSE_DATABASE_URL:-postgres://pulse:pulse@localhost:5432/pulse} cargo test -p pulse-store

replay: ## Re-drive input offsets [FROM, TO) and diff against live output (exit 1 if different)
	cargo run -q -p story-processor --release -- replay $(if $(FROM),--from $(FROM)) $(if $(TO),--to $(TO))

EMBEDDED ?= data/fixtures/smoke.pulseem
cluster-eval: ## Cluster an embedded fixture and print story quality (EMBEDDED=...)
	cargo run -q -p story-processor --release -- eval --fixture $(EMBEDDED) --top 10 --audit 10

cluster-sweep: ## Sweep similarity thresholds on an embedded fixture
	cargo run -q -p story-processor --release -- sweep --fixture $(EMBEDDED)

ann-recall: ## HNSW recall@10 vs brute force on an embedded fixture
	cargo run -q -p story-processor --release -- recall --fixture $(EMBEDDED)

# --- Frontend ----------------------------------------------------------------

web-install: ## Install frontend dependencies (pnpm)
	pnpm --dir web install --frozen-lockfile

web: ## Run the frontend dev server on :5173 (proxies /api to $$PULSE_API or :9105)
	pnpm --dir web dev

web-build: ## Type-check and build the frontend into web/dist
	pnpm --dir web build

web-check: ## Lint, type-check and test the frontend
	pnpm --dir web lint
	pnpm --dir web test

# --- Rust --------------------------------------------------------------------

build: ## Build all Rust crates
	cargo build --workspace

test: ## Run Rust tests
	cargo test --workspace

fmt: ## Format Rust code
	cargo fmt --all

lint: ## Lint Rust code
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings

# --- Protobuf ----------------------------------------------------------------

proto-lint: ## Lint protobuf schemas with buf
	$(BUF) lint

# --- Python embedder ---------------------------------------------------------

$(PY):
	python3 -m venv embedder/.venv
	$(PY) -m pip install -q --upgrade pip
	$(PY) -m pip install -q -e 'embedder[dev]'

py-setup: $(PY) ## Create the embedder virtualenv

py-proto: $(PY) ## Generate Python protobuf/gRPC code
	cd embedder && PATH="$(CURDIR)/embedder/.venv/bin:$$PATH" ./scripts/gen_proto.sh

py-test: py-proto ## Lint and test the embedder
	cd embedder && .venv/bin/ruff check . && .venv/bin/pytest -q

# --- Everything --------------------------------------------------------------

check: lint test proto-lint py-test web-check ## Run every check CI runs


.PHONY: help env up down nuke ps logs topics doctor ingest sources-check fixture-record build test fmt lint proto-lint py-setup py-proto py-test check model embedder relay bench chaos-relay web-install web web-build web-check
