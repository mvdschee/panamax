dev:
	watchexec -q -c -w src -w templates -w static --exts rs,html,css,js --restart "cargo run -- serve ./.local/mirror"

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
