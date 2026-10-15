//! Host side of wrapper: everything that runs on the user's machine rather
//! than next to lite: the QEMU launcher and the GUI server.

pub mod gui;
pub mod platform;
pub mod qemu;
pub mod update;

use std::path::{Path, PathBuf};

/// Directory of the running executable (where packages put `qemu/`, `rootfs/`).
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}
