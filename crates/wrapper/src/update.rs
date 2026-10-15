//! `wrapper update`: upgrade this install from GitHub releases.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::{Args, ValueEnum};
use wrapper_host::update::{self, Component, Install, Plan, Progress, Source};

#[derive(Args)]
pub struct UpdateArgs {
    /// Only show what an update would do.
    #[arg(long)]
    check: bool,
    /// Install this release tag (e.g. v3.0.1) instead of the latest; allows
    /// downgrades.
    #[arg(long, value_name = "TAG")]
    version: Option<String>,
    /// Do not ask for confirmation.
    #[arg(short, long)]
    yes: bool,
    /// Reinstall components that look up to date; update containers and
    /// cargo builds too.
    #[arg(long)]
    force: bool,
    /// Update only these components.
    #[arg(long, value_enum, value_delimiter = ',')]
    only: Vec<ComponentArg>,
    /// Install to update [default: the directory of this executable].
    #[arg(long)]
    dir: Option<PathBuf>,
    /// GitHub repository releases come from (`owner/name`, or the URL of a
    /// mirror laid out like a GitHub releases page).
    #[arg(long, env = "WRAPPER_UPDATE_REPO", default_value = update::DEFAULT_REPO)]
    repo: String,
    /// Download through gh-proxy.com (for mainland China).
    #[arg(long)]
    mirror: bool,
    /// HTTP or SOCKS proxy for the downloads.
    #[arg(long, env = "WRAPPER_PROXY")]
    proxy: Option<String>,
    /// Internal (release CI): print the content digest of an installed
    /// component and exit.
    #[arg(long, hide = true, num_args = 2, value_names = ["COMPONENT", "PATH"])]
    digest: Vec<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum ComponentArg {
    /// The wrapper executable.
    Bin,
    /// QEMU guest kernel and initramfs (contains the agent).
    Guest,
    /// QEMU itself (qemu/bin).
    Qemu,
    /// The native-mode rootfs (rootfs/data is always kept).
    Rootfs,
}

impl From<ComponentArg> for Component {
    fn from(c: ComponentArg) -> Self {
        match c {
            ComponentArg::Bin => Component::Bin,
            ComponentArg::Guest => Component::Guest,
            ComponentArg::Qemu => Component::Qemu,
            ComponentArg::Rootfs => Component::Rootfs,
        }
    }
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

pub async fn run(args: UpdateArgs) -> anyhow::Result<()> {
    if let [component, path] = args.digest.as_slice() {
        let c = Component::parse(component).with_context(|| format!("unknown component {component}"))?;
        println!("{}", update::digest(c, path.as_ref()).with_context(|| path.clone())?);
        return Ok(());
    }

    let install = match &args.dir {
        Some(d) => Install::at(d)?,
        None => Install::current()?,
    };
    if let Some(why) = install.refuse_reason(args.force) {
        bail!("{why}");
    }
    install.remove_leftovers();
    let src = Source { mirror: args.mirror, proxy: args.proxy.clone().filter(|p| !p.is_empty()), ..Source::new(&args.repo) };
    let current = env!("CARGO_PKG_VERSION");

    eprintln!("checking {} for {} ...", args.repo, args.version.as_deref().unwrap_or("the latest release"));
    let (manifest, signed) = src.manifest(args.version.as_deref()).await?;
    let newer = update::is_newer(&manifest.version, current);
    eprintln!(
        "installed: {current}   release: {}{}",
        manifest.version,
        if signed { "  (signature ok)" } else { "  (unsigned build: checking hashes only)" }
    );
    if args.version.is_none() && !newer && !args.force {
        eprintln!("wrapper is up to date");
        return Ok(());
    }
    if args.version.is_some() && update::is_newer(current, &manifest.version) {
        eprintln!("note: this is a downgrade");
    }

    let only: Vec<Component> = args.only.iter().map(|&c| c.into()).collect();
    let (inst, force) = (install.clone(), args.force);
    let plan = tokio::task::spawn_blocking(move || update::plan(&inst, manifest, &only, force)).await??;
    print_plan(&plan);
    if plan.steps.is_empty() {
        eprintln!("nothing to download: every installed component already matches {}", plan.manifest.tag);
        return Ok(());
    }
    if args.check {
        return Ok(());
    }

    let busy = update::processes_using(&install.root);
    if !busy.is_empty() {
        for (pid, exe) in &busy {
            eprintln!("  running: pid {pid} {}", exe.display());
        }
        bail!("stop wrapper (and QEMU) running from {} first", install.root.display());
    }
    if !args.yes && !confirm(&format!("download {} and update?", mb(plan.download_size())))? {
        bail!("cancelled");
    }

    let result = update::apply(&src, &install, &plan, &print_progress).await;
    eprintln!();
    result?;
    eprintln!("updated to {}", plan.manifest.version);
    if let Some(notes) = &plan.manifest.notes {
        eprintln!("release notes: {notes}");
    }
    Ok(())
}

fn print_plan(plan: &Plan) {
    for s in &plan.steps {
        eprintln!("  update  {:<7} {:>9}  {}", s.component.name(), mb(s.asset.size), s.asset.asset);
    }
    for (c, why) in &plan.skipped {
        eprintln!("  keep    {:<7} {why}", c.name());
    }
}

fn print_progress(p: Progress) {
    let line = match p {
        Progress::Download { component, done, total } => {
            let pct = (done * 100).checked_div(total).unwrap_or(100);
            format!("downloading {:<7} {:>3}%  {} / {}", component.name(), pct, mb(done), mb(total))
        }
        Progress::Verify(c) => format!("verifying   {}", c.name()),
        Progress::Install(c) => format!("installing  {}", c.name()),
    };
    eprint!("\r{line:<60}");
    let _ = std::io::stderr().flush();
}

fn confirm(question: &str) -> anyhow::Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("not a terminal; pass --yes to update without confirmation");
    }
    eprint!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}
