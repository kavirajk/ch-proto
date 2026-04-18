CLICKHOUSE_VERSION ?= 25.3-alpine

.PHONY: up down test test-integration test-unit

up:
	CLICKHOUSE_VERSION=$(CLICKHOUSE_VERSION) docker compose up -d --wait --remove-orphans --force-recreate

down:
	docker compose down

test-unit:
	cargo test

test-integration: up
	cargo test --features integration -- --test-threads=1

test: test-unit test-integration
