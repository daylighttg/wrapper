# wrapper QEMU guest

The guest is a Linux kernel plus an initramfs containing the wrapper rootfs
(Android libraries + `lite`), busybox and the static `wrapper` binary. Its
`init` brings up networking and the data disk and runs:

```
/bin/wrapper agent --runtime direct --state-dir /data --host 0.0.0.0 --port 12340 <extra args>
```

The host side is `wrapper qemu` (or the GUI), which forwards one host port to
guest port 12340.

## Layout

```text
<exe dir>/
├── wrapper(.exe)                host binary
└── qemu/
    ├── vmlinuz-lite-qemu
    ├── lite-initramfs.cpio.gz
    ├── data.img                 created on first run, formatted by the guest
    └── bin/                     bundled QEMU + firmware (optional)
```

## Build the guest

```bash
apt-get download busybox-static
cargo build --release --target x86_64-unknown-linux-musl   # or set WRAPPER_BIN
./qemu/build.sh
```

## Host ↔ guest contract

| What | How |
|---|---|
| Agent arguments | fw_cfg `opt/wrapper/args` (one per line); fallback `wrapper_args_b64=` on the kernel command line |
| API | `hostfwd=tcp:<host>:<host-port>-:12340` |
| Persistent state | virtio disk `/dev/vda` → `/data` (ext2/4; formatted when it has no ext superblock) |
| DNS | QEMU user networking, `nameserver 10.0.2.3` |
| Shutdown | `POST /api/system/shutdown`; init syncs, unmounts and powers off when the agent exits |

## Acceleration

`--accel auto` picks KVM (Linux with `/dev/kvm`), WHPX (Windows), HVF (Intel
macOS) or TCG, and falls back to TCG when the chosen accelerator fails within
the first 20 seconds. Force one with `--accel kvm|whpx|hvf|tcg`.

## QEMU lookup order

1. `--qemu-bin` / `QEMU_BIN`
2. `qemu/bin/` (bundled)
3. `PATH`
4. well-known install directories
