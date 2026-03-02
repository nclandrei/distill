// Shell hook installer — installs notification check into shell config.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::ShellType;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MARKER: &str = "# distill hook";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Outcome of an install or uninstall operation.
#[derive(Debug, PartialEq)]
pub enum HookStatus {
    /// Hook was successfully written to the config file.
    Installed,
    /// Hook marker was already present; no changes were made.
    AlreadyInstalled,
    /// Hook was successfully removed from the config file.
    Removed,
    /// No hook marker was found; nothing to remove.
    NotFound,
    /// Shell is not supported (e.g. `ShellType::Other`); no config path exists.
    Unsupported,
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Return the shell-specific hook snippet that should be appended to the
/// config file.  Returns an empty string for unsupported shells.
pub fn hook_snippet(shell: &ShellType) -> &'static str {
    match shell {
        ShellType::Zsh | ShellType::Bash => {
            "# distill hook\ncommand -v distill &>/dev/null && distill notify --check"
        }
        ShellType::Fish => {
            "# distill hook\nif command -q distill; distill notify --check; end"
        }
        ShellType::Other => "",
    }
}

/// Return the path of the shell config file that should receive the hook,
/// relative to the given `home` directory.  Returns `None` for unsupported
/// shells.
pub fn shell_config_path(shell: &ShellType, home: &Path) -> Option<PathBuf> {
    match shell {
        ShellType::Zsh => Some(home.join(".zshrc")),
        ShellType::Bash => Some(home.join(".bashrc")),
        ShellType::Fish => Some(
            home.join(".config")
                .join("fish")
                .join("conf.d")
                .join("distill.fish"),
        ),
        ShellType::Other => None,
    }
}

// ---------------------------------------------------------------------------
// Install / uninstall
// ---------------------------------------------------------------------------

/// Append the distill hook to the shell config file under `home`.
///
/// * Returns `HookStatus::Unsupported` when the shell has no known config
///   path.
/// * Returns `HookStatus::AlreadyInstalled` when the marker comment is
///   already present in the file.
/// * Creates parent directories when necessary (required for fish's
///   `conf.d/` layout).
/// * Returns `HookStatus::Installed` on success.
pub fn install_hook(shell: &ShellType, home: &Path) -> Result<HookStatus> {
    let config_path = match shell_config_path(shell, home) {
        Some(p) => p,
        None => return Ok(HookStatus::Unsupported),
    };

    // Read the existing file content, or start with an empty string.
    let existing = if config_path.exists() {
        fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?
    } else {
        String::new()
    };

    // Idempotency check — bail out if the marker is already present.
    if existing.contains(MARKER) {
        return Ok(HookStatus::AlreadyInstalled);
    }

    // Ensure parent directories exist (critical for fish conf.d/).
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    let snippet = hook_snippet(shell);

    // Build the new content: existing text + separator + snippet + trailing
    // newline.  Use a single newline if the file already ends with one,
    // otherwise use two newlines so the block is visually separated.
    let new_content = if existing.is_empty() {
        format!("{}\n", snippet)
    } else if existing.ends_with('\n') {
        format!("{}\n{}\n", existing, snippet)
    } else {
        format!("{}\n\n{}\n", existing, snippet)
    };

    fs::write(&config_path, new_content)
        .with_context(|| format!("Failed to write {}", config_path.display()))?;

    Ok(HookStatus::Installed)
}

/// Remove the distill hook from the shell config file under `home`.
///
/// The hook block consists of the marker line (`# distill hook`) and the
/// immediately following command line.  Both lines are removed.
///
/// * Returns `HookStatus::Unsupported` when the shell has no known config
///   path.
/// * Returns `HookStatus::NotFound` when the config file does not exist or
///   does not contain the marker.
/// * Returns `HookStatus::Removed` on success.
pub fn uninstall_hook(shell: &ShellType, home: &Path) -> Result<HookStatus> {
    let config_path = match shell_config_path(shell, home) {
        Some(p) => p,
        None => return Ok(HookStatus::Unsupported),
    };

    if !config_path.exists() {
        return Ok(HookStatus::NotFound);
    }

    let existing = fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    if !existing.contains(MARKER) {
        return Ok(HookStatus::NotFound);
    }

    // Walk the lines; drop the marker line and the line that immediately
    // follows it (the actual shell command).
    let lines: Vec<&str> = existing.lines().collect();
    let mut new_lines: Vec<&str> = Vec::with_capacity(lines.len());
    let mut skip_next = false;

    for line in &lines {
        if skip_next {
            skip_next = false;
            continue;
        }
        if *line == MARKER {
            skip_next = true;
            continue;
        }
        new_lines.push(line);
    }

    // Re-join, preserving a trailing newline when the original had one.
    let mut new_content = new_lines.join("\n");
    if existing.ends_with('\n') {
        new_content.push('\n');
    }

    fs::write(&config_path, new_content)
        .with_context(|| format!("Failed to write {}", config_path.display()))?;

    Ok(HookStatus::Removed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── hook_snippet ──────────────────────────────────────────────────────────

    #[test]
    fn test_hook_snippet_zsh() {
        let snippet = hook_snippet(&ShellType::Zsh);
        assert_eq!(
            snippet,
            "# distill hook\ncommand -v distill &>/dev/null && distill notify --check"
        );
    }

    #[test]
    fn test_hook_snippet_bash() {
        let snippet = hook_snippet(&ShellType::Bash);
        assert_eq!(
            snippet,
            "# distill hook\ncommand -v distill &>/dev/null && distill notify --check"
        );
    }

    #[test]
    fn test_hook_snippet_fish() {
        let snippet = hook_snippet(&ShellType::Fish);
        assert_eq!(
            snippet,
            "# distill hook\nif command -q distill; distill notify --check; end"
        );
    }

    #[test]
    fn test_hook_snippet_other_is_empty() {
        assert_eq!(hook_snippet(&ShellType::Other), "");
    }

    // ── shell_config_path ─────────────────────────────────────────────────────

    #[test]
    fn test_shell_config_path_zsh() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = shell_config_path(&ShellType::Zsh, home).unwrap();
        assert_eq!(path, home.join(".zshrc"));
    }

    #[test]
    fn test_shell_config_path_bash() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = shell_config_path(&ShellType::Bash, home).unwrap();
        assert_eq!(path, home.join(".bashrc"));
    }

    #[test]
    fn test_shell_config_path_fish() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let path = shell_config_path(&ShellType::Fish, home).unwrap();
        assert_eq!(
            path,
            home.join(".config").join("fish").join("conf.d").join("distill.fish")
        );
    }

    #[test]
    fn test_shell_config_path_other() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        assert!(shell_config_path(&ShellType::Other, home).is_none());
    }

    // ── install_hook ──────────────────────────────────────────────────────────

    #[test]
    fn test_install_hook_zsh() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let status = install_hook(&ShellType::Zsh, home).unwrap();
        assert_eq!(status, HookStatus::Installed);

        let contents = fs::read_to_string(home.join(".zshrc")).unwrap();
        assert!(contents.contains(MARKER));
        assert!(contents.contains("distill notify --check"));
    }

    #[test]
    fn test_install_hook_bash() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let status = install_hook(&ShellType::Bash, home).unwrap();
        assert_eq!(status, HookStatus::Installed);

        let contents = fs::read_to_string(home.join(".bashrc")).unwrap();
        assert!(contents.contains(MARKER));
    }

    #[test]
    fn test_install_hook_appends_to_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Pre-populate .zshrc with some existing content.
        fs::write(home.join(".zshrc"), "export PATH=$HOME/bin:$PATH\n").unwrap();

        install_hook(&ShellType::Zsh, home).unwrap();

        let contents = fs::read_to_string(home.join(".zshrc")).unwrap();
        // Original content must still be present.
        assert!(contents.contains("export PATH=$HOME/bin:$PATH"));
        // Hook must follow it.
        assert!(contents.contains(MARKER));
    }

    #[test]
    fn test_install_hook_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // First install.
        let first = install_hook(&ShellType::Zsh, home).unwrap();
        assert_eq!(first, HookStatus::Installed);

        // Second install must report AlreadyInstalled and must NOT duplicate
        // the marker in the file.
        let second = install_hook(&ShellType::Zsh, home).unwrap();
        assert_eq!(second, HookStatus::AlreadyInstalled);

        let contents = fs::read_to_string(home.join(".zshrc")).unwrap();
        let count = contents.matches(MARKER).count();
        assert_eq!(count, 1, "marker must appear exactly once");
    }

    #[test]
    fn test_install_hook_fish_creates_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // The conf.d directory does not exist yet.
        let conf_d = home.join(".config").join("fish").join("conf.d");
        assert!(!conf_d.exists());

        let status = install_hook(&ShellType::Fish, home).unwrap();
        assert_eq!(status, HookStatus::Installed);

        // Directory must have been created.
        assert!(conf_d.is_dir());

        let fish_file = conf_d.join("distill.fish");
        let contents = fs::read_to_string(&fish_file).unwrap();
        assert!(contents.contains(MARKER));
        assert!(contents.contains("if command -q distill"));
    }

    #[test]
    fn test_install_hook_unsupported_shell() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let status = install_hook(&ShellType::Other, home).unwrap();
        assert_eq!(status, HookStatus::Unsupported);
    }

    // ── uninstall_hook ────────────────────────────────────────────────────────

    #[test]
    fn test_uninstall_hook() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Install first, then uninstall.
        install_hook(&ShellType::Zsh, home).unwrap();

        let status = uninstall_hook(&ShellType::Zsh, home).unwrap();
        assert_eq!(status, HookStatus::Removed);

        let contents = fs::read_to_string(home.join(".zshrc")).unwrap();
        assert!(!contents.contains(MARKER));
        assert!(!contents.contains("distill notify --check"));
    }

    #[test]
    fn test_uninstall_hook_preserves_surrounding_content() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        fs::write(home.join(".zshrc"), "export EDITOR=vim\n").unwrap();
        install_hook(&ShellType::Zsh, home).unwrap();
        // Add more content after the hook by appending directly.
        let mut contents = fs::read_to_string(home.join(".zshrc")).unwrap();
        contents.push_str("alias ll='ls -la'\n");
        fs::write(home.join(".zshrc"), &contents).unwrap();

        uninstall_hook(&ShellType::Zsh, home).unwrap();

        let final_contents = fs::read_to_string(home.join(".zshrc")).unwrap();
        assert!(final_contents.contains("export EDITOR=vim"));
        assert!(final_contents.contains("alias ll='ls -la'"));
        assert!(!final_contents.contains(MARKER));
    }

    #[test]
    fn test_uninstall_hook_not_found_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // .zshrc does not exist at all.
        let status = uninstall_hook(&ShellType::Zsh, home).unwrap();
        assert_eq!(status, HookStatus::NotFound);
    }

    #[test]
    fn test_uninstall_hook_not_found_no_marker() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // .zshrc exists but has no hook.
        fs::write(home.join(".zshrc"), "export EDITOR=vim\n").unwrap();

        let status = uninstall_hook(&ShellType::Zsh, home).unwrap();
        assert_eq!(status, HookStatus::NotFound);
    }

    #[test]
    fn test_uninstall_hook_unsupported_shell() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        let status = uninstall_hook(&ShellType::Other, home).unwrap();
        assert_eq!(status, HookStatus::Unsupported);
    }

    #[test]
    fn test_uninstall_hook_fish() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        install_hook(&ShellType::Fish, home).unwrap();

        let status = uninstall_hook(&ShellType::Fish, home).unwrap();
        assert_eq!(status, HookStatus::Removed);

        let fish_file = home
            .join(".config")
            .join("fish")
            .join("conf.d")
            .join("distill.fish");
        let contents = fs::read_to_string(&fish_file).unwrap();
        assert!(!contents.contains(MARKER));
        assert!(!contents.contains("if command -q distill"));
    }
}
