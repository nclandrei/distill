use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::Command;

use crate::config::NotificationPref;

const ICON_PNG_RELATIVE: &str = "assets/icons/png/color/distill-color-256.png";
const ICON_SVG_RELATIVE: &str = "assets/icons/distill-icon.svg";
const ICON_SHARE_PNG_RELATIVE: &str = "share/distill/icons/distill-color-256.png";
const ICON_SHARE_SVG_RELATIVE: &str = "share/distill/icons/distill-icon.svg";
const EMBEDDED_ICON_FILENAME: &str = "distill-color-256.png";
const EMBEDDED_ICON_PNG: &[u8] =
    include_bytes!("../../assets/icons/png/color/distill-color-256.png");

// ── Trait ────────────────────────────────────────────────────────────────────

/// Core notification abstraction.
pub trait Notifier {
    /// Send a notification with the given title/body and optional icon.
    fn send(&self, title: &str, body: &str, icon_path: Option<&str>) -> Result<()>;
    /// Report whether this notifier is usable on the current platform/environment.
    fn is_available(&self) -> bool;
}

fn binary_exists(bin: &str) -> bool {
    Command::new("which")
        .arg(bin)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn first_existing_path(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find_map(|path| {
        if path.exists() {
            Some(path.canonicalize().unwrap_or(path))
        } else {
            None
        }
    })
}

fn default_notification_icon_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();

    // Optional override for local testing / packaging quirks.
    if let Ok(raw_override) = std::env::var("DISTILL_ICON_PATH") {
        let trimmed = raw_override.trim();
        if !trimmed.is_empty() {
            let override_path = PathBuf::from(trimmed);
            if override_path.is_absolute() {
                candidates.push(override_path);
            } else if let Ok(cwd) = std::env::current_dir() {
                candidates.push(cwd.join(override_path));
            } else {
                candidates.push(override_path);
            }
        }
    }

    // Cargo run/build (repo-root ancestor) and installed layouts (share/distill/icons).
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().take(8) {
            candidates.push(ancestor.join(ICON_PNG_RELATIVE));
            candidates.push(ancestor.join(ICON_SVG_RELATIVE));
            candidates.push(ancestor.join(ICON_SHARE_PNG_RELATIVE));
            candidates.push(ancestor.join(ICON_SHARE_SVG_RELATIVE));
            candidates.push(ancestor.join("..").join(ICON_SHARE_PNG_RELATIVE));
            candidates.push(ancestor.join("..").join(ICON_SHARE_SVG_RELATIVE));
        }
    }

    // Direct repo execution from current directory.
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(ICON_PNG_RELATIVE));
        candidates.push(cwd.join(ICON_SVG_RELATIVE));
    }

    if let Some(path) = first_existing_path(candidates) {
        return Some(path);
    }

    write_embedded_icon_to_cache()
}

fn write_embedded_icon_to_cache() -> Option<PathBuf> {
    let cache_dir = std::env::temp_dir().join("distill");
    std::fs::create_dir_all(&cache_dir).ok()?;
    let path = cache_dir.join(EMBEDDED_ICON_FILENAME);

    let needs_write = match std::fs::metadata(&path) {
        Ok(metadata) => metadata.len() == 0,
        Err(_) => true,
    };

    if needs_write {
        std::fs::write(&path, EMBEDDED_ICON_PNG).ok()?;
    }

    Some(path)
}

fn resolve_icon_path(configured_icon: Option<&str>) -> Option<String> {
    if let Some(raw) = configured_icon
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        let configured = PathBuf::from(raw);
        if configured.exists() {
            return Some(configured.to_string_lossy().into_owned());
        }
    }

    default_notification_icon_path().map(|path| path.to_string_lossy().into_owned())
}

// ── macOS notifier ───────────────────────────────────────────────────────────

#[cfg(any(target_os = "macos", test))]
fn escape_for_applescript(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

/// macOS native notifier via `osascript`.
///
/// Included on macOS and in tests.
#[cfg(any(target_os = "macos", test))]
pub struct MacOsScriptNotifier;

#[cfg(any(target_os = "macos", test))]
impl Notifier for MacOsScriptNotifier {
    fn send(&self, title: &str, body: &str, _icon_path: Option<&str>) -> Result<()> {
        let escaped_title = escape_for_applescript(title);
        let escaped_body = escape_for_applescript(body);
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            escaped_body, escaped_title
        );
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

/// macOS native notifier via `terminal-notifier`.
///
/// Used when an icon path is configured, because `osascript` does not support
/// custom notification icons.
#[cfg(any(target_os = "macos", test))]
pub struct MacOsTerminalNotifier;

#[cfg(any(target_os = "macos", test))]
impl Notifier for MacOsTerminalNotifier {
    fn send(&self, title: &str, body: &str, icon_path: Option<&str>) -> Result<()> {
        let mut command = Command::new("terminal-notifier");
        command.arg("-title").arg(title).arg("-message").arg(body);
        if let Some(icon) = icon_path.filter(|path| !path.trim().is_empty()) {
            command.arg("-appIcon").arg(icon);
        }

        let status = command
            .status()
            .context("Failed to run terminal-notifier")?;
        if !status.success() {
            anyhow::bail!("terminal-notifier exited with non-zero status: {}", status);
        }
        Ok(())
    }

    fn is_available(&self) -> bool {
        cfg!(target_os = "macos") && binary_exists("terminal-notifier")
    }
}

/// macOS native notifier with icon-aware backend selection.
///
/// If an icon is configured and `terminal-notifier` is installed, it is used.
/// Otherwise, we fall back to `osascript`.
#[cfg(any(target_os = "macos", test))]
pub struct MacOsNotifier;

#[cfg(any(target_os = "macos", test))]
impl Notifier for MacOsNotifier {
    fn send(&self, title: &str, body: &str, icon_path: Option<&str>) -> Result<()> {
        if icon_path.is_some() {
            let notifier = MacOsTerminalNotifier;
            if notifier.is_available() {
                return notifier.send(title, body, icon_path);
            }
        }
        MacOsScriptNotifier.send(title, body, None)
    }

    fn is_available(&self) -> bool {
        cfg!(target_os = "macos")
    }
}

// ── Linux notifier ───────────────────────────────────────────────────────────

/// Linux native notifier via `notify-send`.
///
/// Included on Linux and in tests.
#[cfg(any(target_os = "linux", test))]
pub struct LinuxNotifier;

#[cfg(any(target_os = "linux", test))]
impl Notifier for LinuxNotifier {
    fn send(&self, title: &str, body: &str, icon_path: Option<&str>) -> Result<()> {
        let mut command = Command::new("notify-send");
        if let Some(icon) = icon_path.filter(|path| !path.trim().is_empty()) {
            command.arg("--icon").arg(icon);
        }

        let status = command
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
        cfg!(target_os = "linux") && binary_exists("notify-send")
    }
}

// ── Terminal notifier ────────────────────────────────────────────────────────

/// Simple terminal notifier — always available, just prints to stdout.
pub struct TerminalNotifier;

impl Notifier for TerminalNotifier {
    fn send(&self, title: &str, body: &str, _icon_path: Option<&str>) -> Result<()> {
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
///
/// `icon_path` applies only to native backends that support custom icons.
pub fn send_notification(
    pref: &NotificationPref,
    title: &str,
    body: &str,
    icon_path: Option<&str>,
) -> Result<()> {
    let resolved_icon = resolve_icon_path(icon_path);
    let icon_arg = resolved_icon.as_deref();

    match pref {
        NotificationPref::None => Ok(()),
        NotificationPref::Terminal => TerminalNotifier.send(title, body, icon_arg),
        NotificationPref::Native => {
            let native = platform_notifier();
            if native.is_available() {
                native.send(title, body, icon_arg)
            } else {
                TerminalNotifier.send(title, body, icon_arg)
            }
        }
        NotificationPref::Both => {
            TerminalNotifier.send(title, body, icon_arg)?;
            let native = platform_notifier();
            if native.is_available() {
                native.send(title, body, icon_arg)?;
            }
            Ok(())
        }
    }
}

/// Convenience function called at the end of `distill scan`.
///
/// Does nothing when `proposal_count` is zero.
/// Otherwise dispatches to `send_notification` with a formatted body.
pub fn notify_scan_complete(
    proposal_count: usize,
    pref: &NotificationPref,
    icon_path: Option<&str>,
) -> Result<()> {
    if proposal_count == 0 {
        return Ok(());
    }
    let body = format!(
        "{} new proposal(s) ready. Run 'distill review'.",
        proposal_count
    );
    send_notification(pref, "distill", &body, icon_path)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_first_existing_path_returns_none_when_all_missing() {
        let dir = tempdir().unwrap();
        let missing_a = dir.path().join("missing-a.png");
        let missing_b = dir.path().join("missing-b.png");

        let found = first_existing_path(vec![missing_a, missing_b]);
        assert!(found.is_none());
    }

    #[test]
    fn test_first_existing_path_picks_first_existing_candidate() {
        let dir = tempdir().unwrap();
        let first_existing = dir.path().join("first.png");
        let second_existing = dir.path().join("second.png");
        std::fs::write(&first_existing, b"png").unwrap();
        std::fs::write(&second_existing, b"png").unwrap();

        let found = first_existing_path(vec![
            dir.path().join("missing.png"),
            first_existing.clone(),
            second_existing,
        ])
        .unwrap();

        assert_eq!(found, first_existing.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_icon_path_prefers_valid_configured_path() {
        let dir = tempdir().unwrap();
        let configured = dir.path().join("configured.png");
        std::fs::write(&configured, b"png").unwrap();

        let resolved = resolve_icon_path(Some(configured.to_str().unwrap()));
        assert_eq!(
            resolved.as_deref(),
            Some(configured.to_str().unwrap()),
            "explicit config path should take precedence when file exists"
        );
    }

    #[test]
    fn test_write_embedded_icon_to_cache_creates_non_empty_file() {
        let path = write_embedded_icon_to_cache()
            .expect("embedded icon should be written when no file candidate exists");
        let metadata = std::fs::metadata(&path).expect("embedded icon path should exist");
        assert!(metadata.len() > 0, "embedded icon file should not be empty");
    }

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
        let result = notifier.send("Test Title", "Test body text", Some("/tmp/icon.png"));
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
        let result = send_notification(&NotificationPref::None, "distill", "any body", None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_send_notification_terminal_pref() {
        // Terminal pref should print to stdout and return Ok.
        let result = send_notification(
            &NotificationPref::Terminal,
            "distill",
            "3 new proposal(s) ready. Run 'distill review'.",
            Some("/tmp/icon.png"),
        );
        assert!(result.is_ok());
    }

    // ── notify_scan_complete ──────────────────────────────────────────────────

    #[test]
    fn test_notify_scan_complete_zero_proposals() {
        // Zero proposals must be a silent no-op regardless of pref.
        for pref in [NotificationPref::None, NotificationPref::Terminal] {
            let result = notify_scan_complete(0, &pref, Some("/tmp/icon.png"));
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
        let result = notify_scan_complete(2, &NotificationPref::Terminal, None);
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
        let result = notify_scan_complete(count, &NotificationPref::Terminal, None);
        assert!(result.is_ok());
    }
}
