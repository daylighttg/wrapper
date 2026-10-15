# Updating

```sh
wrapper update            # check, show what changes, ask, install
wrapper update --check    # only show what would change
wrapper update -y         # no question (scripts, cron)
wrapper update --version v3.0.1   # a specific release; allows downgrades
wrapper update --mirror   # download through gh-proxy.com (mainland China)
```

Stop `wrapper gui` / `wrapper agent` / QEMU first; on Linux the updater
refuses while anything runs from the install directory. The panel checks for
releases at start-up and shows "vX available" under the version; set
`WRAPPER_NO_UPDATE_CHECK=1` to turn that off.

Not updatable in place: the Android APK (install the new APK), Docker
(pull the new image) and cargo builds (`--force` overrides the last two).

## What gets updated

An install is made of up to four components. Only those whose content
changed are downloaded, and only those already installed are touched:

| component | where                                   | changes when            |
|-----------|-----------------------------------------|-------------------------|
| `guest`   | `qemu/vmlinuz-lite-qemu`, `qemu/lite-initramfs.cpio.gz` | every release (it contains the agent) |
| `qemu`    | `qemu/bin/`                             | QEMU is upgraded        |
| `rootfs`  | `rootfs/` (`rootfs/data` is always kept) | lite changes           |
| `bin`     | `wrapper` / `wrapper.exe`               | every release           |

`--only bin,guest` limits the update; `--force` reinstalls even unchanged
components. `qemu/data.img`, `rootfs/data/` and `wrapper-gui.json` are never
touched.

Everything is downloaded, checked against the release manifest, unpacked and
(for the binary) test-run before anything is replaced. Replacing is a series
of renames inside the install directory, undone in reverse if one fails. On
Windows the running `wrapper.exe` is moved to `.wrapper-update/` and removed
by the next `wrapper update`.

## How releases are made

The `release` job of `build-lite.yml` runs `scripts/release-manifest.py`. It
cuts the components out of the platform packages, publishes them as extra
assets (`wrapper-<platform>[.exe]`, `guest.tar.gz`, `qemu-<platform>.tar.gz`,
`rootfs-<platform>.tar.gz`) and writes `manifest.json`:

```json
{
  "schema": 1, "version": "3.1.0", "tag": "v3.1.0",
  "notes": "https://github.com/itouakirai/wrapper/releases/tag/v3.1.0",
  "platforms": {
    "linux-x86_64": {
      "bin":   { "asset": "wrapper-linux-x86_64", "size": 0, "sha256": "…", "digest": "…" },
      "guest": { "asset": "guest.tar.gz", "…": "…" }
    }
  }
}
```

`sha256` is of the downloaded file; `digest` is of the installed files,
computed by `wrapper update --digest <component> <path>`, the same code the
updater uses on the user's machine. The updater finds the manifest at
`https://github.com/<repo>/releases/latest/download/manifest.json` (no API
calls, no rate limit). `--repo` / `WRAPPER_UPDATE_REPO` take `owner/name` or
the URL of a mirror laid out like a releases page.

## Signing

The manifest is signed with Ed25519 (`manifest.json.sig`). Binaries built
with a key in `crates/host/update-key.pub` refuse manifests without a valid
signature. This matters most with `--mirror`, where a third party serves the
files. Without a key, releases are unsigned and only hashes are checked.

To set it up (once):

```sh
openssl genpkey -algorithm ed25519 -out update-key.pem
openssl pkey -in update-key.pem -pubout -outform DER | tail -c 32 | od -An -tx1 | tr -d ' \n'
```

1. Put the printed hex on its own line in `crates/host/update-key.pub` and commit.
2. Store the whole `update-key.pem` as the repository secret
   `UPDATE_SIGNING_KEY` (Settings → Secrets and variables → Actions), then
   keep the file offline or delete it.

The release job fails if the secret is missing or does not match the
committed key. **If you lose the key or it leaks**, put a new public key in
`update-key.pub` and replace the secret. Existing installs will then reject
new manifests, so their users must download one release by hand.
