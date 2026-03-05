use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Interval {
    Daily,
    #[default]
    Weekly,
    Monthly,
}

impl fmt::Display for Interval {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Interval::Daily => write!(f, "daily"),
            Interval::Weekly => write!(f, "weekly"),
            Interval::Monthly => write!(f, "monthly"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum NotificationPref {
    Terminal,
    Native,
    #[default]
    Both,
    None,
}

impl fmt::Display for NotificationPref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotificationPref::Terminal => write!(f, "terminal"),
            NotificationPref::Native => write!(f, "native"),
            NotificationPref::Both => write!(f, "both"),
            NotificationPref::None => write!(f, "none"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ShellType {
    Zsh,
    Bash,
    Fish,
    Other,
}

impl fmt::Display for ShellType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShellType::Zsh => write!(f, "zsh"),
            ShellType::Bash => write!(f, "bash"),
            ShellType::Fish => write!(f, "fish"),
            ShellType::Other => write!(f, "other"),
        }
    }
}

impl ShellType {
    /// Detect the current shell from the `$SHELL` environment variable.
    /// Falls back to `Other` if the variable is absent or unrecognised.
    pub fn detect() -> Self {
        let shell = std::env::var("SHELL").unwrap_or_default();
        if shell.contains("zsh") {
            ShellType::Zsh
        } else if shell.contains("bash") {
            ShellType::Bash
        } else if shell.contains("fish") {
            ShellType::Fish
        } else {
            ShellType::Other
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentEntry {
    pub name: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SyncAgentsConfig {
    #[serde(default)]
    pub projects: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub agents: Vec<AgentEntry>,
    #[serde(default)]
    pub scan_interval: Interval,
    pub proposal_agent: String,
    pub shell: ShellType,
    #[serde(default)]
    pub notifications: NotificationPref,
    #[serde(default)]
    pub notification_icon: Option<String>,
    #[serde(default)]
    pub sync_agents: SyncAgentsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            agents: vec![
                AgentEntry {
                    name: "claude".into(),
                    enabled: true,
                },
                AgentEntry {
                    name: "codex".into(),
                    enabled: true,
                },
            ],
            scan_interval: Interval::default(),
            proposal_agent: "claude".into(),
            shell: ShellType::Zsh,
            notifications: NotificationPref::default(),
            notification_icon: None,
            sync_agents: SyncAgentsConfig::default(),
        }
    }
}

impl Config {
    /// Returns the base distill directory (~/.distill)
    pub fn base_dir() -> PathBuf {
        dirs_or_home().join(".distill")
    }

    /// Returns the path to the config file
    pub fn config_path() -> PathBuf {
        Self::base_dir().join("config.yaml")
    }

    /// Returns the proposals directory
    pub fn proposals_dir() -> PathBuf {
        Self::base_dir().join("proposals")
    }

    /// Returns the skills directory
    pub fn skills_dir() -> PathBuf {
        Self::base_dir().join("skills")
    }

    /// Returns the history directory
    pub fn history_dir() -> PathBuf {
        Self::base_dir().join("history")
    }

    /// Returns the last-scan.json path
    pub fn last_scan_path() -> PathBuf {
        Self::base_dir().join("last-scan.json")
    }

    /// Returns the last-sync-agents.json path
    pub fn last_sync_agents_path() -> PathBuf {
        Self::base_dir().join("last-sync-agents.json")
    }

    /// Check if config file exists
    pub fn exists() -> bool {
        Self::config_path().exists()
    }

    /// Ensure all distill directories exist under the default base path.
    pub fn ensure_dirs() -> Result<()> {
        Self::ensure_dirs_at(&Self::base_dir())
    }

    /// Ensure all distill directories exist under the given base path.
    /// Creates `base`, `base/proposals`, `base/skills`, and `base/history`.
    pub fn ensure_dirs_at(base: &Path) -> Result<()> {
        for dir in [
            base.to_path_buf(),
            base.join("proposals"),
            base.join("skills"),
            base.join("history"),
        ] {
            fs::create_dir_all(&dir)
                .with_context(|| format!("Failed to create directory: {}", dir.display()))?;
        }
        Ok(())
    }

    /// Load config from the default location on disk.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::config_path())
    }

    /// Load config from a specific path.
    pub fn load_from(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let config: Config =
            serde_yaml::from_str(&contents).with_context(|| "Failed to parse config")?;
        Ok(config)
    }

    /// Save config to the default location on disk.
    /// Also ensures the full distill directory structure exists.
    pub fn save(&self) -> Result<()> {
        Self::ensure_dirs()?;
        self.save_to(&Self::config_path())
    }

    /// Save config to a specific path, creating parent directories as needed.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        let yaml = serde_yaml::to_string(self).context("Failed to serialize config")?;
        fs::write(path, yaml).with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(())
    }
}

fn dirs_or_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── existing tests (kept intact) ─────────────────────────────────────────

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.scan_interval, Interval::Weekly);
        assert_eq!(config.notifications, NotificationPref::Both);
        assert_eq!(config.notification_icon, None);
        assert!(config.sync_agents.projects.is_empty());
        assert_eq!(config.proposal_agent, "claude");
        assert_eq!(config.agents.len(), 2);
    }

    #[test]
    fn test_config_roundtrip_yaml() {
        let config = Config::default();
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_config_deserialize_from_yaml() {
        let yaml = r#"
agents:
  - name: claude
    enabled: true
  - name: codex
    enabled: false
scan_interval: daily
proposal_agent: claude
shell: bash
notifications: terminal
notification_icon: /tmp/distill.png
sync_agents:
  projects:
    - /tmp/project-a
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.scan_interval, Interval::Daily);
        assert_eq!(config.notifications, NotificationPref::Terminal);
        assert_eq!(
            config.notification_icon.as_deref(),
            Some("/tmp/distill.png")
        );
        assert_eq!(
            config.sync_agents.projects,
            vec!["/tmp/project-a".to_string()]
        );
        assert_eq!(config.shell, ShellType::Bash);
        assert!(!config.agents[1].enabled);
    }

    #[test]
    fn test_config_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");

        let config = Config::default();
        let yaml = serde_yaml::to_string(&config).unwrap();
        fs::write(&config_path, &yaml).unwrap();

        let contents = fs::read_to_string(&config_path).unwrap();
        let loaded: Config = serde_yaml::from_str(&contents).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn test_config_deserialize_without_sync_agents_defaults_empty() {
        let yaml = r#"
agents:
  - name: claude
    enabled: true
scan_interval: weekly
proposal_agent: claude
shell: zsh
notifications: both
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert!(config.sync_agents.projects.is_empty());
    }

    // ── new tests ─────────────────────────────────────────────────────────────

    #[test]
    fn test_save_to_load_from_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.yaml");

        let config = Config::default();
        config.save_to(&config_path).unwrap();

        let loaded = Config::load_from(&config_path).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn test_save_to_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // Deeply nested path that does not yet exist.
        let config_path = dir.path().join("a").join("b").join("config.yaml");

        let config = Config::default();
        config.save_to(&config_path).unwrap();

        assert!(config_path.exists(), "config file should have been created");
        let loaded = Config::load_from(&config_path).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn test_ensure_dirs_at_creates_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("distill");

        Config::ensure_dirs_at(&base).unwrap();

        assert!(base.is_dir(), "base dir should exist");
        assert!(
            base.join("proposals").is_dir(),
            "proposals dir should exist"
        );
        assert!(base.join("skills").is_dir(), "skills dir should exist");
        assert!(base.join("history").is_dir(), "history dir should exist");
    }

    #[test]
    fn test_ensure_dirs_at_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("distill");

        // Calling twice must not error.
        Config::ensure_dirs_at(&base).unwrap();
        Config::ensure_dirs_at(&base).unwrap();

        assert!(base.join("skills").is_dir());
    }

    #[test]
    fn test_interval_display() {
        assert_eq!(Interval::Daily.to_string(), "daily");
        assert_eq!(Interval::Weekly.to_string(), "weekly");
        assert_eq!(Interval::Monthly.to_string(), "monthly");
    }

    #[test]
    fn test_notification_pref_display() {
        assert_eq!(NotificationPref::Terminal.to_string(), "terminal");
        assert_eq!(NotificationPref::Native.to_string(), "native");
        assert_eq!(NotificationPref::Both.to_string(), "both");
        assert_eq!(NotificationPref::None.to_string(), "none");
    }

    #[test]
    fn test_shell_type_display() {
        assert_eq!(ShellType::Zsh.to_string(), "zsh");
        assert_eq!(ShellType::Bash.to_string(), "bash");
        assert_eq!(ShellType::Fish.to_string(), "fish");
        assert_eq!(ShellType::Other.to_string(), "other");
    }

    /// All `ShellType::detect()` assertions live in one test function so that
    /// the sequential `$SHELL` mutations cannot race with other tests.
    #[test]
    fn test_shell_type_detect() {
        // SAFETY: This is a single-threaded test; no other thread reads $SHELL here.
        unsafe {
            std::env::set_var("SHELL", "/bin/zsh");
            assert_eq!(ShellType::detect(), ShellType::Zsh);

            std::env::set_var("SHELL", "/bin/bash");
            assert_eq!(ShellType::detect(), ShellType::Bash);

            std::env::set_var("SHELL", "/usr/local/bin/fish");
            assert_eq!(ShellType::detect(), ShellType::Fish);

            std::env::set_var("SHELL", "/bin/sh");
            assert_eq!(ShellType::detect(), ShellType::Other);
        }
    }

    #[test]
    fn test_load_from_missing_optional_fields_uses_defaults() {
        // `scan_interval` and `notifications` carry `#[serde(default)]`, so
        // a minimal document that omits them must still deserialise cleanly.
        let yaml = r#"
agents:
  - name: claude
    enabled: true
proposal_agent: claude
shell: zsh
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.scan_interval, Interval::Weekly);
        assert_eq!(config.notifications, NotificationPref::Both);
        assert_eq!(config.notification_icon, None);
    }

    #[test]
    fn test_load_from_returns_error_on_missing_file() {
        let result = Config::load_from(Path::new("/nonexistent/path/config.yaml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_save_to_load_from_preserves_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.yaml");

        let config = Config {
            agents: vec![
                AgentEntry {
                    name: "claude".into(),
                    enabled: true,
                },
                AgentEntry {
                    name: "codex".into(),
                    enabled: false,
                },
            ],
            scan_interval: Interval::Monthly,
            proposal_agent: "codex".into(),
            shell: ShellType::Fish,
            notifications: NotificationPref::Native,
            notification_icon: Some("/tmp/distill-icon.png".into()),
            sync_agents: SyncAgentsConfig {
                projects: vec!["/tmp/project-a".into(), "/tmp/project-b".into()],
            },
        };

        config.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();

        assert_eq!(loaded.scan_interval, Interval::Monthly);
        assert_eq!(loaded.proposal_agent, "codex");
        assert_eq!(loaded.shell, ShellType::Fish);
        assert_eq!(loaded.notifications, NotificationPref::Native);
        assert_eq!(
            loaded.notification_icon.as_deref(),
            Some("/tmp/distill-icon.png")
        );
        assert_eq!(
            loaded.sync_agents.projects,
            vec!["/tmp/project-a".to_string(), "/tmp/project-b".to_string()]
        );
        assert!(!loaded.agents[1].enabled);
    }
}
