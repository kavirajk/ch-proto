CLICKHOUSE_VERSION ?= 26.5-alpine

# Path to a ClickHouse source checkout (for the differential test corpus).
CLICKHOUSE_QUERIES ?= $(HOME)/src/ClickHouse/tests/queries/0_stateless

.PHONY: up down test test-integration test-unit test-differential-stage0 test-differential-full corpus-filter test-differential-cleanup

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

# Full differential run against the filtered corpus (~1100 tests, ~6 min).
# Requires the corpus list to exist; run `make corpus-filter` first to
# regenerate it from your ClickHouse source checkout.
test-differential-full: up
	cargo build --release --bin ch-tsv
	test -f tests/differential/corpus_filtered.txt || \
	    { echo "Run 'make corpus-filter' first"; exit 1; }
	tests/differential/run.sh $(CLICKHOUSE_QUERIES) tests/differential/corpus_filtered.txt

# Drop any leftover `test_*` databases from prior harness runs. Each test
# is supposed to DROP its own database at the end, but a mid-run crash or
# error can leak. This sweeps them all in one shot.
test-differential-cleanup: up
	@docker exec ch-proto-clickhouse-1 bash -c "for db in \$$(clickhouse-client -q \"SELECT name FROM system.databases WHERE name LIKE 'test\_%' FORMAT TabSeparated\"); do clickhouse-client -q \"DROP DATABASE IF EXISTS \\\`\$$db\\\`\"; done"
	@echo -n "remaining test_* databases: "; docker exec ch-proto-clickhouse-1 clickhouse-client -q "SELECT count() FROM system.databases WHERE name LIKE 'test\_%'"

# Regenerate the filtered corpus list by walking the ClickHouse stateless
# test directory and keeping only tests the harness can tackle today.
# Filter rules are inline below — adjust them as Stage 2/3 lands and more
# of the corpus becomes tractable.
#
# Stage 2 expansion: CREATE/INSERT/DROP/SET/SETTINGS no longer excluded
# because the harness now wraps each test in a per-test database with
# `CREATE DATABASE / USE / ... / DROP DATABASE`. SET works transparently
# because it's a regular SQL statement on the wire.
#
# Still excluded:
# - FORMAT clauses (test the server's output formatters, not our client)
# - `-- { serverError }` / `-- { clientError }` markers (Stage 3)
# - `-- Tags:` (stateful needs test.hits dataset; distributed needs a cluster)
# - `system.*` instance-specific tables (non-deterministic)
# - now()/rand()/generateUUIDv4()/currentUser() (non-deterministic)
# - currentDatabase() (resolves to our per-test name, not the canonical one)
corpus-filter:
	@cd $(CLICKHOUSE_QUERIES) && for f in *.sql; do \
	    grep -qiE "FORMAT[[:space:]]+(JSON|CSV|Pretty|Vertical|RowBinary|Values|XML|Markdown|TSV|TabSeparated|Native|LineAsString|Raw)|serverError|clientError|ATTACH|DETACH|GRANT|REVOKE|EXPLAIN|^-- Tags:" "$$f" && continue; \
	    grep -qE "test\.(hits|visits)|system\.(parts|columns|tables|processes|metrics|asynchronous_metrics|merges|replicas|clusters|disks|users|grants|privileges|databases|formats|functions|build_options|errors|table_engines|data_skipping|projections|dictionaries|filesystem)" "$$f" && continue; \
	    grep -qE "now\(\)|rand\(\)|randCanonical|generateUUIDv4|currentDatabase|currentUser|hostName|tcpPort|fqdn|uptime" "$$f" && continue; \
	    [ -s "$$f" ] && echo "$$f"; \
	done > $(CURDIR)/tests/differential/corpus_filtered.txt
	@wc -l tests/differential/corpus_filtered.txt
