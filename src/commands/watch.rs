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
        for line in linux_post_install_hints() {
            println!("{line}");
        }
    }
}

/// Linux post-install hint lines for `distill watch --install`.
///
/// Extracted as a pure function so the wording (in particular, the
/// `loginctl enable-linger` reminder) is unit-testable. Without lingering,
/// `systemd --user` timers don't fire while the user is logged out — the
/// scheduled scan would silently never run on a server or on a laptop after
/// reboot if the user hasn't logged in yet, and there is nothing in the
/// install path that would surface that fact otherwise.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn linux_post_install_hints() -> Vec<String> {
    vec![
        "  Logs:     journalctl --user -u distill.service".to_string(),
        "  Verify:   systemctl --user start distill.service && journalctl --user -u distill.service -n 50".to_string(),
        "  Persist:  loginctl enable-linger \"$USER\"   # so timers fire while logged out".to_string(),
        "  Note:     Terminal-only notifications surface via the shell hook (`distill notify --check`).".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linux_post_install_hints_mentions_loginctl_enable_linger() {
        // systemd --user timers do not fire while the user is logged out
        // unless lingering is enabled. The post-install hint output is the
        // only place a user would learn this from distill itself, so it must
        // surface the exact command they need to run.
        let hints = linux_post_install_hints();
        let joined = hints.join("\n");
        assert!(
            joined.contains("loginctl enable-linger"),
            "post-install hints must reference `loginctl enable-linger`, got:\n{joined}"
        );
    }

    #[test]
    fn test_linux_post_install_hints_keeps_journal_and_verify_lines() {
        // The journal log path and the verify command are how users confirm
        // that the timer fired correctly. Don't regress them while adding the
        // linger guidance.
        let hints = linux_post_install_hints();
        let joined = hints.join("\n");
        assert!(
            joined.contains("journalctl --user -u distill.service"),
            "hints must keep pointing at journalctl for log inspection"
        );
        assert!(
            joined.contains("systemctl --user start distill.service"),
            "hints must keep the manual verify command"
        );
    }
}
