# Local mirror for trying panamax end to end: only the crates in LOCK, no rustup.
LOCAL := .local/mirror
LOCAL_PORT := 8282
LOCK ?= Cargo.lock

local: local-sync local-serve

local-sync:
	@test -f $(LOCAL)/mirror.toml || cargo run -q -- init --ignore-rustup $(LOCAL)
	@sed -i.bak 's|^base_url = .*|base_url = "http://localhost:$(LOCAL_PORT)/crates"|' $(LOCAL)/mirror.toml && rm $(LOCAL)/mirror.toml.bak
	cargo run -- sync $(LOCAL) --cargo-lock $(LOCK)

local-serve:
	cargo run -- serve $(LOCAL) --port $(LOCAL_PORT)

# fetch the crates in LOCK through the running local mirror, with a throwaway cargo home.
# --config beats ~/.cargo/config.toml, which cargo also finds by walking up from here.
local-check:
	@rm -rf .local/cargo-home
	CARGO_HOME=$(abspath .local/cargo-home) cargo fetch --locked \
		--manifest-path $(dir $(abspath $(LOCK)))Cargo.toml \
		--config 'source.local.registry="sparse+http://localhost:$(LOCAL_PORT)/index/"' \
		--config 'source.crates-io.replace-with="local"'

dev:
	watchexec -q -c -w src -w templates -w static --exts rs,html,css,js --restart "cargo run -- serve $(LOCAL) --port $(LOCAL_PORT)"

check:
	cargo clippy --all-targets -- -D warnings
	cargo fmt --check
	cargo test

# tags the version in Cargo.toml and pushes it, which starts release.yml
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

release:
	@test "$$(git branch --show-current)" = master || (echo "not on master" && exit 1)
	@test -z "$$(git status --porcelain)" || (echo "working tree not clean" && exit 1)
	@git fetch -q origin master
	@test "$$(git rev-parse HEAD)" = "$$(git rev-parse origin/master)" || (echo "master is not in sync with origin, run git pull" && exit 1)
	@grep -A1 '^name = "panamax"' Cargo.lock | grep -q 'version = "$(VERSION)"' || (echo "Cargo.lock is not at $(VERSION), run cargo build" && exit 1)
	git tag -a v$(VERSION) -m v$(VERSION)
	git push origin v$(VERSION)
