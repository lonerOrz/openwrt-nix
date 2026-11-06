host := env_var_or_default("ROUTER_HOST", "192.168.188.2")
# SSH port for the target (override when the target is a podman container whose
# 22 is published on a non-standard local port, e.g. ROUTER_PORT=22222).
port := env_var_or_default("ROUTER_PORT", "22")

ssh_opts := "-o ControlMaster=auto -o ControlPath=/tmp/ssh-%C -o ControlPersist=5m"

eval-config:
	nix run .#example

test-all:
	cargo test

# Integration tests require Docker/Podman
test-integration:
	cargo test --test integration_test -- --test-threads=1 --nocapture

test-unit:
	cargo test --lib
	cargo run -- compile tests/test_uci.json > /dev/null
	cargo run -- compile tests/test_edge_cases.json -s tests/test_secrets > /dev/null
	@echo "All local mock configuration tests passed!"

fmt:
	cargo fmt
	nix fmt || true

clippy:
	cargo clippy --all-targets -- -D warnings

clean:
	cargo clean

build-containers:
	podman build -q -t openwrt-test-opkg-env -f tests/Containerfile.opkg .
	podman build -q -t openwrt-test-apk-env -f tests/Containerfile.apk .

# Smoke test for `deploy --verbose`: without a TARGET arg it spins up a throwaway
# apk container to show per-stage banners vs. silent default output. Pass a target
# to run against a real box: just smoke-verbose root@192.168.1.1 --identity ~/.ssh/id_ed25519
smoke-verbose:
	./examples/verbose-smoke.sh

docs:
	mdbook build

docs-serve:
	mdbook serve

ssh +command:
	ssh -p {{port}} {{ssh_opts}} "root@{{host}}" "{{command}}"

dry-run:
	nix build .#example-json -o /tmp/nuci-example.json
	cargo run -- diff /tmp/nuci-example.json --target "root@{{host}}" --port {{port}}

# Applies SSH keys, password, packages, UCI, and tinc — all hermetic.
apply:
	nix run .#example -- "root@{{host}}" --port {{port}}

upgrade:
	./scripts/upgrade.sh "{{host}}" "{{port}}" "{{ssh_opts}}"
	just apply
