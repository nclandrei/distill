use anyhow::Result;

use crate::config::Config;
use crate::schedule;

pub fn run(install: bool, uninstall: bool) -> Result<()> {
    let scheduler = schedule::create_scheduler_default();
    if install {
        let config = Config::load()?;
        scheduler.install(&config.scan_interval)?;
        println!("Scheduler installed.");
        print_post_install_hints(scheduler.as_ref());
    } else if uninstall {
        scheduler.uninstall()?;
        println!("Scheduler removed.");
    } else {
        let status = scheduler.status()?;
        println!("Scheduler status: {:?}", status);
    }
    Ok(())
}

/// Surface the manifest path, log paths, and how to fire the job manually so
/// users can verify the scheduler before the next calendar slot. Without this,
/// a misconfigured plist (stale Cellar path, denied perms, etc.) is invisible
/// until the next 09:00 trigger.
fn print_post_install_hints(scheduler: &dyn schedule::Scheduler) {
    let manifest = scheduler.plist_or_unit_path();
    println!("  Manifest: {}", manifest.display());

    #[cfg(target_os = "macos")]
    {
        let logs_dir = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".distill")
            .join("logs");
        println!(
            "  Logs:     {}/scheduled-run.log (stderr in scheduled-run.err.log)",
            logs_dir.display()
        );
        println!(
            "  Verify:   launchctl kickstart -k gui/$(id -u)/com.distill.agent && tail -n 20 {}/scheduled-run.log",
            logs_dir.display()
        );
        println!(
            "  Note:     Terminal-only notifications surface via the shell hook (`distill notify --check`),"
        );
        println!(
            "            because launchd has no terminal attached. Install the hook during onboarding."
        );
    }

    #[cfg(target_os = "linux")]
    {
        println!("  Logs:     journalctl --user -u distill.service");
        println!(
            "  Verify:   systemctl --user start distill.service && journalctl --user -u distill.service -n 50"
        );
        println!(
            "  Note:     Terminal-only notifications surface via the shell hook (`distill notify --check`)."
        );
    }
}
