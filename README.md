# nuci

Declarative config management for OpenWrt. Define everything in Nix, compile it with Rust, deploy over SSH.

```
Nix ──► uci.json ──► nuci ──► SSH pipe ──► Router
                         │
                    validates, resolves
                    secrets, serializes
                    UCI batch commands
```

## Why?

- Web UI config = no version control, no reproducibility
- Ansible on a 128MB router = painful
- Running Nix on the router itself = impossible

**nuci** does all the heavy lifting locally. The router just runs a small shell script.

## How it works

1. You write Nix config with `@placeholder@` for secrets
2. `nuci compile` → validates, decrypts SOPS, outputs `uci batch` script
3. `nuci deploy` → SSHes the script to the router, with rollback safety

**Watchdog**: if the new config kills SSH, the router auto-restores from backup within 60s.

## Quick start

```bash
# Setup
age-keygen -o age.key
sops test/secrets.enc.json     # add wifi_password, root_password, etc.

# Edit config
vim example.nix                 # use @wifi_password@ for secrets

# Deploy
export ROUTER_HOST=192.168.1.1
just apply                      # full deploy
just dry-run                    # preview only
```

## CLI

```
nuci compile <json> [secrets_dir]   # JSON → UCI batch (stdout)
nuci deploy <json>                  # Deploy to router
  --target <user@host>              # SSH target
  --port <port>                     # SSH port (default: 22)
  --identity <key_file>             # SSH identity file
```

## Testing

```bash
just test-all      # unit + integration
just test-unit     # cargo test + mock JSON
```

Integration tests run real OpenWrt containers via Podman — no physical router needed.

| Test | Verifies |
|------|----------|
| `TestCommandGeneration` | UCI batch syntax (opkg + apk) |
| `TestDeployment` | Deploy + UCI state verification |
| `TestWatchdogRollback` | Auto-restore after config break |
| `TestNetworkFaultInjection` | Watchdog under packet loss / blackout |
| `TestAgentLockout` | SSH key lockout prevention |
| `TestRealDeploy` | End-to-end `nuci deploy` binary |

Each test run gets unique UUID-based container names and dynamic ports — run multiple suites in parallel safely.

## Structure

```
src/
├── main.rs          # CLI (clap), compile & deploy
├── deploy.rs        # SSH transport, watchdog, key lockout
├── generator.rs     # UCI batch serialization
├── validation.rs    # UCI spec validation
├── secrets.rs       # SOPS decryption + @placeholder@ resolution
├── models.rs        # JSON config models
├── helpers.rs       # Utilities
└── error.rs         # Error types

test/
├── integration_test.py      # 25 pytest tests
├── package-server.py        # .ipk/.apk builder
├── Containerfile            # OpenWrt sandbox
└── test_config.nix          # Test fixture

nix/
├── default.nix      # writeUci + deployment script
├── module-options.nix
└── nuci.nix         # Rust package build
```

## License

MIT
