.PHONY: build build-rust build-ts build-cortex build-napi build-adapters build-mcp-sidecar test test-all test-rust test-ts test-cortex test-cortex-napi test-cortex-mcp test-cortex-mcp-navigate test-cortex-mcp-navigate-payload test-cortex-adapters test-e2e test-real-extraction lint lint-rust lint-rust-fmt lint-rust-clippy lint-ts fmt fmt-rust clean dev-tauri build-tauri

build: build-rust build-ts

build-rust:
	cargo build --workspace

build-ts:
ifdef CI
	pnpm install --frozen-lockfile
else
	pnpm install
endif
	pnpm build

test: test-rust test-ts

test-rust:
	cargo test --workspace

test-ts:
	pnpm test

test-e2e:
	cd e2e && npx playwright test --project=agent-engine --project=recorder --project=context-pipeline --project=adversarial

test-real-extraction:
	cargo build -p cel-context --example context_snapshot --release
	cd e2e && npx playwright test --project=real-extraction

test-e2e-ui:
	cd e2e && npx playwright install chromium && npx playwright test

lint: lint-rust lint-ts

lint-rust: lint-rust-fmt lint-rust-clippy

lint-rust-fmt:
	cargo fmt --all -- --check

lint-rust-clippy:
	cargo clippy --workspace -- -D warnings

lint-ts:
	pnpm lint

# Auto-fix formatting (companion to lint-rust-fmt)
fmt: fmt-rust

fmt-rust:
	cargo fmt --all

build-cortex:
	cargo build -p cel-cortex

build-napi:
	cargo build --release -p cel-napi
	cp target/release/libcel_napi.dylib cel/cel-napi/cel-napi.darwin-arm64.node

# Build all ProcessDriver adapter binaries. The cortex discovers them at
# runtime via each `adapters/<name>/adapter.json`; the entrypoint there
# points at `../../target/release/adapter-<name>`. Run this whenever you
# add a new adapter or change one's lib code.
build-adapters:
	cargo build --release -p adapter-mail -p adapter-calendar -p adapter-reminders -p adapter-messages

build-mcp-sidecar: build-napi
	cd mcp-server && pnpm build
	mkdir -p app/src-tauri/binaries
	@if command -v bun >/dev/null 2>&1; then \
		bun build --compile mcp-server/dist/index.js --outfile app/src-tauri/binaries/cel-mcp-aarch64-apple-darwin; \
	else \
		echo "bun not installed — sidecar binary not built. Use 'node mcp-server/dist/index.js' instead."; \
	fi
	cp cel/cel-napi/cel-napi.darwin-arm64.node app/src-tauri/binaries/

test-all: test-rust test-ts test-cortex test-cortex-napi test-cortex-mcp

test-cortex:
	cargo test -p cel-cortex

test-cortex-napi:
	node tests/cortex/napi-smoke.mjs

test-cortex-mcp:
	node tests/cortex/mcp-protocol.mjs

test-cortex-mcp-navigate:
	node tests/cortex/mcp-act-navigate.mjs

test-cortex-mcp-navigate-payload:
	node tests/cortex/mcp-act-navigate-payload.mjs

# Smoke-test the four ProcessDriver productivity adapters end-to-end via
# MCP. Requires Automation permission for the apps (Mail / Calendar /
# Reminders) and Full Disk Access for Messages. Set
# CEL_TEST_REMINDERS_LIST=<list> if the default "Reminders" list does not
# exist on the test machine.
test-cortex-adapters: build-napi build-adapters
	cd mcp-server && pnpm build
	node tests/cortex/mcp-adapter-mail.mjs
	node tests/cortex/mcp-adapter-calendar.mjs
	node tests/cortex/mcp-adapter-reminders.mjs
	node tests/cortex/mcp-adapter-messages.mjs

dev-tauri:
	cd app && pnpm tauri dev

build-tauri: build-mcp-sidecar
	cd app && pnpm tauri build

clean:
	cargo clean
	pnpm -r exec rm -rf dist
	rm -rf node_modules
