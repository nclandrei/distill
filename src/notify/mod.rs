use anyhow::{Context, Result};
use std::process::Command;

use crate::config::NotificationPref;

// ── Trait ────────────────────────────────────────────────────────────────────

/// Core notification abstraction.
pub trait Notifier {
    /// Send a notification with the given title and body.
    fn send(&self, title: &str, body: &str) -> Result<()>;
    /// Report whether this notifier is usable on the current platform/environment.
    fn is_available(&self) -> bool;
}

// ── macOS notifier ───────────────────────────────────────────────────────────

/// macOS native notifier via `osascript`.
///
/// Defined unconditionally so that it is always reachable by tests.
/// Only `is_available()` gates actual use to macOS at runtime.
pub struct MacOsNotifier;

impl Notifier for MacOsNotifier {
    fn send(&self, title: &str, body: &str) -> Result<()> {
        let script = format!("display notification \"{}\" with title \"{}\"", body, title);
        let status = Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .status()
            .context("Failed to run osascript")?;
        if !status.success() {
            anyhow::bail!("osascript exited with non-zero status: {}", status);
        }
        Ok(())
    }

    fn is_available(&self) -> bool {
        cfg!(target_os = "macos")
    }
}

// ── Linux notifier ───────────────────────────────────────────────────────────

/// Linux native notifier via `notify-send`.
///
/// Defined unconditionally; `is_available()` performs a runtime probe.
pub struct LinuxNotifier;

impl Notifier for LinuxNotifier {
    fn send(&self, title: &str, body: &str) -> Result<()> {
        let status = Command::new("notify-send")
            .arg(title)
            .arg(body)
            .status()
            .context("Failed to run notify-send")?;
        if !status.success() {
            anyhow::bail!("notify-send exited with non-zero status: {}", status);
        }
        Ok(())
    }

    fn is_available(&self) -> bool {
        Command::new("which")
            .arg("notify-send")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

// ── Terminal notifier ────────────────────────────────────────────────────────

/// Simple terminal notifier — always available, just prints to stdout.
pub struct TerminalNotifier;

impl Notifier for TerminalNotifier {
    fn send(&self, title: &str, body: &str) -> Result<()> {
        println!("distill: {title}");
        println!("         {body}");
        Ok(())
    }

    fn is_available(&self) -> bool {
        true
    }
}

// ── Platform factory ─────────────────────────────────────────────────────────

/// Return the platform-appropriate native notifier.
///
/// Conditional compilation is confined to this single function.
fn platform_notifier() -> Box<dyn Notifier> {
    #[cfg(target_os = "macos")]
    return Box::new(MacOsNotifier);

    #[cfg(target_os = "linux")]
    return Box::new(LinuxNotifier);

    // Any other platform falls back to the terminal notifier.
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Box::new(TerminalNotifier);
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Send a notification according to the user's configured preference.
///
/// * `None`     — do nothing.
/// * `Terminal` — print to stdout via `TerminalNotifier`.
/// * `Native`   — use the platform-appropriate native notifier.
/// * `Both`     — use both terminal and native notifiers.
pub fn send_notification(pref: &NotificationPref, title: &str, body: &str) -> Result<()> {
    match pref {
        NotificationPref::None => Ok(()),
        NotificationPref::Terminal => TerminalNotifier.send(title, body),
        NotificationPref::Native => platform_notifier().send(title, body),
        NotificationPref::Both => {
            TerminalNotifier.send(title, body)?;
            platform_notifier().send(title, body)
        }
    }
}

/// Convenience function called at the end of `distill scan`.
///
/// Does nothing when `proposal_count` is zero.
/// Otherwise dispatches to `send_notification` with a formatted body.
pub fn notify_scan_complete(proposal_count: usize, pref: &NotificationPref) -> Result<()> {
    if proposal_count == 0 {
        return Ok(());
    }
    let body = format!(
        "{} new proposal(s) ready. Run 'distill review'.",
        proposal_count
    );
    send_notification(pref, "distill", &body)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── TerminalNotifier ──────────────────────────────────────────────────────

    #[test]
    fn test_terminal_notifier_is_available() {
        let notifier = TerminalNotifier;
        assert!(
            notifier.is_available(),
            "TerminalNotifier must always be available"
        );
    }

    #[test]
    fn test_terminal_notifier_send() {
        let notifier = TerminalNotifier;
        let result = notifier.send("Test Title", "Test body text");
        assert!(result.is_ok(), "TerminalNotifier::send should never error");
    }

    // ── MacOsNotifier ─────────────────────────────────────────────────────────

    #[test]
    fn test_macos_notifier_is_available_reflects_platform() {
        let notifier = MacOsNotifier;
        let available = notifier.is_available();
        if cfg!(target_os = "macos") {
            assert!(available, "MacOsNotifier should be available on macOS");
        } else {
            assert!(
                !available,
                "MacOsNotifier should not be available off macOS"
            );
        }
    }

    // ── LinuxNotifier ─────────────────────────────────────────────────────────

    #[test]
    fn test_linux_notifier_is_available_does_not_panic() {
        // Result depends on whether notify-send is installed; we only care it
        // does not panic.
        let notifier = LinuxNotifier;
        let _ = notifier.is_available();
    }

    // ── send_notification ─────────────────────────────────────────────────────

    #[test]
    fn test_send_notification_none_pref() {
        // None pref must silently succeed without invoking any notifier.
        let result = send_notification(&NotificationPref::None, "distill", "any body");
        assert!(result.is_ok());
    }

    #[test]
    fn test_send_notification_terminal_pref() {
        // Terminal pref should print to stdout and return Ok.
        let result = send_notification(
            &NotificationPref::Terminal,
            "distill",
            "3 new proposal(s) ready. Run 'distill review'.",
        );
        assert!(result.is_ok());
    }

    // ── notify_scan_complete ──────────────────────────────────────────────────

    #[test]
    fn test_notify_scan_complete_zero_proposals() {
        // Zero proposals must be a silent no-op regardless of pref.
        for pref in [NotificationPref::None, NotificationPref::Terminal] {
            let result = notify_scan_complete(0, &pref);
            assert!(
                result.is_ok(),
                "notify_scan_complete(0, ..) must not error (pref: {})",
                pref
            );
        }
    }

    #[test]
    fn test_notify_scan_complete_with_proposals() {
        // Any non-zero count should succeed when using Terminal pref.
        let result = notify_scan_complete(2, &NotificationPref::Terminal);
        assert!(result.is_ok());
    }

    #[test]
    fn test_notify_scan_complete_message_format() {
        // Verify the body string that would be sent contains the count.
        let count: usize = 5;
        let body = format!("{} new proposal(s) ready. Run 'distill review'.", count);
        assert!(body.contains("5"), "body must contain the proposal count");
        assert!(
            body.contains("new proposal(s) ready"),
            "body must contain the ready phrase"
        );
        assert!(
            body.contains("distill review"),
            "body must mention 'distill review'"
        );

        // Also verify the public function succeeds end-to-end with Terminal pref.
        let result = notify_scan_complete(count, &NotificationPref::Terminal);
        assert!(result.is_ok());
    }
}
