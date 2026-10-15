# wrapper-lite

A multi-account HTTP wrapper for Apple Music decryption.

One installation runs any number of Apple Music accounts and serves them
behind a single wrapper-lite compatible endpoint (`/m3u8`, `/key`, `/lyrics`,
`/webplayback`, `/license`, `/status`). Requests are routed to an account
whose storefront can serve the item. The API is also compatible with
[wrapper-manager](https://github.com/WorldObservationLog/wrapper-manager)
(`POST /login`, `POST /logout`), so
[AppleMusicDecrypt](https://github.com/WorldObservationLog/AppleMusicDecrypt)
and other clients work unchanged.

Everything on the host is one Rust binary, `wrapper`, with a web panel. The
Apple-side `lite` process runs inside a small QEMU guest (any OS) or natively
on Linux x86_64. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the
design.

```
client ──HTTP──▶ wrapper agent ──▶ lite (account A, jp)
                     │          └─▶ lite (account B, us) …
                     └─ inside QEMU (Windows / macOS / Android / Linux) or native Linux
```

## Quick start

Download the package for your platform from the
[latest release](https://github.com/itouakirai/wrapper/releases/latest)
(builds of every commit on `lite` are on the
[nightly page](https://nightly.link/itouakirai/wrapper/workflows/build-lite/lite?preview)):

| Platform | Package |
|---|---|
| Windows x86_64 | `wrapper-<version>-windows-x86_64.zip` |
| macOS Apple Silicon | `wrapper-<version>-macos-aarch64.tar.gz` |
| Linux x86_64 (QEMU or native) | `wrapper-<version>-linux-x86_64.tar.gz` |
| Linux aarch64 | `wrapper-<version>-linux-aarch64.tar.gz` |
| Android | `wrapper-<version>-android.apk`, or `wrapper-<version>-android-aarch64.tar.gz` for Termux |

1. Extract and run `wrapper` (`wrapper.exe` on Windows). The panel opens at
   `http://127.0.0.1:12340`, the same address as the API.
2. Click **Start**. The first QEMU boot creates and formats a data disk.
3. Open **Accounts → Add account**, enter your Apple ID and password, and the
   2FA code when asked. Add as many accounts as you like.
4. Point your client at the API endpoint shown on the dashboard
   (default `http://127.0.0.1:12340`).

To serve other machines, set **Settings → Network → Listen on** to
`0.0.0.0` (it applies at once). The panel and the API share that port: on
this machine the panel opens directly, other machines sign in with the
management token shown in the panel (or open the `?token=` URL wrapper
prints). The management endpoints (`/login`, `/logout`, `/api`) always
require the token.

### Public mode

**Public mode** lets anyone who opens your address add their own Apple Music
account, so a group can pool accounts on one server. Turn it on under
**Public mode** in the panel (or `wrapper agent --public`):

* Visitors get a public page instead of the panel: service status, regions,
  request statistics per endpoint (`/m3u8`, `/key`, `/lyrics`, …) and per
  account, the account list with masked names (`jo***@g***.com`), and a
  login form with 2FA. Apple IDs, logs, settings and the token stay hidden.
* **Maximum active accounts** caps how many logged-in accounts the server
  runs (each is a lite process, about 150 MB), so a small machine does not run
  out of memory. Accounts you add yourself are not limited.
* A visitor who logs in gets a **logout key** (remembered by their browser).
  Logging out needs the Apple ID and that key, so nobody can remove someone
  else's account. An Apple ID that is already serving cannot be logged in
  again publicly until it is logged out.
* At most two public logins run at a time, and each address may start six
  per 30 minutes.

### Backup and migration

**Backup** in the panel downloads one `.tar.gz` with every account's login
session (tokens, Apple session database, device id), the public-mode settings
and the statistics. Restored accounts come up without logging in again.
Restoring **merges** (adds the backup's accounts) or **replaces** everything.

To move to a new server, start wrapper there and use **Backup → Directly from
another server** with the old address and token: the new server downloads
the backup itself. The same from the command line:

```bash
wrapper backup  --url http://old:12340 --token <token>             # -> wrapper-backup-<date>.tar.gz
wrapper restore --url http://new:12340 --token <token> wrapper-backup-*.tar.gz
wrapper restore --url http://new:12340 --token <token> --from http://old:12340 --from-token <old token>
```

A backup lets anyone use the accounts in it: keep it private.

Upgrading from the single-account version: an existing `qemu/data.img` (or
`rootfs/data`) keeps working; its account is migrated automatically and shows
up as `legacy-account` (log in again to refresh its name).

### Android

Install the APK. It runs `wrapper` with the bundled QEMU in a foreground
service (with wake and Wi-Fi locks) and shows the same panel. The API is
reachable at `http://127.0.0.1:12340`, or from your network when the listen
address is `0.0.0.0`. Stop it from the notification.

In Termux: `./wrapper gui --no-browser`, then open the printed URL.

The APK protects the panel with a token even on the device (other apps can
reach `127.0.0.1` too); the app opens it for you.

### Linux without QEMU

On Linux x86_64 the agent can run lite directly in a user-namespace chroot
(no root needed; the kernel must allow unprivileged user namespaces). Choose
**Native** in the panel, or run headless:

```bash
./wrapper agent --host 0.0.0.0 --port 12340 --api-token <secret>   # uses ./rootfs
./wrapper login --token <secret>                                  # Apple ID, password, 2FA
```

The headless agent serves the panel on its API port as well (without the
engine and host settings); sign in with the token. `--no-ui` turns it off.

### Headless QEMU

```bash
./wrapper qemu --host 0.0.0.0 --host-port 12340 --api-token <secret>
./wrapper login --token <secret>
./wrapper accounts --token <secret>
```

Ctrl+C stops the lite processes and powers the guest off cleanly.

### Docker

```bash
WRAPPER_API_TOKEN=<secret> docker compose up -d --build
docker compose exec wrapper /app/wrapper login --token <secret>
```

The container runs the native agent; account data lives in `./rootfs/data`.
The panel is at `http://<host>:12340`; set `WRAPPER_PUBLIC=true` in `.env`
for public mode.
User namespaces need `seccomp`/`apparmor` relaxed (as in `compose.yaml`) or
`--privileged`.

## Commands

| Command | Purpose |
|---|---|
| `wrapper` / `wrapper gui` | Web panel and API on one port (`--host`, `--port` override and save Settings → Network; `--no-browser`, `--data-dir`, `--assets-dir`, `--rootfs`, `--access-token`) |
| `wrapper qemu` | Boot the agent in QEMU (`--host`, `--host-port`, `--memory`, `--smp`, `--accel kvm\|whpx\|hvf\|tcg`, `--qemu-bin`, `--disk-size`, `--api-token`, `--proxy`, `-- <agent args>`) |
| `wrapper agent` | Run the agent (Linux; `--runtime auto\|direct\|sandbox\|sandbox-privileged`, `--proxy`, `--api-token`, `--device-info`, `--instance-concurrency`, `--public`, `--max-accounts`, `--no-ui`, …) |
| `wrapper login` | Add or re-login an account on a running agent, with interactive 2FA |
| `wrapper accounts` | List accounts and their state |
| `wrapper backup` | Download a backup of every account (`--url`, `--token`, `-o`) |
| `wrapper restore` | Restore a backup file, or migrate with `--from <url> --from-token`; `--replace`, `--settings` |
| `wrapper update` | Upgrade to the latest release, only downloading what changed (`--check`, `-y`, `--version <tag>`, `--only bin,guest,qemu,rootfs`, `--mirror`); see [docs/UPDATING.md](docs/UPDATING.md) |

`wrapper <command> --help` lists every option.

## HTTP API

All responses use:

```json
{"code":0,"msg":"SUCCESS","data":{...}}
```

| Endpoint | Method | Parameters |
|----------|--------|------------|
| `/m3u8` | GET | `adamId` |
| `/key` | GET | `adamId`, `uri` (required; the prefetch `skd://itunes.apple.com/P000000000/s1/e1` is rejected unless `adamId=0`) |
| `/lyrics` | GET | `adamId`, optional `language`, optional `syllable` (`1`=word-timed syllable-lyrics with translations/romanization, `0`=line-timed lyrics converted in-wrapper **and still carrying translations/romanization as whole-line text**), optional `script` (transliteration script, default `en-Latn`) |
| `/webplayback` | GET | `adamId` |
| `/license` | POST | JSON: `adamId`, `challenge`, `uri`, optional `drm-type` (`wv`=Widevine (default), `pr`=PlayReady) |
| `/status` | GET | `regions` (storefronts that can be served), `clientCount` (serving accounts), `ready`, `status`, `degraded`, `accounts` |
| `/health` | GET | Whether an account can really deliver a key (the agent requests the prefetch `/key` from each lite). Answers `503` when none can. Default: last known result per account, no Apple traffic. `?probe=1` probes live and needs the token when one is set; so does the per-account list. Suitable for a Docker/uptime check. |

When no account can serve a request the agent answers
`{"code":-1,"msg":"no available instance"}`. Failures that may be
account-specific are retried on another account.

Account management (token required):

| Endpoint | Method | Body |
|---|---|---|
| `/login` | POST | `{"username","password"}` → `code 2` + `data.loginId` when a 2FA code is needed; repeat with `"code"` |
| `/logout` | POST | `{"username"}` |
| `/api/...` | | full management API (accounts, settings, statistics, backup / restore), see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#http-api) |
| `/api/public/...` | | public mode (no token), see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#public-mode) |

## Build from source

```bash
# 1. lite (Android NDK r23b unpacked in the repo root) -> rootfs/system/bin/lite
cmake -S . -B build -DCMAKE_BUILD_TYPE=Release && cmake --build build -j

# 2. web panel -> web/dist (embedded into the host binary)
(cd web && npm ci && npm run build)

# 3. host binary -> target/release/wrapper
cargo build --release

# 4. QEMU guest -> qemu/vmlinuz-lite-qemu, qemu/lite-initramfs.cpio.gz
cargo build --release --target x86_64-unknown-linux-musl
apt-get download busybox-static && ./qemu/build.sh
```

The CI workflow (`.github/workflows/build-lite.yml`) also builds the Android
binary (`aarch64-linux-android`, NDK clang as linker) and the APK, which
bundles it as `libwrapper.so`.

## lite arguments

`lite` is normally started by the agent; these are its own options.

| Argument | Default | Description |
|----------|---------|-------------|
| `--login user:pass` | — | login, cache tokens, then exit |
| `--code-from-file` | off | read 2FA code from file |
| `--code-timeout` | `60` | seconds to wait for `2fa.txt` |
| `--host` | `127.0.0.1` | listen address |
| `--port` | `12340` | listen port |
| `--base-dir` | `data` | data directory |
| `--device-info` | auto | override the entire device-info string (see [Device info](#device-info)) |
| `--proxy` | — | proxy URL |
| `--debug` | off | debug logging; disables SSL verify in Release |
| `--log-level` | `info` | `debug`, `info`, `warn`, `error` |
| `--log-file` | — | log file path |
| `--token-refresh-interval` | `1800` | background refresh interval in seconds |

## Device info

`lite` identifies itself to Apple's servers with a device-info string of the
form:

```
ClientIdentifier/VersionIdentifier/PlatformIdentifier/ProductVersion/DeviceModel/BuildVersion/LocaleIdentifier/LanguageIdentifier/AndroidID
```

Example: `Music/5.0.2/Android/10/Pixel 8/7663314/en-US/en-US/<android-id>`.

The trailing `AndroidID` is derived automatically:

- During `--login`, it is the FNV-1a 64-bit hash of the account username and is
  persisted to `<base-dir>/ANDROID_ID`, so each account keeps a stable device
  identity across runs.
- In service mode (no username), the persisted `ANDROID_ID` is reused; if none
  exists, a built-in default is used.

Pass `--device-info <string>` (per account in the login dialog, or as the
default in the settings) to override the entire string explicitly.

## Build notes

- Release: SSL verification is enabled by default. Pass `--debug` to disable it temporarily.
- Debug: SSL verification is disabled by default.

## License

This project is licensed under the [MIT License](LICENSE).
