.PHONY: run check test fmt perf \
	release-verify release-verify-archive release-verify-artifacts \
	release-doctor release-check-credentials release-github-doctor \
	release-notarize release-draft release-dry-run release

run:
	cargo run -p bonsai

fmt:
	cargo fmt --all

test:
	cargo test --workspace --locked --features bonsai/runtime_shaders

check:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --locked --features bonsai/runtime_shaders -- -D warnings
	cargo test --workspace --locked --features bonsai/runtime_shaders

# Measures a generated 100k-commit repository against docs/performance.md.
perf:
	cargo test -p bonsai --release --locked --features bonsai/runtime_shaders --test perf -- --ignored --nocapture

# Release packaging (macOS): see docs/releasing.md.
release-verify:
	./script/package_release.sh --verify

release-verify-archive:
	./script/package_release.sh --verify-archive

release-verify-artifacts:
	./script/package_release.sh --verify-artifacts

release-doctor:
	./script/package_release.sh --doctor

release-check-credentials:
	./script/package_release.sh --check-credentials

release-github-doctor:
	./script/package_release.sh --github-doctor

release-notarize:
	./script/package_release.sh --notarize

release-draft:
	./script/create_github_draft_release.sh

release-dry-run:
	gh workflow run release.yml -f dry_run=true

# A real release runs by itself when a version bump reaches main; this
# retries one by hand.
release:
	gh workflow run release.yml -f dry_run=false
