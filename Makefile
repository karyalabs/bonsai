.PHONY: run check test fmt

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
