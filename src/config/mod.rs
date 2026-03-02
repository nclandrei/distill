use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Interval {
    Daily,
    Weekly,
    Monthly,
}

impl Default for Interval {
    fn default() -> Self {
        Self::Weekly
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum NotificationPref {
    Terminal,
    Native,
    Both,
    None,
}

impl Default for NotificationPref {
    fn default() -> Self {
        Self::Both
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentEntry {
    pub name: String,
    pub enabled: bool,
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

    /// Check if config file exists
    pub fn exists() -> bool {
        Self::config_path().exists()
    }

    /// Ensure all distill directories exist
    pub fn ensure_dirs() -> Result<()> {
        for dir in [
            Self::base_dir(),
            Self::proposals_dir(),
            Self::skills_dir(),
            Self::history_dir(),
        ] {
            fs::create_dir_all(&dir)
                .with_context(|| format!("Failed to create directory: {}", dir.display()))?;
        }
        Ok(())
    }

    /// Load config from disk
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        let contents =
            fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;
        let config: Config =
            serde_yaml::from_str(&contents).with_context(|| "Failed to parse config.yaml")?;
        Ok(config)
    }

    /// Save config to disk
    pub fn save(&self) -> Result<()> {
        Self::ensure_dirs()?;
        let path = Self::config_path();
        let yaml = serde_yaml::to_string(self).context("Failed to serialize config")?;
        fs::write(&path, yaml)
            .with_context(|| format!("Failed to write {}", path.display()))?;
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

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.scan_interval, Interval::Weekly);
        assert_eq!(config.notifications, NotificationPref::Both);
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
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.scan_interval, Interval::Daily);
        assert_eq!(config.notifications, NotificationPref::Terminal);
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
}
