# Architecture

wrapper-lite was restructured so that a single Rust binary, `wrapper`,
replaces the old C launchers (`wrapper-lite.c`, `wrapper-lite-rootless.c`),
the C++ QEMU launcher (`wrapper-lite-qemu.cpp`) and the Go GUI, and so that one
installation can serve **multiple Apple Music accounts** (the feature set of
[wrapper-manager](https://github.com/WorldObservationLog/wrapper-manager)).

The Android-side `lite` binary (`lite/*.cpp`) is unchanged in role: it links
Apple's Android libraries and therefore stays C++. Its HTTP API (`/m3u8`,
`/key`, `/lyrics`, `/webplayback`, `/license`, `/status`) is the contract the
Rust side builds on.

```
host (Windows / macOS / Linux / Android)        QEMU guest, or Linux natively
┌──────────────────────────────┐               ┌───────────────────────────────────┐
│ wrapper gui        (phase 3) │   hostfwd     │ wrapper agent                     │
│  web panel, QEMU launcher,   │──────────────▶│  aggregated lite API + /api       │
│  package download            │  one port     │  per-account lite processes       │
└──────────────────────────────┘               │  ├─ lite --base-dir accounts/<id1>│
                                               │  └─ lite --base-dir accounts/<id2>│
                                               └───────────────────────────────────┘
```

## Delivery phases

1. **Agent core + native Linux** — `crates/agent`, `wrapper agent`,
   `wrapper sandbox-exec`.
2. **QEMU launcher in Rust** — `crates/host`, `wrapper qemu`; the guest init
   starts `wrapper agent`.
3. **Web GUI** — `wrapper gui` (default command) serves the Vite + Svelte
   panel from `web/`, runs the engine and proxies the agent API.
4. **CI, Android, cleanup** — new workflow (Rust checks, web, lite, guest
   with a native-sandbox smoke test, desktop/Android packages, APK), thin
   Android shell, Docker on the native agent, old launchers and GUI removed.

## Crates

| Crate | Purpose |
|---|---|
| `crates/agent` (`wrapper-agent`) | Library: account registry, lite supervision, login/2FA, routing, HTTP server, Linux sandbox. |
| `crates/host` (`wrapper-host`) | Library: QEMU launcher (asset lookup, acceleration fallback, graceful shutdown) and the GUI server. |
| `web/` | The panel (Svelte 5, Vite). `crates/host/build.rs` embeds `web/dist` into the binary. |
| `crates/wrapper` | The `wrapper` binary: `agent`, `qemu`, `login`, `accounts`, internal `sandbox-exec`. |

The guest contract (fw_cfg arguments, data disk, shutdown) is described in
[qemu/README.md](../qemu/README.md).

## The agent

`wrapper agent` runs where lite can run:

| Runtime | Where | How lite is started |
|---|---|---|
| `direct` | QEMU guest (root fs = wrapper rootfs) | exec `/system/bin/lite` |
| `sandbox` | Linux host, no root | `wrapper sandbox-exec`: user + mount + PID namespace, chroot into `rootfs/` |
| `sandbox-privileged` | Linux host as root | same, without a user namespace |

`--runtime auto` picks `direct` when `/system/bin/lite` exists, else `sandbox`.

### State layout

```
<state>/                    guest: /data   native: <rootfs>/data
├── accounts.json           [{id, username, region, enabled, deviceInfo?, createdAt}]
├── data/                   Android app data shared by all instances (ANDROID_DATA)
└── accounts/<id>/          lite --base-dir of one account
    ├── mpl_db/
    ├── MUSIC_TOKEN, DEV_TOKEN, STOREFRONT_ID, token_cache.json, ANDROID_ID
    ├── .login-args         credentials for a running login (0600, removed after)
    └── 2fa.txt             only while a login waits for its code
```

`<id>` is the UUIDv5 (nil namespace) of the lower-cased, trimmed username.
A pre-multi-account install (tokens directly in `<state>/`) is moved into
`accounts/<id>/` as `legacy-account` on first start.

### Instance lifecycle

```
Stopped ──start──▶ Starting ──/status has a region──▶ Running
   ▲                  │  exit / timeout                 │ exit / 3 failed health checks
   │                  ▼                                 ▼
   └──stop──────── Backoff (2s … 120s, doubling) ◀──────┘

Running ──"No Active Subscription" / session ended──▶ NeedsLogin
```

* One lite process per account, on a free `127.0.0.1` port.
* At most `--startup-parallelism` (2) instances start at once; lite under TCG is
  CPU heavy while starting.
* Liveness: `/status` every 30 s; region changes are persisted. This only
  proves lite's HTTP server is up.
* FairPlay failures (log lines containing `Fairplay error` or
  `KDCanProcessCKC`, as in wrapper-manager) open a circuit breaker after 5
  hits: the account is skipped for 5 minutes but keeps running. Any
  successful `/key` (or probe), and every fresh lite process, closes it again
  and resets the count.

### Key-path health (`crates/agent/src/health.rs`)

Those errors are not recoverable inside lite; restarting is. Log lines are
only a *suspicion*, a `/key?adamId=0` request for the prefetch key is the
*judge*, and a separate reachability check of Apple's key server (through
lite's proxy) separates "the network is down" from "lite is broken":

```
FairPlay log line ─┐
every 10 min,      ├─▶ probe /key (adamId=0)
unless a real      │      ok ──────────────▶ healthy, circuit closed
/key succeeded ────┘      failed
                           ├─ Apple key server unreachable ─▶ network problem, no restart
                           └─ reachable, failed twice (15 s apart) ─▶ restart lite
                                 └─ already restarted 3× in the last hour ─▶ circuit breaker instead
```

* A health restart goes through the normal backoff (`Backoff`, 2 s doubling).
* `--health-probe-interval <s>` (default 600, `0` = only on log suspicion and
  `/health?probe=1`).
* `GET /health` reports the result per account (`verdict`: `ok`, `busy`,
  `liteDown`, `networkDown`, `broken`); the per-account list needs the token
  when one is set (it names the Apple accounts). The same `health` object is
  part of every account in `/api/accounts`.
* A freshly started lite begins with a clean record (no verdict, no counted
  failures); only the restart budget carries over.
* Probes take a normal request slot, so they never exceed
  `--instance-concurrency`.

### Login

1. `POST /api/accounts/login {username, password, deviceInfo?}` starts
   `lite --login` with credentials passed through `LITE_ARGS_FILE` (not argv).
   The call returns when the login settles, or as soon as lite waits for a
   code (`state: "need2fa"`).
2. `POST /api/logins/<id>/code {code}` writes `2fa.txt` and waits again.
3. On success the account is registered and its service instance started.

Logging in to an existing account stops its instance first (they share the
base dir) and restarts it if the login fails. lite waits `--code-timeout`
(default 300 s) for the code.

### Routing

For `/m3u8`, `/key`, `/webplayback`, `/license`:

1. Candidates = `Running` accounts whose circuit is closed.
2. If they span several storefronts, keep those whose catalog has the item
   (`amp-api` probe per region, songs then music videos, cached 24 h). If every
   probe errors (no Apple access), keep all.
3. Least in-flight first; at most 2 concurrent requests per instance.
4. Retry on another candidate (max 3 attempts, 45 s budget) unless the lite
   code is 400, or 404 for `/lyrics`.

`/lyrics` first prefers an account whose storefront actually has lyrics in the
requested language (HEAD probe with that account's music token). Storefronts
that list the language (Apple's `/v1/storefronts`, `supportedLanguageTags`) are
probed first, because another store may have the lyrics but not their
translations; the rest only if none of those has them.

## HTTP API

Responses use the lite envelope `{"code", "msg", "data"}`. Agent-generated
failures use `code: -1`.

### Resource API (open)

Same parameters as lite; see the README. `GET /status` returns
`{status, regions, clientCount, ready, degraded, accounts}` (wrapper-manager
shape plus extras).

### Management API (`--api-token` protects these when set)

Send `Authorization: Bearer <token>`; `?token=` also works (for SSE).

| Method | Path | Body / query | Result |
|---|---|---|---|
| GET | `/api/overview` | | version, ready, runtime, accounts, logins |
| GET | `/api/accounts` | | `AccountView[]` |
| POST | `/api/accounts/login` | `{username, password, deviceInfo?}` | `LoginView` |
| POST | `/api/logins/{id}/code` | `{code}` | `LoginView` |
| GET / DELETE | `/api/logins/{id}` | | view / cancel |
| GET | `/api/logins` | | `LoginView[]` |
| POST | `/api/accounts/{id}/start` · `/stop` · `/restart` | | `AccountView` |
| DELETE | `/api/accounts/{id}` | | remove account and its data |
| POST | `/api/health/probe` | | probe every running account now (like `/health?probe=1`), returns the `/health` report |
| GET | `/api/logs` | `source`, `after`, `limit` | log entries |
| POST | `/api/system/shutdown` | | stop all instances and exit (the guest then powers off) |
| GET | `/api/events` | | SSE: `log`, `account`, `accountRemoved`, `login` |

## The GUI (`wrapper gui`)

```
browser ──▶ wrapper gui (127.0.0.1:12341)
             ├─ /api/host/*     settings, engine start/stop, console, package download
             ├─ /api/agent/*    ─▶ agent /api/*  (adds the management token)
             ├─ /api/public/*   ─▶ agent resource API (API tester)
             └─ /*              embedded single-page app
```

* Engines: **QEMU** (any OS; `crates/host/src/qemu.rs`) or **native** (Linux
  x86_64; spawns `wrapper agent --runtime sandbox`).
* Settings live in `<data dir>/wrapper-gui.json` (default: next to the
  binary). A random management token is generated on first run and passed to
  the agent, so `/api` is never open on the public port.
* Listening on a non-loopback address requires an access token (generated
  and printed as `?token=…`, then kept in a cookie).
* Children die with the GUI: `PR_SET_PDEATHSIG` on Linux, a kill-on-close job
  object on Windows; closing the console window triggers a graceful stop.

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/host/info` | platform, component status, package URL |
| GET / PUT | `/api/host/config` | settings (PUT merges fields; an empty `apiToken` regenerates it) |
| GET | `/api/host/status` | engine state |
| POST | `/api/host/start` · `/stop` · `/restart` | control the engine |
| GET | `/api/host/console` | QEMU / agent console lines |
| GET | `/api/host/events` | SSE: `console`, `status`, `download` |
| POST | `/api/host/package/download` | download and install the platform package |

## Android

The APK is a thin Kotlin shell around the same binary, built for
`aarch64-linux-android` and shipped as `jniLibs/arm64-v8a/libwrapper.so` (so
it is extracted to the executable native library dir):

* `WrapperService` (foreground service, wake + Wi-Fi locks) unpacks the
  bundled Termux QEMU and guest files to `filesDir/qemu` once per APK version,
  then runs `libwrapper.so gui --data-dir filesDir --assets-dir filesDir/qemu
  --access-token <random>` and restarts it if it dies.
* `MainActivity` is a WebView on the panel URL that wrapper prints.
* `targetSdk` stays 28 so QEMU can be executed from `filesDir`.

## Packages

| Artifact | Contents |
|---|---|
| `wrapper-<os>-<arch>` | `wrapper(.exe)`, `qemu/` (kernel, initramfs, bundled QEMU in `qemu/bin`); linux-x86_64 also `rootfs/` for the native engine |
| `wrapper-android-aarch64` | Android `wrapper` + Termux QEMU, for Termux |
| `wrapper-android-apk` | the APK |

The GUI's "download package" fetches `wrapper-<os>-<arch>.zip` and installs
it next to `qemu/`, skipping the running executable, `qemu/data.img` and
`rootfs/data/`.

### wrapper-manager compatibility

`POST /login {username, password[, code]}` → `code 0` (done), `code 2`
(`"2fa code require"`, `data.loginId`), `code -1` (failure, or
`"already login"`). `POST /logout {username}` or `{id}` (the account UUID;
`id` wins when both are given) removes the account.
Disable both with `--no-legacy-management`.
