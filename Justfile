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

# Apply configuration to router
apply:
	#!/usr/bin/env bash
	set -eux -o pipefail

	# Set root password
	password=$(sops -d --extract '["root_password"]' secrets.yml)
	echo -e "$password\n$password" | just ssh "passwd root"

	# Set root SSH keys
	just ssh "mkdir -p /etc/dropbear/ && umask 177 && cat > /etc/dropbear/authorized_keys" <<EOF
	ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKbBp2dH2X3dcU1zh+xW3ZsdYROKpJd3n13ssOP092qE joerg@turingmachine
	EOF

	# Apply UCI configuration (now outputs a safe shell script)
	just eval-config | just ssh 'sh -s'

	# Set up internet after firmware reset
	if ! just ssh "ip link | grep -q pppoe-wan"; then
		just ssh "/etc/init.d/network restart"
		while ! ping -c1 -W 1 8.8.8.8; do :; done
	fi

	# Setup tinc keys if needed
	just ssh "if [ ! -f /etc/tinc/retiolum/rsa_key.priv ]; then mkdir -p /etc/tinc/retiolum; tinc -n retiolum generate-keys; /etc/init.d/tinc start; fi"
	rsync -e ssh -ac /etc/tinc/retiolum/hosts "root@{{host}}:/etc/tinc/retiolum"

# Upgrade router firmware
upgrade:
	wget "{{sysupgrade_url}}" -O openwrt.sysupgrade.itb
	rsync -e ssh -ac openwrt.sysupgrade.itb "root@{{host}}:/tmp/openwrt.sysupgrade.itb"
	just ssh "sysupgrade -v /tmp/openwrt.sysupgrade.itb" || true
	while ! ping -c1 -W1 8.8.8.8; do sleep 2; done
	just apply
