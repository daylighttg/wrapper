//! `wrapper sandbox-exec`: run a program chrooted into the Android rootfs.
//!
//! Replaces the old `wrapper-lite` (root) and `wrapper-lite-rootless`
//! launchers. It must run in a fresh, single-threaded process (it forks), so
//! the agent spawns it as a helper instead of calling it in-process:
//!
//! 1. `unshare(NEWNS | NEWPID [| NEWUSER])`; rootless maps our uid/gid to 0.
//! 2. Fork; the child is PID 1 of the new PID namespace.
//! 3. Child: bind `/dev/urandom` + `/dev/null`, mount `/proc`, chroot, exec.
//! 4. Parent forwards SIGINT/SIGTERM/SIGHUP and exits with the child's status.

use std::ffi::{CString, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicI32, Ordering};

#[derive(Debug, Clone)]
pub struct SandboxSpec {
    pub rootfs: PathBuf,
    /// Use the caller's real root privileges instead of a user namespace.
    pub privileged: bool,
    /// Program path inside the rootfs, e.g. `/system/bin/lite`.
    pub program: OsString,
    pub args: Vec<OsString>,
}

static CHILD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn forward_signal(sig: libc::c_int) {
    let pid = CHILD.load(Ordering::SeqCst);
    if pid > 0 {
        unsafe {
            libc::kill(pid, sig);
        }
    }
}

fn cstr(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::other("path contains NUL"))
}

fn check(ret: libc::c_int, what: &str) -> io::Result<()> {
    if ret == -1 {
        let e = io::Error::last_os_error();
        Err(io::Error::new(e.kind(), format!("{what}: {e}")))
    } else {
        Ok(())
    }
}

fn write_file(path: &str, content: &str) -> io::Result<()> {
    std::fs::write(path, content).map_err(|e| io::Error::new(e.kind(), format!("{path}: {e}")))
}

fn has_cap_sys_admin() -> bool {
    const CAP_SYS_ADMIN: u32 = 21;
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    status
        .lines()
        .find_map(|l| l.strip_prefix("CapEff:"))
        .and_then(|v| u64::from_str_radix(v.trim(), 16).ok())
        .is_some_and(|caps| caps & (1 << CAP_SYS_ADMIN) != 0)
}

fn bind_dev(rootfs: &Path, name: &str) -> io::Result<()> {
    let target = rootfs.join("dev").join(name);
    if !target.exists() {
        std::fs::File::create(&target)?;
    }
    let src = cstr(&Path::new("/dev").join(name))?;
    let dst = cstr(&target)?;
    check(
        unsafe { libc::mount(src.as_ptr(), dst.as_ptr(), ptr::null(), libc::MS_BIND, ptr::null()) },
        &format!("bind /dev/{name}"),
    )
}

/// Child side: build the root and exec. Only returns on error.
fn enter_and_exec(spec: &SandboxSpec) -> io::Error {
    let run = || -> io::Result<()> {
        unsafe {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
        }
        let rootfs = &spec.rootfs;
        std::fs::create_dir_all(rootfs.join("dev"))?;
        bind_dev(rootfs, "urandom")?;
        // Not required by lite, but some Android code paths open it.
        let _ = bind_dev(rootfs, "null");

        std::fs::create_dir_all(rootfs.join("proc"))?;
        let proc_dst = cstr(&rootfs.join("proc"))?;
        check(
            unsafe {
                libc::mount(c"proc".as_ptr(), proc_dst.as_ptr(), c"proc".as_ptr(), 0, ptr::null())
            },
            "mount proc",
        )?;

        let root = cstr(rootfs)?;
        check(unsafe { libc::chdir(root.as_ptr()) }, "chdir rootfs")?;
        check(unsafe { libc::chroot(c".".as_ptr()) }, "chroot")?;
        check(unsafe { libc::chdir(c"/".as_ptr()) }, "chdir /")?;

        let prog = CString::new(spec.program.as_bytes()).map_err(|_| io::Error::other("bad program"))?;
        unsafe {
            libc::chmod(prog.as_ptr(), 0o755);
            libc::chmod(c"/system/bin/linker64".as_ptr(), 0o755);
        }

        let mut argv_c = vec![prog.clone()];
        for a in &spec.args {
            argv_c.push(CString::new(a.as_bytes()).map_err(|_| io::Error::other("bad argument"))?);
        }
        let mut argv: Vec<*const libc::c_char> = argv_c.iter().map(|c| c.as_ptr()).collect();
        argv.push(ptr::null());

        extern "C" {
            static environ: *const *const libc::c_char;
        }
        unsafe {
            libc::execve(prog.as_ptr(), argv.as_ptr(), environ);
        }
        Err(io::Error::new(io::Error::last_os_error().kind(), format!("execve: {}", io::Error::last_os_error())))
    };
    match run() {
        Ok(()) => io::Error::other("execve returned"),
        Err(e) => e,
    }
}

/// Runs the sandbox; returns the exit code to use for this process.
pub fn run(spec: &SandboxSpec) -> io::Result<i32> {
    if !spec.rootfs.join("system/bin/linker64").exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not look like the wrapper rootfs", spec.rootfs.display()),
        ));
    }
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    let mut flags = libc::CLONE_NEWNS | libc::CLONE_NEWPID;
    if spec.privileged {
        if !has_cap_sys_admin() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "privileged sandbox needs root"));
        }
    } else {
        flags |= libc::CLONE_NEWUSER;
    }
    check(unsafe { libc::unshare(flags) }, "unshare")?;

    if !spec.privileged {
        write_file("/proc/self/uid_map", &format!("0 {uid} 1\n"))?;
        write_file("/proc/self/setgroups", "deny\n")?;
        write_file("/proc/self/gid_map", &format!("0 {gid} 1\n"))?;
    }
    // Keep our bind mounts out of the host namespace.
    check(
        unsafe {
            libc::mount(ptr::null(), c"/".as_ptr(), ptr::null(), libc::MS_REC | libc::MS_PRIVATE, ptr::null())
        },
        "make / private",
    )?;

    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = forward_signal as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, ptr::null_mut());
        }
    }

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        let err = enter_and_exec(spec);
        eprintln!("sandbox-exec: {err}");
        unsafe { libc::_exit(127) };
    }
    CHILD.store(pid, Ordering::SeqCst);

    let mut status = 0;
    loop {
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        if r == pid {
            break;
        }
        if r == -1 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    })
}
