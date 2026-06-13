# Convenience targets for local development of the Portunus operator
# server + Web UI.
#
# Most developers want:
#   make dev            # hot-reload front-end + backend together
#
# First run of `make dev` (or `make backend`) auto-bootstraps a
# `_superadmin` operator account and sets a FIXED development password
# (DEV_PASSWORD, default `superadmin123`). There is NO forced password
# change — log in with the same credentials every time.
# Login at http://localhost:5173 with:
#     user_id  = _superadmin
#     password = superadmin123   (or your DEV_PASSWORD override)
# Override with `make dev DEV_PASSWORD=mylongpassword` (>= 12 chars).
# Reset local state? `make clean && make dev` re-applies it.
#
# Producing a release-shaped binary that embeds the UI:
#   make setup          # one-time per checkout
#   make bootstrap      # one-time per DATA_DIR
#   make serve          # run the embedded build on http://localhost:7080
#
# Reproducing the Docker `--operator-http-listen 0.0.0.0:7080` scenario:
#   make serve-docker
#
# Variables (override on the command line, e.g. `make serve LISTEN=…`):
#   DATA_DIR        Where the server stores state.db + TLS material.
#                   Default: /tmp/portunus-dev. `make clean` wipes it.
#   LISTEN          Operator HTTP bind (host:port). Default: 127.0.0.1:7080.
#                   Note: the Vite proxy is hard-coded to 127.0.0.1:7080
#                   in webui/vite.config.ts — changing LISTEN for `make
#                   dev` only works if you sync that file too.
#   CARGO_PROFILE   `release` (default) or `dev`. Controls which target/
#                   subdir `$(SERVER_BIN)` resolves to.
#   DEMO_ARGS       Extra flags forwarded to `scripts/demo.sh` by the
#                   `demo` target (e.g. --users 5 --rules-per-user 3).
#                   Default: empty.

DATA_DIR    ?= /tmp/portunus-dev
LISTEN      ?= 127.0.0.1:7080
CARGO_PROFILE ?= release
DEMO_ARGS   ?=
# Fixed Web UI password set for `_superadmin` on first `make dev` /
# `make backend`. Must be >= 12 chars (server-side minimum). Override
# on the command line, e.g. `make dev DEV_PASSWORD=mylongpassword`.
DEV_PASSWORD ?= superadmin123
SERVER_BIN  := target/$(CARGO_PROFILE)/portunus-server

ifeq ($(CARGO_PROFILE),release)
CARGO_FLAGS := --release
else
CARGO_FLAGS :=
endif

.DEFAULT_GOAL := help
.PHONY: help setup webui-install webui-build server-build bootstrap \
        dev-bootstrap serve serve-docker dev backend ui test test-csrf clean \
        demo

help:
	@awk 'BEGIN{FS=":.*##"} /^[a-zA-Z_-]+:.*##/ {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

## --- one-time setup ---------------------------------------------------------

# Run once after `git clone` (or after pulling a new lockfile / webui change).
# Idempotent — re-running is safe but slow. Skip this if you're only going
# to use `make dev` (which does NOT need the embedded UI bundle).
setup: webui-install webui-build server-build  ## Install pnpm deps, build UI, build server

# Materialise webui/node_modules from pnpm-lock.yaml. Frozen lockfile so
# CI and local stay in sync; if pnpm-lock.yaml is stale you'll see an
# error here and need to bump it deliberately.
webui-install:  ## pnpm install --frozen-lockfile (webui/)
	cd webui && pnpm install --frozen-lockfile

# Compile the React SPA to webui/dist/. The next cargo build of
# portunus-server picks it up via rust-embed and bakes the static files
# into the binary, so `make serve` will serve the UI on the same port as
# the operator HTTP API. Re-run this whenever you change UI source and
# want the embedded copy to update (i.e. when NOT using `make dev`).
webui-build: webui/node_modules  ## Build embedded Web UI bundle
	cd webui && pnpm build

# Compile the server binary. With CARGO_PROFILE=release this produces
# target/release/portunus-server (optimised, slow build). For inner-loop
# work use `make CARGO_PROFILE=dev server-build`. The build script
# requires webui/dist/index.html to exist — if you haven't run
# `webui-build` it will error and tell you so.
server-build:  ## Build portunus-server (CARGO_PROFILE=release|dev)
	cargo build $(CARGO_FLAGS) -p portunus-server

## --- one-time per DATA_DIR --------------------------------------------------

# Create the reserved `_superadmin` operator account and print its bearer
# token EXACTLY ONCE. Capture the token from stdout if you want to drive
# the API from curl/CLI; for browser login (preferred) you can ignore the
# token — first login lets you set a password. Run this once per fresh
# DATA_DIR; subsequent runs against the same DATA_DIR error with
# `already_bootstrapped`. Use `make clean` then `make bootstrap` to reset.
bootstrap: server-build  ## Create _superadmin in $(DATA_DIR); prints bearer token
	mkdir -p $(DATA_DIR)
	$(SERVER_BIN) --data-dir $(DATA_DIR) bootstrap-superadmin --name ops

# `dev-bootstrap` is the all-in-one credentials helper invoked
# implicitly by `make dev` / `make backend` on first run. It:
#   1. Creates the `_superadmin` operator account (one-shot, errors if
#      already bootstrapped).
#   2. Sets a FIXED development password (DEV_PASSWORD, default
#      `superadmin123`) via `reset-password --password-stdin`, with NO
#      forced password change — so the Web UI login is just
#      user_id=`_superadmin` + that password every time.
# Marker file `$(DATA_DIR)/.dev-credentials-set` is touched on success
# so subsequent `make dev` runs don't re-bootstrap (which would error).
# To regenerate creds: `make clean && make dev`.
dev-bootstrap: $(DATA_DIR)/.dev-credentials-set  ## (internal) Bootstrap superadmin + temp password if not yet done

$(DATA_DIR)/.dev-credentials-set:
	@mkdir -p $(DATA_DIR)
	@echo ""
	@echo "==============================================================="
	@echo "  First-run bootstrap of $(DATA_DIR)"
	@echo "==============================================================="
	PORTUNUS_SKIP_WEBUI=1 cargo run -p portunus-server -- \
	  --data-dir $(DATA_DIR) bootstrap-superadmin --name ops
	@echo ""
	@echo "→ setting fixed dev password for _superadmin..."
	printf '%s\n' "$(DEV_PASSWORD)" | PORTUNUS_SKIP_WEBUI=1 cargo run -p portunus-server -- \
	  --data-dir $(DATA_DIR) reset-password _superadmin --password-stdin --keep-api-tokens
	@touch $(DATA_DIR)/.dev-credentials-set
	@echo ""
	@echo "==============================================================="
	@echo "  Web UI login (fixed dev credentials, no forced change):"
	@echo "    user_id  = _superadmin"
	@echo "    password = $(DEV_PASSWORD)"
	@echo "  Override with DEV_PASSWORD=… (>= 12 chars)."
	@echo "==============================================================="
	@echo ""

## --- run --------------------------------------------------------------------

# Run the production-shaped server: single binary, embedded UI served at
# /, operator HTTP + Web UI on $(LISTEN). Blocks the terminal — Ctrl-C
# stops it. Requires `make setup` + `make bootstrap` to have run.
# Browser entry point: http://$(LISTEN)/
serve: server-build  ## Run server with embedded UI on http://$(LISTEN)
	$(SERVER_BIN) --data-dir $(DATA_DIR) serve --operator-http-listen $(LISTEN)

# Same as `serve` but binds 0.0.0.0:7080, reproducing the docker-compose
# default. Use this to verify the CSRF same-origin fix: from the host
# browser, http://localhost:7080 and http://127.0.0.1:7080 must both work
# without csrf_origin_mismatch (pre-fix this was broken).
serve-docker: server-build  ## Run server bound to 0.0.0.0:7080 (simulates Docker)
	$(SERVER_BIN) --data-dir $(DATA_DIR) serve --operator-http-listen 0.0.0.0:7080

# The everyday dev loop: one command, two processes, hot reload on both
# sides. Auto-bootstraps the superadmin on first run if state.db doesn't
# exist yet. Backend runs via `cargo run` with PORTUNUS_SKIP_WEBUI=1 (no
# UI bundle needed → no pnpm build → faster rebuilds when you touch Rust
# code). Front-end runs via `pnpm dev` which serves at :5173 and proxies
# /v1 + /metrics → 127.0.0.1:7080.
#   • Browser entry point: http://localhost:5173
#   • UI edits hot-reload through Vite (no rebuild)
#   • Backend edits: Ctrl-C and re-run `make dev` (cargo incremental
#     rebuilds the changed crates only)
# `trap 'kill 0'` tears down both child processes on Ctrl-C via the
# shared shell process group. Each child is also wrapped in a subshell
# so that if EITHER exits on its own (e.g. the backend fails to start —
# `serve failed: ...`), the other is killed and `make dev` exits
# non-zero with a pointer to the cause, instead of silently lingering as
# a half-up dev env (Vite alone, every /v1 request ECONNREFUSED).
dev: dev-bootstrap webui/node_modules  ## Run backend (skip embed) + Vite UI together — open http://localhost:5173
	@echo "→ backend on http://$(LISTEN)  |  UI on http://localhost:5173  (Ctrl-C stops both)"
	@trap 'kill 0' INT TERM; \
	  ( PORTUNUS_SKIP_WEBUI=1 cargo run -p portunus-server -- \
	      --data-dir $(DATA_DIR) serve --operator-http-listen $(LISTEN); \
	    st=$$?; echo ""; \
	    echo ">>> make dev: BACKEND exited (status=$$st). Scroll up for the cause (e.g. 'serve failed: ...'). Tearing down." >&2; \
	    kill 0 ) & \
	  ( cd webui && pnpm dev; \
	    st=$$?; echo ""; \
	    echo ">>> make dev: Vite exited (status=$$st). Tearing down." >&2; \
	    kill 0 ) & \
	  wait

# Vite needs webui/node_modules before `pnpm dev` can start. On a fresh
# clone (or after `make clean` blew it away) this directory is missing
# and `make dev` would crash with "pnpm: command not found" or "vite:
# not found". Treat node_modules as a file-target so make runs
# `webui-install` once, then never again unless someone deletes it.
webui/node_modules: webui/pnpm-lock.yaml
	cd webui && pnpm install --frozen-lockfile
	@touch webui/node_modules

# Backend-only variant of `dev`. Use this when you want to run the UI in
# a separate terminal (e.g. to keep its log clean), or when pairing with
# `make ui`. Same auto-bootstrap behaviour. PORTUNUS_SKIP_WEBUI=1 means
# accessing 7080 directly serves only a stub HTML — go through Vite at
# 5173 for the real UI.
backend: dev-bootstrap  ## Run server with PORTUNUS_SKIP_WEBUI=1 (for pairing with `make ui`)
	PORTUNUS_SKIP_WEBUI=1 cargo run -p portunus-server -- \
	  --data-dir $(DATA_DIR) serve --operator-http-listen $(LISTEN)

# Front-end-only variant. Vite dev server, hot reload, proxies /v1 +
# /metrics to a backend you must run separately (`make backend` or
# anything that listens on 127.0.0.1:7080). Browser: http://localhost:5173
ui: webui/node_modules  ## Run Vite dev server on http://localhost:5173 (proxies /v1 → 7080)
	cd webui && pnpm dev

## --- test -------------------------------------------------------------------

# Full server test suite that doesn't require a real network: 217 unit
# tests + the auth-session + password contract tests (which cover the
# CSRF middleware paths). Uses PORTUNUS_SKIP_WEBUI=1 so the test binary
# build doesn't depend on webui/dist.
test:  ## Run server lib tests + auth contract tests
	PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib
	PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server \
	  --test http_auth_session_contract --test http_password_contract

# Focused: just the CSRF unit tests (same-origin / explicit-origin /
# missing-Origin / missing-Host paths). Sub-second feedback loop while
# iterating on operator/csrf.rs.
test-csrf:  ## Run just the CSRF unit tests (fast)
	PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib operator::csrf

## --- cleanup ----------------------------------------------------------------

# Nuke the local data directory. After this, `make serve` / `make dev`
# will auto-rerun bootstrap. Useful when you want a fresh _superadmin or
# when a SQLite migration left state in a confusing shape during dev.
# Does NOT touch target/ — use `cargo clean` for that.
clean:  ## Remove $(DATA_DIR) (forces re-bootstrap on next run)
	rm -rf $(DATA_DIR)

## --- standalone ---------------------------------------------------------------

standalone:  ## Build portunus-standalone binary (CARGO_PROFILE=release|dev)
	PORTUNUS_SKIP_WEBUI=1 cargo build $(CARGO_FLAGS) -p portunus-standalone

standalone-check:  ## Validate all valid_*.toml fixtures via --check (expects "ok" × 3)
	@for f in crates/portunus-standalone/tests/fixtures/valid_*.toml; do \
	  result=$$(PORTUNUS_SKIP_WEBUI=1 cargo run $(CARGO_FLAGS) -p portunus-standalone --quiet -- --check --config "$$f" 2>/dev/null); \
	  echo "$$f: $$result"; \
	done

## --- demo -------------------------------------------------------------------

# Stand up a full multi-user demo: server + N RBAC users each with an
# independent edge client and K real forwarding rules to local echo
# upstreams. Starts the Vite Web UI on http://localhost:5173, verifies
# real end-to-end TCP forwarding + RBAC isolation, then holds the
# environment open (Ctrl-C tears everything down). Uses an isolated
# /tmp/portunus-demo data dir — does not touch `make dev` state.
# Web UI login is _superadmin / portunus-demo-password by default
# (override with PORTUNUS_DEMO_PASSWORD=...).
# Override args, e.g.: make demo DEMO_ARGS="--users 5 --rules-per-user 3"
demo:  ## Multi-user demo: server + N edges + K rules, verify + hold open
	@bash scripts/demo.sh $(DEMO_ARGS)
