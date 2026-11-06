# ==============================================================================
# Configuration Variables
# ==============================================================================

# Allow overriding via ROUTER_HOST env var
host := env_var_or_default("ROUTER_HOST", "192.168.188.2")
# SSH port for the target (override when the target is a podman container whose
# 22 is published on a non-standard local port, e.g. ROUTER_PORT=22222).
port := env_var_or_default("ROUTER_PORT", "22")

# SSH connection reuse
ssh_opts := "-o ControlMaster=auto -o ControlPath=/tmp/ssh-%C -o ControlPersist=5m"

# ==============================================================================
# Development & Testing Recipes
# ==============================================================================

# Evaluate the nix module configuration and render UCI commands
eval-config:
	nix run .#example

# Run all Rust tests (unit + integration)
test-all:
	cargo test

# Run Rust integration tests (requires Docker/Podman)
test-integration:
	cargo test --test integration_test -- --test-threads=1 --nocapture

# Run Rust unit tests only
test-unit:
	cargo test --lib
	cargo run -- compile tests/test_uci.json > /dev/null
	cargo run -- compile tests/test_edge_cases.json -s tests/test_secrets > /dev/null
	@echo "All local mock configuration tests passed!"

# Format both Rust and Nix files
fmt:
	cargo fmt
	nix fmt || true

# Run Rust linter
clippy:
	cargo clippy --all-targets -- -D warnings

# Clean rust compilation targets
clean:
	cargo clean

# Rebuild test container images
build-containers:
	podman build -q -t openwrt-test-opkg-env -f tests/Containerfile.opkg .
	podman build -q -t openwrt-test-apk-env -f tests/Containerfile.apk .

# Smoke test `deploy --verbose`: runs examples/verbose-smoke.sh, which builds the
# binary and (if no TARGET arg) spins up a throwaway apk container to show the
# per-stage banners vs. silent default output. Pass a target to use a real box:
#   just smoke-verbose root@192.168.1.1 --identity ~/.ssh/id_ed25519
smoke-verbose:
	./examples/verbose-smoke.sh
# Build the documentation site (mdBook) into ./book
docs:
	mdbook build

# Serve the documentation site locally with live reload
docs-serve:
	mdbook serve

# ==============================================================================
# Router Deployment & Management
# ==============================================================================

# Execute a command on the router via SSH
ssh +command:
	ssh -p {{port}} {{ssh_opts}} "root@{{host}}" "{{command}}"

# Dry-run: Preview UCI changes on the router without applying them
dry-run:
	nix build .#example-json -o /tmp/nuci-example.json
	cargo run -- diff /tmp/nuci-example.json --target "root@{{host}}" --port {{port}}

# Apply configuration to router (SSH keys, password, packages, UCI, tinc — all hermetic)
apply:
	nix run .#example -- "root@{{host}}" --port {{port}}

# Upgrade router firmware
upgrade:
	./scripts/upgrade.sh "{{host}}" "{{port}}" "{{ssh_opts}}"
	just apply
