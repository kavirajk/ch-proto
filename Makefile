CLICKHOUSE_VERSION ?= 26.5-alpine

# Path to a ClickHouse source checkout (for the differential test corpus).
CLICKHOUSE_QUERIES ?= $(HOME)/src/ClickHouse/tests/queries/0_stateless

.PHONY: up down test test-integration test-unit test-differential-stage0

up:
	CLICKHOUSE_VERSION=$(CLICKHOUSE_VERSION) docker compose up -d --wait --remove-orphans --force-recreate

down:
	docker compose down

test-unit:
	cargo test

test-integration: up
	cargo test --features integration -- --test-threads=1

test: test-unit test-integration

# Stage 0 differential harness: run a hand-picked allowlist of ClickHouse
# stateless query tests through our wrapper and diff against `.reference`.
test-differential-stage0: up
	cargo build --release --bin ch-tsv
	tests/differential/run.sh $(CLICKHOUSE_QUERIES) tests/differential/stage0.txt
