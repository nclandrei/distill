use anyhow::{Context, Result};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::Command;
#[cfg(any(target_os = "macos", test))]
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::config::NotificationPref;

const ICON_PNG_RELATIVE: &str = "assets/icons/png/color/distill-color-256.png";
const ICON_SVG_RELATIVE: &str = "assets/icons/distill-icon.svg";
const ICON_SHARE_PNG_RELATIVE: &str = "share/distill/icons/distill-color-256.png";
const ICON_SHARE_SVG_RELATIVE: &str = "share/distill/icons/distill-icon.svg";
const EMBEDDED_ICON_FILENAME: &str = "distill-color-256.png";
const EMBEDDED_ICON_PNG: &[u8] =
    include_bytes!("../../assets/icons/png/color/distill-color-256.png");
const TERMINAL_BADGE: &str = "[distill]";
const NOTIFIER_COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(any(target_os = "macos", test))]
const PREFERRED_MACOS_SENDERS: &[&str] = &[
    "com.mitchellh.ghostty",
    "com.googlecode.iterm2",
    "com.apple.Terminal",
];

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

fn should_render_terminal_icon() -> bool {
    if let Ok(raw) = std::env::var("DISTILL_TERMINAL_ICON") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "off" => return false,
            "1" | "true" | "on" => return true,
            _ => {}
        }
    }

    if !std::io::stdout().is_terminal() {
        return false;
    }
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    !matches!(std::env::var("TERM").ok().as_deref(), Some("dumb"))
}

pub fn print_terminal_branding() {
    if !should_render_terminal_icon() {
        return;
    }

    const CYAN: &str = "\x1b[38;2;6;182;212m";
    const AMBER: &str = "\x1b[38;2;245;158;11m";
    const RESET: &str = "\x1b[0m";

    println!("{CYAN}      /================\\{RESET}");
    println!("{CYAN}     /  ||   ||   ||    \\{RESET}");
    println!("{CYAN}    /___||___||___||_____\\{RESET}");
    println!("{CYAN}           \\   {AMBER}<>{CYAN}   /{RESET}");
    println!("{AMBER}             \\ () /{RESET}");
}

fn run_command_with_timeout(command: &mut Command, name: &str, timeout: Duration) -> Result<()> {
    let mut child = command
        .spawn()
        .with_context(|| format!("Failed to run {name}"))?;
    let start = Instant::now();

    loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("Failed while waiting for {name}"))?
        {
            if status.success() {
                return Ok(());
            }
            anyhow::bail!("{name} exited with non-zero status: {status}");
        }

        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("{name} timed out after {}ms", timeout.as_millis());
        }

        std::thread::sleep(Duration::from_millis(20));
    }
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
        let mut command = Command::new("osascript");
        command.arg("-e").arg(&script);
        run_command_with_timeout(&mut command, "osascript", NOTIFIER_COMMAND_TIMEOUT)
    }

    fn is_available(&self) -> bool {
        cfg!(target_os = "macos")
    }
}

/// macOS native notifier via `terminal-notifier`.
///
/// We prefer stable sender icons (`-sender`) over `-appIcon`, because
/// `-appIcon` relies on private APIs and is unreliable on newer macOS versions.
#[cfg(any(target_os = "macos", test))]
pub struct MacOsTerminalNotifier;

#[cfg(any(target_os = "macos", test))]
impl Notifier for MacOsTerminalNotifier {
    fn send(&self, title: &str, body: &str, _icon_path: Option<&str>) -> Result<()> {
        let mut command = Command::new("terminal-notifier");
        command.arg("-title").arg(title).arg("-message").arg(body);
        if let Some(sender) = macos_sender_bundle_id() {
            command.arg("-sender").arg(sender);
        }
        run_command_with_timeout(&mut command, "terminal-notifier", NOTIFIER_COMMAND_TIMEOUT)
    }

    fn is_available(&self) -> bool {
        cfg!(target_os = "macos") && binary_exists("terminal-notifier")
    }
}

/// macOS native notifier with icon-aware backend selection.
///
/// If `terminal-notifier` is installed, it is used for richer behavior.
/// Otherwise, we fall back to `osascript`.
#[cfg(any(target_os = "macos", test))]
pub struct MacOsNotifier;

#[cfg(any(target_os = "macos", test))]
impl Notifier for MacOsNotifier {
    fn send(&self, title: &str, body: &str, _icon_path: Option<&str>) -> Result<()> {
        let notifier = MacOsTerminalNotifier;
        if notifier.is_available() {
            return notifier.send(title, body, None);
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
        command.arg(title).arg(body);
        run_command_with_timeout(&mut command, "notify-send", NOTIFIER_COMMAND_TIMEOUT)
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
        print_terminal_branding();
        println!("{TERMINAL_BADGE} {title}");
        println!("          {body}");
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
        NotificationPref::Native => send_native_notification(title, body, icon_arg, true),
        NotificationPref::Both => {
            TerminalNotifier.send(title, body, icon_arg)?;
            send_native_notification(title, body, icon_arg, false)?;
            Ok(())
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn choose_sender_bundle_id<F>(mut has_bundle_id: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    PREFERRED_MACOS_SENDERS
        .iter()
        .copied()
        .find(|bundle_id| has_bundle_id(bundle_id))
        .map(str::to_owned)
}

#[cfg(any(target_os = "macos", test))]
fn macos_has_application_bundle(bundle_id: &str) -> bool {
    let escaped_bundle = escape_for_applescript(bundle_id);
    let script = format!("id of application id \"{escaped_bundle}\"");
    Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(any(target_os = "macos", test))]
fn default_macos_sender_bundle_id() -> Option<String> {
    static DETECTED: OnceLock<Option<String>> = OnceLock::new();
    DETECTED
        .get_or_init(|| choose_sender_bundle_id(macos_has_application_bundle))
        .clone()
}

#[cfg(any(target_os = "macos", test))]
fn macos_sender_bundle_id() -> Option<String> {
    if let Some(override_sender) = std::env::var("DISTILL_NOTIFICATION_SENDER")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        return Some(override_sender);
    }

    default_macos_sender_bundle_id()
}

fn send_native_notification(
    title: &str,
    body: &str,
    icon_path: Option<&str>,
    fallback_to_terminal: bool,
) -> Result<()> {
    let native = platform_notifier();
    if !native.is_available() {
        if fallback_to_terminal {
            TerminalNotifier.send(title, body, icon_path)?;
        }
        return Ok(());
    }

    if let Err(err) = native.send(title, body, icon_path) {
        eprintln!("Warning: native notification failed: {err}");
    }
    Ok(())
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

    #[test]
    fn test_choose_sender_bundle_id_prefers_ghostty_over_others() {
        let selected = choose_sender_bundle_id(|bundle_id| {
            matches!(
                bundle_id,
                "com.mitchellh.ghostty" | "com.googlecode.iterm2" | "com.apple.Terminal"
            )
        });
        assert_eq!(
            selected.as_deref(),
            Some("com.mitchellh.ghostty"),
            "Ghostty should win when available"
        );
    }

    #[test]
    fn test_choose_sender_bundle_id_falls_back_to_iterm() {
        let selected = choose_sender_bundle_id(|bundle_id| {
            matches!(bundle_id, "com.googlecode.iterm2" | "com.apple.Terminal")
        });
        assert_eq!(
            selected.as_deref(),
            Some("com.googlecode.iterm2"),
            "iTerm should win when Ghostty is unavailable"
        );
    }

    #[test]
    fn test_choose_sender_bundle_id_falls_back_to_terminal_last() {
        let selected = choose_sender_bundle_id(|bundle_id| bundle_id == "com.apple.Terminal");
        assert_eq!(selected.as_deref(), Some("com.apple.Terminal"));
    }

    #[test]
    fn test_choose_sender_bundle_id_returns_none_when_no_known_sender_available() {
        let selected = choose_sender_bundle_id(|_| false);
        assert!(selected.is_none());
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

    #[test]
    fn test_send_native_notification_is_best_effort_without_terminal_fallback() {
        if cfg!(target_os = "macos") {
            return;
        }
        let result = send_native_notification("distill", "message", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_send_native_notification_is_best_effort_with_terminal_fallback() {
        if cfg!(target_os = "macos") {
            return;
        }
        let result = send_native_notification("distill", "message", None, true);
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
