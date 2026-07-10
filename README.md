# nuci (Nix-UCI)

Declarative configuration management for OpenWrt routers. Nix defines the config, Rust compiles it to UCI, and a hermetic shell script deploys it over SSH.

## Architecture

```
Nix Config (.nix)
       │
       ▼
  lib.evalModules ──► uci.json
                          │
SOPS secrets ──► age decrypt (local)
                          │
                          ▼
                    nuci (Rust)
                 validate → resolve → serialize
                          │
                          ▼
               uci -q batch <<'EOF'
               commit <config>
               EOF
                          │
                     SSH pipe ──► Target Router
```

**Pipeline:** `JSON → parse → validate_root → resolve_secrets → serialize_uci → serialize_opkg → shell`

## Features

- **`uci batch` blocks** — atomic writes per config file, minimizing fork/exec overhead on embedded devices
- **AST secret resolution** — `@placeholder@` interpolation happens in a dedicated pass before serialization; the serializer never sees secrets
- **UCI spec validation** — config/section/option names enforced as `[a-zA-Z0-9_]`, types allow `[a-zA-Z0-9_-]` (for `wifi-iface` etc.), null values blocked; fails fast at parse time
- **Deploy-time decryption** — SOPS files decrypted locally via `age`, only plaintext UCI batch crosses the SSH pipe; no private keys on the router
- **Rollback watchdog** — background process on the target restores `/etc/config` backup if connectivity isn't re-established within 60s
- **SSH key lockout prevention** — deployment script ensures the deployer's current key is always appended to `authorized_keys`
- **Package management** — opkg feeds, remote packages, and local `.ipk` transfer via SCP

## Getting Started

### Prerequisites

- [Nix](https://nixos.org/download.html) with flakes enabled (`experimental-features = nix-command flakes`)
- [Just](https://github.com/casey/just) task runner
- [age](https://github.com/FiloSottile/age) for SOPS encryption
- Target device: default `Justfile` targets Linksys E8450 (UBI); edit `sysupgrade_url` for other devices

### 1. Clone and enter the project

```bash
git clone https://github.com/lonerOrz/openwrt-nix.git
cd openwrt-nix
```

### 2. Set up secrets

```bash
age-keygen -o age.key
```

Create `.sops.yaml`:

```yaml
creation_rules:
  - path_regex: secrets\.enc\.json$
    age:
      - <YOUR_AGE_PUBLIC_KEY>
```

Create and encrypt your secrets file:

```bash
sops secrets.enc.json
# add keys like: wifi_password, root_password, tsig_key, etc.
```

### 3. Configure

Edit `example.nix` (or create your own config). Reference secrets with `@placeholder@`:

```nix
uci.settings.wireless.radio0.key = "@wifi_password@";
```

Set your router IP in the `Justfile` (`host` variable) or export `ROUTER_HOST`.

### 4. Deploy

```bash
just apply          # full deployment: SSH keys, password, UCI, packages
just dry-run        # preview changes without applying
just upgrade        # download + flash latest OpenWrt, then re-apply config
```

## Development

### Run all tests

```bash
just test-all
```

### Unit tests (59)

```bash
just test-unit      # cargo test + 5 mock JSON files through the binary
```

### Integration tests (10 phases)

```bash
just test-integration
```

Runs `test/run_integration.sh` against a Podman container (`openwrt/rootfs:latest`):

| Phase | What |
|-------|------|
| 1-2 | Build & start container |
| 3-5 | Wait for dropbear, inject SSH key, create SSH config |
| 6 | Generate temp age key, SOPS-encrypt mock secrets |
| 7 | Verify `nuci` command generation via `nix run .#test-deploy` |
| 8 | Deploy into container, verify UCI state with `uci get` |
| 9 | Verify JSON artifact structure |
| 10 | Watchdog rollback test: break config, verify auto-restore |

### Other commands

```bash
just fmt            # cargo fmt + nix fmt
just clippy         # cargo clippy -D warnings
just eval-config    # render UCI commands to stdout
just ssh <cmd>      # execute command on router
```

## Project Structure

```
├── src/main.rs              # Rust UCI compiler (~1200 LOC, 59 tests)
├── nix/
│   ├── default.nix          # writeUci: JSON generator + deployment script
│   ├── module-options.nix   # NixOS-style option declarations
│   └── nuci.nix             # Rust package build
├── flake.nix                # Build system entry
├── Justfile                 # Task runner
├── example.nix              # Example/real router config
├── test/
│   ├── run_integration.sh   # 10-phase E2E test
│   ├── Containerfile        # OpenWrt test container
│   └── test_config.nix      # Test fixture
└── Cargo.toml
```

## License

MIT
