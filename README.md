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
[nightly builds](https://nightly.link/itouakirai/wrapper/workflows/build-lite/lite?preview):

| Platform | Package |
|---|---|
| Windows x86_64 | `wrapper-windows-x86_64.zip` |
| macOS Apple Silicon | `wrapper-macos-aarch64.zip` |
| Linux x86_64 (QEMU or native) | `wrapper-linux-x86_64.zip` |
| Linux aarch64 | `wrapper-linux-aarch64.zip` |
| Android | `wrapper-android-apk` (APK), or `wrapper-android-aarch64.zip` for Termux |

1. Unzip and run `wrapper` (`wrapper.exe` on Windows). The panel opens at
   `http://127.0.0.1:12341`.
2. Click **Start**. The first QEMU boot creates and formats a data disk.
3. Open **Accounts → Add account**, enter your Apple ID and password, and the
   2FA code when asked. Add as many accounts as you like.
4. Point your client at the API endpoint shown on the dashboard
   (default `http://127.0.0.1:12340`).

To serve other machines, set **Settings → Network → Listen on** to
`0.0.0.0`. The management endpoints (`/login`, `/logout`, `/api`) always
require the management token shown in the panel.

Upgrading from the single-account version: an existing `qemu/data.img` (or
`rootfs/data`) keeps working; its account is migrated automatically and shows
up as `legacy-account` (log in again to refresh its name).

### Android

Install the APK. It runs `wrapper` with the bundled QEMU in a foreground
service (with wake and Wi-Fi locks) and shows the same panel. The API is
reachable at `http://127.0.0.1:12340`, or from your network when the listen
address is `0.0.0.0`. Stop it from the notification.

In Termux: `./wrapper gui --no-browser`, then open the printed URL.

### Linux without QEMU

On Linux x86_64 the agent can run lite directly in a user-namespace chroot
(no root needed; the kernel must allow unprivileged user namespaces). Choose
**Native** in the panel, or run headless:

```bash
./wrapper agent --host 0.0.0.0 --port 12340 --api-token <secret>   # uses ./rootfs
./wrapper login --token <secret>                                  # Apple ID, password, 2FA
```

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
User namespaces need `seccomp`/`apparmor` relaxed (as in `compose.yaml`) or
`--privileged`.

## Commands

| Command | Purpose |
|---|---|
| `wrapper` / `wrapper gui` | Web panel (`--host`, `--port 12341`, `--no-browser`, `--data-dir`, `--assets-dir`, `--rootfs`, `--access-token`) |
| `wrapper qemu` | Boot the agent in QEMU (`--host`, `--host-port`, `--memory`, `--smp`, `--accel kvm\|whpx\|hvf\|tcg`, `--qemu-bin`, `--disk-size`, `--api-token`, `--proxy`, `-- <agent args>`) |
| `wrapper agent` | Run the agent (Linux; `--runtime auto\|direct\|sandbox\|sandbox-privileged`, `--proxy`, `--api-token`, `--device-info`, `--instance-concurrency`, …) |
| `wrapper login` | Add or re-login an account on a running agent, with interactive 2FA |
| `wrapper accounts` | List accounts and their state |

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

When no account can serve a request the agent answers
`{"code":-1,"msg":"no available instance"}`. Failures that may be
account-specific are retried on another account.

Account management (token required):

| Endpoint | Method | Body |
|---|---|---|
| `/login` | POST | `{"username","password"}` → `code 2` + `data.loginId` when a 2FA code is needed; repeat with `"code"` |
| `/logout` | POST | `{"username"}` |
| `/api/...` | | full management API, see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#http-api) |

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
