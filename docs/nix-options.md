# Nix Module Options

## Index

1. [Core](#core)
2. [UCI Configuration](#uci-configuration)
3. [Packages](#packages)
4. [Files and Keys](#files-and-keys)
5. [Secrets and Raw Commands](#secrets-and-raw-commands)
6. [Library Functions](#library-functions)

## Core

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `uci.packageManager` | `enum [ "opkg" "apk" ]` | `"opkg"` | Package manager backend. Use apk on OpenWrt 25.12+, opkg on older releases. |
| `uci.watchdogTimeout` | `int` | `60` | Rollback watchdog timeout in seconds. |

## UCI Configuration

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `uci.settings` | `(pkgs.formats.json {}).type` | `{}` | UCI configuration (config → section → option). |

## Packages

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `uci.packages` | `listOf str` | `[]` | Packages to install; prefix `-` to remove (e.g. `-tcpdump`). |
| `uci.packageSources.opkgFeeds` | `listOf str` | `[]` | opkg feeds (`src/gz <name> <url>`), written to `/etc/opkg/customfeeds.conf`. |
| `uci.packageSources.apkRepositories` | `listOf str` | `[]` | apk repository URLs, written to `/etc/apk/repositories.d/customfeeds.list`. |
| `uci.packageSources.localPackages` | `listOf (either str path)` | `[]` | Local `.ipk` / `.apk` files, streamed to the target `/tmp/`. |

> `opkgFeeds` and `apkRepositories` are mutually exclusive and must match `packageManager`.

## Files and Keys

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `uci.files` | `listOf (submodule)` | `[]` | Custom files; see submodule below. |
| `uci.sshKeys` | `listOf str` | `[]` | Keys deployed to `/etc/dropbear/authorized_keys` (`0600`). |

### `uci.files.*`

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `path` | `str` | required | Absolute destination path on the target. |
| `content` | `nullOr str` | `null` | Text content; empty string creates a zero-byte file. Mutually exclusive with `base64`. |
| `base64` | `nullOr str` | `null` | Base64 binary content. Mutually exclusive with `content`. |
| `checksum` | `nullOr str` | `null` | Expected SHA256; skips the write if the target hash matches. |
| `executable` | `bool` | `false` | `true` sets `0755`, `false` sets `0644`. |

## Secrets and Raw Commands

| Option | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `uci.secrets.sops.files` | `listOf path` | `[]` | SOPS-encrypted files, decrypted in memory at compile time to fill `@name@` placeholders. |
| `uci.rawUci` | `listOf str` | `[]` | Raw UCI command lines; must start with `"uci "`. |

**`@name@` placeholder scope**: `settings` option values, `opkgFeeds` / `apkRepositories`, `rawUci`, `files.content`, `sshKeys`. Not supported in `packages`, `localPackages`, or `files.base64`.

## Library Functions (`nix/default.nix`)

### `writeUci configuration`

Returns `json` (a derivation producing `uci.json`) and `command` (a wrapper script running `nuci compile` / `nuci deploy`).

### `buildFirmware { configuration, profile, release ? null }`

Bakes compiled UCI directives into `/etc/uci-defaults/99-nuci-bootstrap` and inherits `settings`, `packages`, `packageSources`, `sshKeys`, `files`, `rawUci`.
