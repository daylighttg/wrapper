# Architecture

wrapper-lite is being restructured so that a single Rust binary, `wrapper`,
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
   `wrapper sandbox-exec` (this phase).
2. QEMU launcher in Rust; the guest init starts `wrapper agent`.
3. New web GUI (Vite + Svelte) served by `wrapper gui`.
4. CI, thin Android shell, removal of the C/C++/Go launchers.

## Crates

| Crate | Purpose |
|---|---|
| `crates/agent` (`wrapper-agent`) | Library: account registry, lite supervision, login/2FA, routing, HTTP server, Linux sandbox. |
| `crates/wrapper` | The `wrapper` binary (CLI). |

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
* Health check: `/status` every 30 s; region changes are persisted.
* FairPlay failures (`KDCanProcessCKC`) open a circuit breaker after 5 hits:
  the account is skipped for 5 minutes but keeps running.

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
requested language (HEAD probe with that account's music token).

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
| GET | `/api/logs` | `source`, `after`, `limit` | log entries |
| GET | `/api/events` | | SSE: `log`, `account`, `accountRemoved`, `login` |

### wrapper-manager compatibility

`POST /login {username, password[, code]}` → `code 0` (done), `code 2`
(`"2fa code require"`, `data.loginId`), `code -1` (failure, or
`"already login"`). `POST /logout {username}` removes the account.
Disable both with `--no-legacy-management`.
