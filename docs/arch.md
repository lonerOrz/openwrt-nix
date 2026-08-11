# Architecture

## Project Layout

```text
src/
  config/   models.rs, uci_key.rs, validation.rs
  compile/  generator.rs, pipeline.rs, secrets.rs
  target/   deploy.rs, diff.rs
  utils/    error.rs, helpers.rs
```

Single compilation seam: `compile::pipeline::compile_config`.

## UCI Idempotency Strategy

- **Named Section** (`network.lan`): Wiped via `uci delete`, rebuilt via `uci set`. Removed Nix options are deleted on target.
- **Anonymous List** (`wireless.@wifi-iface[0]`): Wiped via `while uci -q delete config.@type[0]; do :; done`, re-added sequentially.
- **Array Diff**: Joined via `\u{1f}` (Unit Separator) control character, making element reordering diff-neutral.
- **Libuci Protection**: Automatically emits `touch /etc/config/<cfg>` before `uci batch` to avoid silent file creation failures.

## Anti-Brick Rollback System

| Layer                    | Trigger                        | Action                                                                                                             |
| :----------------------- | :----------------------------- | :----------------------------------------------------------------------------------------------------------------- |
| **Layer A (In-Session)** | Network loss / SSH timeout     | Background watchdog (`trap '' HUP; sleep 60`). Killed on SSH handshake success.                                    |
| **Layer B (Boot-Time)**  | Power loss / Reboot mid-deploy | Init script `S15nuci_rollback` restores `/etc/config` from `/etc/.uci-rollback-backup` on boot, then self-deletes. |

## Async Detached Reload

Avoids SSH exit status 255 (TCP Reset on interface/sshd restart) by running reloads in a background subshell:

```sh
( sleep 1; <reload_commands> ) >/dev/null 2>&1 &
```

The script exits 0 immediately, closing SSH cleanly before services restart.

## Target Execution Requirements

- **Text Files**: POSIX `cat > path <<'NUCI_FILE_{i}_EOF'` (quoted delimiter prevents shell expansion, zero base64 dependency).
- **Binary Files**: Base64 decoded via `echo '<b64>' | base64 -d > path`.
- **Root Password**: `chpasswd <<'CHPWD'` or `passwd` fallback (POSIX heredoc).
- **Local Packages**: Streamed via pure Rust `tar::Builder` into target `/tmp/` without host disk files.
