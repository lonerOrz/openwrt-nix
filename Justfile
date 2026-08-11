# ==============================================================================
# Configuration Variables
# ==============================================================================

# Allow overriding via ROUTER_HOST env var
host := env_var_or_default("ROUTER_HOST", "192.168.188.2")

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
	cargo test --test integration_test -- --test-threads=1

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
	ssh {{ssh_opts}} "root@{{host}}" "{{command}}"

# Dry-run: Preview UCI changes on the router without applying them
dry-run:
	cargo run -- diff example.nix --target "root@{{host}}"

# Apply configuration to router (SSH keys, password, packages, UCI, tinc — all hermetic)
apply:
	nix run .#example -- "root@{{host}}"

# Upgrade router firmware
upgrade:
	@version=$$(curl --silent https://api.github.com/repos/openwrt/openwrt/releases/latest | jq -r .tag_name | sed 's/^v//') && \
	sysupgrade_url="https://downloads.openwrt.org/releases/$${version}/targets/mediatek/mt7622/openwrt-$${version}-mediatek-mt7622-linksys_e8450-ubi-squashfs-sysupgrade.itb" && \
	echo "Downloading OpenWrt $${version}..." && \
	wget "$${sysupgrade_url}" -O openwrt.sysupgrade.itb
	ssh {{ssh_opts}} "root@{{host}}" "cat > /tmp/openwrt.sysupgrade.itb" < openwrt.sysupgrade.itb
	just ssh "sysupgrade -v /tmp/openwrt.sysupgrade.itb" || true
	while ! ping -c1 -W1 8.8.8.8; do sleep 2; done
	just apply
