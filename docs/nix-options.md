# Nix Module Options Specification

Verified 1:1 against `nix/module-options.nix` and `nix/default.nix`.

## Options Reference

| Option                             | Type                          | Default  | Description / Output Mapping                                                                                                                                                  |
| :--------------------------------- | :---------------------------- | :------- | :---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `uci.packageManager`               | `enum [ "opkg" "apk" ]`       | `"opkg"` | Backend: `"apk"` for OpenWrt 25.12+ (apk became the default at 24.10); `"opkg"` for older releases still on opkg. Runtime discovery verifies the target actually provides it. |
| `uci.settings`                     | `(pkgs.formats.json {}).type` | `{}`     | UCI configuration attrset (`config → section → option`).                                                                                                                      |
| `uci.secrets.sops.files`           | `listOf path`                 | `[]`     | SOPS encrypted files decrypted in-memory at compile time.                                                                                                                     |
| `uci.packages`                     | `listOf str`                  | `[]`     | Packages to install. Prefix with `-` to remove (`-pkg`).                                                                                                                      |
| `uci.packageSources.opkgFeeds`      | `listOf str`                  | `[]`     | Opkg feeds (`src/gz <name> <url>`); written to `/etc/opkg/customfeeds.conf` on opkg targets.                       |
| `uci.packageSources.apkRepositories`| `listOf str`                  | `[]`     | Apk repository URLs; written to `/etc/apk/repositories.d/customfeeds.list` on apk targets.                          |
| `uci.packageSources.feeds`          | `listOf str`                  | `[]`     | **Deprecated.** Unified feed list. On opkg treated as opkg feeds; on apk treated as repository URLs. Prefer the typed fields above. |
| `uci.packageSources.localPackages` | `listOf (either str path)`    | `[]`     | Local `.ipk`/`.apk` paths, streamed via in-memory tar to `/tmp/`.                                                                                                             |
| `uci.sshKeys`                      | `listOf str`                  | `[]`     | Public keys deployed to `/etc/dropbear/authorized_keys` (`0600`).                                                                                                             |
| `uci.watchdogTimeout`              | `int`                         | `60`     | Rollback watchdog timeout in seconds.                                                                                                                                         |
| `uci.rawUci`                       | `listOf str`                  | `[]`     | Raw UCI lines (must start with `"uci "`). Auto-touches missing `/etc/config/<file>`.                                                                                          |
| `uci.files`                        | `listOf (submodule)`          | `[]`     | Custom file payloads (spec below).                                                                                                                                            |

### `uci.files.*` Submodule Options

| Option       | Type         | Default      | Description                                                                            |
| :----------- | :----------- | :----------- | :------------------------------------------------------------------------------------- |
| `path`       | `str`        | _(required)_ | Absolute destination path on target.                                                   |
| `content`    | `nullOr str` | `null`       | Text content (written via POSIX `cat` heredoc). Empty string creates a zero-byte file. |
| `base64`     | `nullOr str` | `null`       | Base64 binary payload (decoded via `base64 -d`). Mutually exclusive with `content`.    |
| `checksum`   | `nullOr str` | `null`       | Expected SHA256 hex. Skips write if target hash matches.                               |
| `executable` | `bool`       | `false`      | File mode (`true` → `0755`, `false` → `0644`).                                         |

## Nix Library Functions (`nix/default.nix`)

### `writeUci configuration`

Evaluates Nix configuration module and returns:

- `json`: Derivation generating `uci.json`.
- `command`: Wrapper script executing `nuci compile` or `nuci deploy` (forwards `$@` flags).

### `buildFirmware { configuration, profile, release ? null }`

Combines `nuci` with `nix-openwrt-imagebuilder`:

- Bakes compiled UCI directives into `/etc/uci-defaults/99-nuci-bootstrap`.
- Inherits `settings`, `packages`, `packageSources`, `sshKeys`, `files`, and `rawUci`.
