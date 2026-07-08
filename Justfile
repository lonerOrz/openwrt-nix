# ==============================================================================
# Configuration Variables
# ==============================================================================

# Allow overriding via ROUTER_HOST env var
host := env_var_or_default("ROUTER_HOST", "192.168.188.2")

# Latest OpenWrt version info
version := `curl --silent https://api.github.com/repos/openwrt/openwrt/releases/latest | jq -r .tag_name | sed 's/^v//'`
sysupgrade_url := "https://downloads.openwrt.org/releases/" + version + "/targets/mediatek/mt7622/openwrt-" + version + "-mediatek-mt7622-linksys_e8450-ubi-squashfs-sysupgrade.itb"

# SSH connection reuse
ssh_opts := "-o ControlMaster=auto -o ControlPath=/tmp/ssh-%r@%h:%p -o ControlPersist=5m"

# ==============================================================================
# Development & Testing Recipes
# ==============================================================================

# Evaluate the nix module configuration and render UCI commands
eval-config:
	nix run .#example

# Run local Rust binary against mock configuration files
test:
	cargo run -- test/test_uci.json > /dev/null
	cargo run -- test/test_interpolate.json test/mock_secrets > /dev/null
	cargo run -- test/test_interp2.json test/mock_secrets > /dev/null
	cargo run -- test/test_unclosed.json > /dev/null
	cargo run -- test/test_edge_cases.json > /dev/null
	@echo "🚀 All local configuration tests passed successfully!"

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

# ==============================================================================
# Router Deployment & Management
# ==============================================================================

# Execute a command on the router via SSH
ssh +command:
	ssh {{ssh_opts}} "root@{{host}}" "{{command}}"

# Dry-run: Preview UCI changes on the router without applying them
dry-run:
	@echo "🔍 Simulating configuration changes on root@{{host}}..."
	@(just eval-config | sed 's/uci commit/uci changes/' && echo "uci revert") | just ssh 'sh -s'

# Apply configuration to router (SSH keys, password, packages, UCI, tinc — all hermetic)
apply:
	nix run .#example -- "root@{{host}}"

# Upgrade router firmware
upgrade:
	wget "{{sysupgrade_url}}" -O openwrt.sysupgrade.itb
	ssh {{ssh_opts}} "root@{{host}}" "cat > /tmp/openwrt.sysupgrade.itb" < openwrt.sysupgrade.itb
	just ssh "sysupgrade -v /tmp/openwrt.sysupgrade.itb" || true
	while ! ping -c1 -W1 8.8.8.8; do sleep 2; done
	just apply
