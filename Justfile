# Target host and SSH port. Override via ROUTER_HOST / ROUTER_PORT.
# A non-default port is needed when the target is a podman container whose 22
# is published on a non-standard local port.
host := env_var_or_default("ROUTER_HOST", "192.168.188.2")
port := env_var_or_default("ROUTER_PORT", "22")

ssh_opts := "-o ControlMaster=auto -o ControlPath=/tmp/ssh-%C -o ControlPersist=5m"

# --- Nix layer -------------------------------------------------------------

# Compile the example config and print the UCI commands
compile:
	nix run .#example

# Nix-layer tests: module options -> generated command script.
test-nix:
	nix build --file tests/nix/test.nix --no-link --impure

# --- Rust tests ------------------------------------------------------------

# Unit tests
test-unit:
	cargo test --lib

# Integration tests. Needs Docker/Podman and access to the package feeds.
test-integration:
	nix develop --command cargo test --test integration_test -- --test-threads=1

# --- Quality ----------------------------------------------------------------

fmt:
	cargo fmt
	nix fmt

clippy:
	cargo clippy --all-targets -- -D warnings

clean:
	cargo clean

# --- Deploy -----------------------------------------------------------------

# Read-only diff between the local config and the live target.
dry-run:
	nix build .#example-json -o /tmp/nuci-example.json
	cargo run -- diff /tmp/nuci-example.json --target "root@{{host}}" --port {{port}}

# Deploy the example config.
apply:
	nix run .#example -- "root@{{host}}" --port {{port}}

# Upgrade the firmware, then re-apply the config.
upgrade:
	./scripts/upgrade.sh "{{host}}" "{{port}}" "{{ssh_opts}}"
	just apply

# With no arguments this spins up a throwaway apk container; pass a target and
# an identity to run against a real box:
#   just smoke-verbose root@192.168.1.1 ~/.ssh/id_ed25519
# Show per-stage banners and the remote command trace.
smoke-verbose *args:
	./examples/verbose-smoke.sh {{args}}

# --- Misc -------------------------------------------------------------------

# Run a command on the target over SSH.
ssh +command:
	ssh -p {{port}} {{ssh_opts}} "root@{{host}}" "{{command}}"

# Build the docs
docs *mode:
	{{ if mode == "serve" { "mdbook serve" } else { "mdbook build" } }}
