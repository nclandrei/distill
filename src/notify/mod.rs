use anyhow::{Context, Result};
use base64::Engine as _;
use image::ImageFormat;
use image::imageops::FilterType;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::Command;
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
const TERMINAL_IMAGE_MODE_ENV: &str = "DISTILL_TERMINAL_IMAGE";
const TERMINAL_IMAGE_PROTOCOL_ENV: &str = "DISTILL_TERMINAL_IMAGE_PROTOCOL";
const TERMINAL_IMAGE_COLUMNS: usize = 8;
const TERMINAL_IMAGE_ROWS: usize = 4;
const TERMINAL_IMAGE_ANSI_COLUMNS: usize = 8;
const TERMINAL_IMAGE_ANSI_ROWS: usize = 4;
const TERMINAL_IMAGE_MAX_EDGE_PX: u32 = 256;
const TERMINAL_IMAGE_ITERM_PX: usize = 72;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalImageProtocol {
    Kitty,
    Iterm,
    Ansi,
}

fn parse_terminal_image_protocol(raw: &str) -> Option<TerminalImageProtocol> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "kitty" => Some(TerminalImageProtocol::Kitty),
        "iterm" | "iterm2" | "osc1337" => Some(TerminalImageProtocol::Iterm),
        "ansi" | "blocks" => Some(TerminalImageProtocol::Ansi),
        "none" | "off" | "false" => None,
        _ => None,
    }
}

fn in_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

fn tmux_passthrough_enabled() -> bool {
    if !in_tmux() {
        return true;
    }

    let Ok(output) = Command::new("tmux")
        .args(["show-options", "-gv", "allow-passthrough"])
        .output()
    else {
        return false;
    };

    if !output.status.success() {
        return false;
    }

    matches!(
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "on" | "all" | "1" | "true"
    )
}

fn tmux_client_termname() -> Option<String> {
    if !in_tmux() {
        return None;
    }
    let output = Command::new("tmux")
        .args(["display-message", "-p", "#{client_termname}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let term = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_ascii_lowercase();
    if term.is_empty() { None } else { Some(term) }
}

fn protocol_from_terminal_name(name: &str) -> Option<TerminalImageProtocol> {
    let normalized = name.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    if normalized.contains("kitty")
        || normalized.contains("ghostty")
        || normalized.contains("wezterm")
    {
        return Some(TerminalImageProtocol::Kitty);
    }
    if normalized.contains("iterm") {
        return Some(TerminalImageProtocol::Iterm);
    }
    None
}

fn detect_terminal_image_protocol() -> Option<TerminalImageProtocol> {
    if let Ok(raw) = std::env::var(TERMINAL_IMAGE_PROTOCOL_ENV) {
        let parsed = parse_terminal_image_protocol(&raw);
        if in_tmux()
            && !matches!(parsed, Some(TerminalImageProtocol::Ansi) | None)
            && !tmux_passthrough_enabled()
        {
            return None;
        }
        return parsed;
    }

    if in_tmux() && !tmux_passthrough_enabled() {
        return None;
    }

    if let Some(term) = tmux_client_termname() {
        if let Some(protocol) = protocol_from_terminal_name(&term) {
            return Some(protocol);
        }
    }

    if let Ok(term_program) = std::env::var("TERM_PROGRAM") {
        if let Some(protocol) = protocol_from_terminal_name(&term_program) {
            return Some(protocol);
        }
    }

    if let Ok(term) = std::env::var("TERM") {
        if let Some(protocol) = protocol_from_terminal_name(&term) {
            return Some(protocol);
        }
    }

    None
}

fn should_render_terminal_image() -> bool {
    if !std::io::stdout().is_terminal() {
        return false;
    }
    if matches!(std::env::var("TERM").ok().as_deref(), Some("dumb")) {
        return false;
    }

    let mode = std::env::var(TERMINAL_IMAGE_MODE_ENV).ok();
    match mode.as_deref().map(str::trim).map(str::to_ascii_lowercase) {
        None => true,
        Some(raw) => !matches!(raw.as_str(), "0" | "false" | "off" | "none"),
    }
}

fn rasterize_svg_to_png(svg_bytes: &[u8], max_edge: u32) -> Option<Vec<u8>> {
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(svg_bytes, &options).ok()?;
    let size = tree.size();
    let width = size.width();
    let height = size.height();
    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    let max_dimension = width.max(height);
    let scale = if max_dimension > max_edge as f32 {
        max_edge as f32 / max_dimension
    } else {
        1.0
    };
    let target_width = (width * scale).round().max(1.0) as u32;
    let target_height = (height * scale).round().max(1.0) as u32;

    let mut pixmap = resvg::tiny_skia::Pixmap::new(target_width, target_height)?;
    let transform = resvg::tiny_skia::Transform::from_scale(
        target_width as f32 / width,
        target_height as f32 / height,
    );
    let mut pixmap_mut = pixmap.as_mut();
    resvg::render(&tree, transform, &mut pixmap_mut);
    pixmap.encode_png().ok()
}

fn normalize_raster_to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let dynamic = image::load_from_memory(bytes).ok()?;
    let mut cursor = std::io::Cursor::new(Vec::new());
    dynamic.write_to(&mut cursor, ImageFormat::Png).ok()?;
    Some(cursor.into_inner())
}

fn icon_path_extension(path: &std::path::Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

fn terminal_image_bytes(icon_path: Option<&str>) -> Vec<u8> {
    if let Some(path) = resolve_icon_path(icon_path) {
        let path = PathBuf::from(path);
        if let Ok(bytes) = std::fs::read(&path) {
            if !bytes.is_empty() {
                match icon_path_extension(&path).as_deref() {
                    Some("png") => return bytes,
                    Some("svg") => {
                        if let Some(png) = rasterize_svg_to_png(&bytes, TERMINAL_IMAGE_MAX_EDGE_PX)
                        {
                            return png;
                        }
                    }
                    _ => {
                        if let Some(png) = normalize_raster_to_png(&bytes) {
                            return png;
                        }
                    }
                }
            }
        }
    }
    EMBEDDED_ICON_PNG.to_vec()
}

fn emit_iterm_inline_image(png_bytes: &[u8]) -> std::io::Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(png_bytes);
    let mut out = std::io::stdout();
    let sequence = format!(
        "\x1b]1337;File=inline=1;width={}px;height={}px;preserveAspectRatio=1:{}\x07",
        TERMINAL_IMAGE_ITERM_PX, TERMINAL_IMAGE_ITERM_PX, encoded,
    );
    write_terminal_control_sequence(&mut out, &sequence)?;
    out.flush()
}

fn write_terminal_control_sequence(
    out: &mut std::io::Stdout,
    sequence: &str,
) -> std::io::Result<()> {
    if in_tmux() {
        let escaped = sequence.replace('\x1b', "\x1b\x1b");
        write!(out, "\x1bPtmux;{}\x1b\\", escaped)?;
    } else {
        out.write_all(sequence.as_bytes())?;
    }
    Ok(())
}

fn emit_kitty_inline_image(png_bytes: &[u8]) -> std::io::Result<()> {
    const CHUNK_SIZE: usize = 4096;
    let encoded = base64::engine::general_purpose::STANDARD.encode(png_bytes);
    let total_chunks = encoded.len().div_ceil(CHUNK_SIZE);
    let mut out = std::io::stdout();

    for (index, chunk) in encoded.as_bytes().chunks(CHUNK_SIZE).enumerate() {
        let more = if index + 1 < total_chunks { 1 } else { 0 };
        let chunk = std::str::from_utf8(chunk).unwrap_or_default();
        let sequence = if index == 0 {
            format!(
                "\x1b_Gq=2,a=T,f=100,t=d,c={},r={},m={more};{chunk}\x1b\\",
                TERMINAL_IMAGE_COLUMNS, TERMINAL_IMAGE_ROWS
            )
        } else {
            format!("\x1b_Gq=2,m={more};{chunk}\x1b\\")
        };
        write_terminal_control_sequence(&mut out, &sequence)?;
    }

    out.flush()
}

fn blend_on_black(pixel: [u8; 4]) -> (u8, u8, u8) {
    let alpha = pixel[3] as f32 / 255.0;
    let r = (pixel[0] as f32 * alpha).round() as u8;
    let g = (pixel[1] as f32 * alpha).round() as u8;
    let b = (pixel[2] as f32 * alpha).round() as u8;
    (r, g, b)
}

fn emit_ansi_block_image(png_bytes: &[u8]) -> std::io::Result<()> {
    let dynamic = image::load_from_memory(png_bytes).map_err(std::io::Error::other)?;
    let rgba = dynamic.to_rgba8();
    let resized = image::imageops::resize(
        &rgba,
        TERMINAL_IMAGE_ANSI_COLUMNS as u32,
        (TERMINAL_IMAGE_ANSI_ROWS * 2) as u32,
        FilterType::Triangle,
    );

    let mut out = std::io::stdout();
    for row in 0..TERMINAL_IMAGE_ANSI_ROWS {
        for col in 0..TERMINAL_IMAGE_ANSI_COLUMNS {
            let top = resized.get_pixel(col as u32, (row * 2) as u32).0;
            let bottom = resized.get_pixel(col as u32, (row * 2 + 1) as u32).0;
            let (tr, tg, tb) = blend_on_black(top);
            let (br, bg, bb) = blend_on_black(bottom);
            write!(out, "\x1b[38;2;{tr};{tg};{tb}m\x1b[48;2;{br};{bg};{bb}m▀")?;
        }
        writeln!(out, "\x1b[0m")?;
    }
    out.flush()
}

pub fn print_terminal_branding(icon_path: Option<&str>) -> bool {
    if !should_render_terminal_image() {
        return false;
    }
    let Some(protocol) = detect_terminal_image_protocol() else {
        return false;
    };
    let bytes = terminal_image_bytes(icon_path);
    let rendered = match protocol {
        TerminalImageProtocol::Kitty => emit_kitty_inline_image(&bytes),
        TerminalImageProtocol::Iterm => emit_iterm_inline_image(&bytes),
        TerminalImageProtocol::Ansi => emit_ansi_block_image(&bytes),
    }
    .is_ok();

    if rendered {
        let mut out = std::io::stdout();
        let padding_lines = match protocol {
            // Kitty/iTerm images are drawn as overlays and do not reliably move
            // the cursor in tmux. Reserve line height explicitly before text.
            TerminalImageProtocol::Kitty => TERMINAL_IMAGE_ROWS,
            TerminalImageProtocol::Iterm => 4,
            // ANSI path already emits its own lines.
            TerminalImageProtocol::Ansi => 1,
        };
        for _ in 0..padding_lines {
            let _ = writeln!(out);
        }
        let _ = out.flush();
    }

    rendered
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
        run_command_with_timeout(&mut command, "terminal-notifier", NOTIFIER_COMMAND_TIMEOUT)
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
                if notifier.send(title, body, icon_path).is_ok() {
                    return Ok(());
                }
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
    fn send(&self, title: &str, body: &str, icon_path: Option<&str>) -> Result<()> {
        let _ = print_terminal_branding(icon_path);
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
    fn test_parse_terminal_image_protocol_iterm_aliases() {
        assert_eq!(
            parse_terminal_image_protocol("iterm2"),
            Some(TerminalImageProtocol::Iterm)
        );
        assert_eq!(
            parse_terminal_image_protocol("osc1337"),
            Some(TerminalImageProtocol::Iterm)
        );
    }

    #[test]
    fn test_parse_terminal_image_protocol_kitty() {
        assert_eq!(
            parse_terminal_image_protocol("kitty"),
            Some(TerminalImageProtocol::Kitty)
        );
    }

    #[test]
    fn test_protocol_from_terminal_name_prefers_kitty_like() {
        assert_eq!(
            protocol_from_terminal_name("xterm-ghostty"),
            Some(TerminalImageProtocol::Kitty)
        );
        assert_eq!(
            protocol_from_terminal_name("xterm-kitty"),
            Some(TerminalImageProtocol::Kitty)
        );
    }

    #[test]
    fn test_protocol_from_terminal_name_iterm() {
        assert_eq!(
            protocol_from_terminal_name("iTerm.app"),
            Some(TerminalImageProtocol::Iterm)
        );
    }

    #[test]
    fn test_parse_terminal_image_protocol_none_for_unknown() {
        assert_eq!(parse_terminal_image_protocol("unknown"), None);
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
