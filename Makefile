.DEFAULT_GOAL := help

CARGO ?= cargo
FEATURES ?=
ARGS ?=
TEST_PACKAGE ?=
TEST_ARGS ?=
SUITE ?= headless
PROJECT ?= fixtures/demo.rawdawg
OUTPUT ?= target/verification/demo.wav
MODE ?= pattern
SECONDS ?= 60
FORMAT ?= deb

# Use a shell argument for each path or value, including paths with spaces or quotes.
quote = '$(subst ','"'"',$(1))'
feature_flags = $(if $(strip $(FEATURES)),--features $(call quote,$(FEATURES)))
test_scope = $(if $(strip $(TEST_PACKAGE)),--package $(call quote,$(TEST_PACKAGE)),--workspace)
# Spell out the Cargo alias so developer tasks also honor Cargo.lock.
xtask = $(CARGO) run --quiet --locked --package xtask --

.PHONY: help build build-plugins release run dev devices fmt fmt-check check clippy test doc \
        fixtures verify verify-headless verify-plugins verify-all render bench-audio \
        ci audit licenses package clean

help: ## Show available targets and overrides
	@printf '%s\n\n' 'Usage: make [target] [VARIABLE=value ...]'
	@awk '/^## / { printf "\n%s\n", substr($$0, 4) } \
	  /^[a-z][a-z-]*:.* ## / { \
	    split($$0, entry, " ## "); \
	    sub(/:.*/, "", entry[1]); \
	    printf "  %-18s %s\n", entry[1], entry[2]; \
	  }' $(call quote,$(lastword $(MAKEFILE_LIST)))
	@printf '\nExamples\n'
	@printf '%s\n' \
	  '  make run ARGS="--safe-mode"' \
	  '  make build FEATURES=rawdawg/jack' \
	  '  make test TEST_PACKAGE=rawdawg-dsp TEST_ARGS="-- --nocapture"' \
	  '  make render PROJECT=fixtures/demo-song.rawdawg MODE=song OUTPUT=target/song.wav' \
	  '  make verify SUITE=plugins' \
	  '  make bench-audio SECONDS=10' \
	  '  make package FORMAT=msi'

## Build and run
build: ## Build the workspace in debug mode
	$(CARGO) build --workspace --locked $(feature_flags)

build-plugins: ## Build the debug CLAP and VST3 modules used by verification
	$(CARGO) build --locked --package rawdawg-plugin-clap --package rawdawg-plugin-vst3

release: ## Build the optimized workspace, including plugin crates
	$(CARGO) build --workspace --release --locked $(feature_flags)

run: ## Launch the optimized DAW (optional ARGS and FEATURES)
	$(CARGO) run --release --locked --package rawdawg $(feature_flags) -- $(ARGS)

dev: ## Launch the debug DAW (optional ARGS and FEATURES)
	$(CARGO) run --locked --package rawdawg $(feature_flags) -- $(ARGS)

devices: ## List audio hosts and devices as JSON
	$(CARGO) run --locked --package rawdawg $(feature_flags) -- --list-audio-devices

## Code quality
fmt: ## Apply rustfmt to the workspace
	$(CARGO) fmt --all

fmt-check: ## Check formatting
	$(CARGO) fmt --all -- --check

check: ## Type-check all workspace targets
	$(CARGO) check --workspace --all-targets --locked $(feature_flags)

clippy: ## Lint all workspace targets and fail on warnings
	$(CARGO) clippy --workspace --all-targets --locked $(feature_flags) -- -D warnings

test: ## Run tests (optional TEST_PACKAGE and TEST_ARGS)
	$(CARGO) test $(test_scope) --locked $(feature_flags) $(TEST_ARGS)

doc: ## Generate workspace API docs without dependency docs
	$(CARGO) doc --workspace --no-deps --locked $(feature_flags)

## Fixtures, verification and rendering
fixtures: ## Regenerate the deterministic test fixtures
	$(xtask) fixtures

verify: $(if $(filter plugins all,$(SUITE)),build-plugins) ## Run SUITE=headless, plugins, or all (default headless)
	$(xtask) verify --suite $(call quote,$(SUITE))

verify-headless: ## Run headless checks using existing fixtures
	$(xtask) verify --suite headless

verify-plugins: build-plugins ## Build and verify the CLAP and VST3 modules
	$(xtask) verify --suite plugins

verify-all: build-plugins ## Run both suites and report manual verification gates
	$(xtask) verify --suite all

render: ## Render PROJECT to WAV at OUTPUT in MODE=pattern or song
	$(xtask) render --project $(call quote,$(PROJECT)) --output $(call quote,$(OUTPUT)) --mode $(call quote,$(MODE))

bench-audio: ## Benchmark offline processing (SECONDS=60 by default)
	$(xtask) bench-audio --seconds $(call quote,$(SECONDS))

## CI and maintenance
ci: ## Run local CI checks, regenerate fixtures, verify and render a demo
	$(MAKE) fmt-check clippy test release
	$(MAKE) fixtures
	$(MAKE) verify-headless verify-plugins render

audit: ## Check advisories, licenses, bans and sources (requires cargo-deny)
	$(CARGO) deny check licenses advisories bans sources

licenses: ## Generate the dependency license report (requires cargo-about)
	$(CARGO) about generate --workspace --locked --config about.toml --format json

package: ## Invoke the packaging stub (FORMAT=deb, msi, or portable)
	$(xtask) package --format $(call quote,$(FORMAT))

clean: ## Remove Cargo build artifacts and verification output
	$(CARGO) clean
