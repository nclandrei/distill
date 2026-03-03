// Scheduler — launchd (macOS) / systemd (Linux) installer.

use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::config::Interval;

// ─── Status ───────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub enum SchedulerStatus {
    Installed,
    NotInstalled,
}

// ─── Trait ────────────────────────────────────────────────────────────────────

pub trait Scheduler {
    fn install(&self, interval: &Interval) -> Result<()>;
    fn uninstall(&self) -> Result<()>;
    fn status(&self) -> Result<SchedulerStatus>;
    /// Returns the primary file path managed by this scheduler
    /// (plist on macOS, timer unit on Linux).
    fn plist_or_unit_path(&self) -> PathBuf;
}

// ─── macOS launchd ────────────────────────────────────────────────────────────

#[cfg(any(not(target_os = "linux"), test))]
pub struct LaunchdScheduler {
    home: PathBuf,
    launchctl_path: PathBuf,
}

#[cfg(any(not(target_os = "linux"), test))]
impl LaunchdScheduler {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            launchctl_path: std::env::var_os("DISTILL_LAUNCHCTL_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("launchctl")),
        }
    }

    #[cfg(test)]
    fn with_launchctl_path(home: PathBuf, launchctl_path: PathBuf) -> Self {
        Self {
            home,
            launchctl_path,
        }
    }

    fn run_launchctl(&self, action: &str, plist_path: &PathBuf) -> Result<()> {
        let output = Command::new(&self.launchctl_path)
            .arg(action)
            .arg(plist_path)
            .output()
            .with_context(|| {
                format!("Failed to run {} {}", self.launchctl_path.display(), action)
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            anyhow::bail!(
                "{} {} failed with status {}{}",
                self.launchctl_path.display(),
                action,
                output.status,
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            );
        }

        Ok(())
    }
}

#[cfg(any(not(target_os = "linux"), test))]
impl Scheduler for LaunchdScheduler {
    fn plist_or_unit_path(&self) -> PathBuf {
        self.home
            .join("Library")
            .join("LaunchAgents")
            .join("com.distill.agent.plist")
    }

    fn install(&self, interval: &Interval) -> Result<()> {
        let exe = std::env::current_exe()
            .unwrap_or_else(|_| PathBuf::from("distill"))
            .to_string_lossy()
            .to_string();

        let start_interval: u32 = match interval {
            Interval::Daily => 86400,
            Interval::Weekly => 604800,
            Interval::Monthly => 2592000,
        };

        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
    "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.distill.agent</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>scan</string>
    </array>
    <key>StartInterval</key>
    <integer>{start_interval}</integer>
    <key>RunAtLoad</key>
    <false/>
</dict>
</plist>
"#
        );

        let plist_path = self.plist_or_unit_path();
        if let Some(parent) = plist_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        fs::write(&plist_path, &plist)
            .with_context(|| format!("Failed to write plist: {}", plist_path.display()))?;
        self.run_launchctl("load", &plist_path)?;
        Ok(())
    }

    fn uninstall(&self) -> Result<()> {
        let plist_path = self.plist_or_unit_path();
        if plist_path.exists() {
            self.run_launchctl("unload", &plist_path)?;
            fs::remove_file(&plist_path)
                .with_context(|| format!("Failed to remove plist: {}", plist_path.display()))?;
        }
        Ok(())
    }

    fn status(&self) -> Result<SchedulerStatus> {
        if self.plist_or_unit_path().exists() {
            Ok(SchedulerStatus::Installed)
        } else {
            Ok(SchedulerStatus::NotInstalled)
        }
    }
}

// ─── Linux systemd ────────────────────────────────────────────────────────────

#[cfg(any(target_os = "linux", test))]
pub struct SystemdScheduler {
    home: PathBuf,
    systemctl_path: PathBuf,
}

#[cfg(any(target_os = "linux", test))]
impl SystemdScheduler {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            systemctl_path: std::env::var_os("DISTILL_SYSTEMCTL_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("systemctl")),
        }
    }

    #[cfg(test)]
    fn with_systemctl_path(home: PathBuf, systemctl_path: PathBuf) -> Self {
        Self {
            home,
            systemctl_path,
        }
    }

    fn unit_dir(&self) -> PathBuf {
        self.home.join(".config").join("systemd").join("user")
    }

    pub fn service_path(&self) -> PathBuf {
        self.unit_dir().join("distill.service")
    }

    pub fn timer_path(&self) -> PathBuf {
        self.unit_dir().join("distill.timer")
    }

    fn run_systemctl(&self, args: &[&str]) -> Result<()> {
        let output = Command::new(&self.systemctl_path)
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .output()
            .with_context(|| {
                format!(
                    "Failed to run {} {}",
                    self.systemctl_path.display(),
                    args.join(" ")
                )
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            anyhow::bail!(
                "{} {} failed with status {}{}",
                self.systemctl_path.display(),
                args.join(" "),
                output.status,
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            );
        }

        Ok(())
    }
}

#[cfg(any(target_os = "linux", test))]
impl Scheduler for SystemdScheduler {
    /// Returns the timer unit path (the primary managed file for systemd).
    fn plist_or_unit_path(&self) -> PathBuf {
        self.timer_path()
    }

    fn install(&self, interval: &Interval) -> Result<()> {
        let exe = std::env::current_exe()
            .unwrap_or_else(|_| PathBuf::from("distill"))
            .to_string_lossy()
            .to_string();

        let on_calendar = match interval {
            Interval::Daily => "*-*-* 09:00:00",
            Interval::Weekly => "Mon *-*-* 09:00:00",
            Interval::Monthly => "*-*-01 09:00:00",
        };

        let service = format!(
            "[Unit]\n\
             Description=Distill AI agent session scanner\n\
             \n\
             [Service]\n\
             Type=oneshot\n\
             ExecStart={exe} scan\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
        );

        let timer = format!(
            "[Unit]\n\
             Description=Distill scheduled scan timer\n\
             \n\
             [Timer]\n\
             OnCalendar={on_calendar}\n\
             Persistent=true\n\
             \n\
             [Install]\n\
             WantedBy=timers.target\n"
        );

        let service_path = self.service_path();
        let timer_path = self.timer_path();

        let unit_dir = self.unit_dir();
        fs::create_dir_all(&unit_dir)
            .with_context(|| format!("Failed to create directory: {}", unit_dir.display()))?;

        fs::write(&service_path, &service)
            .with_context(|| format!("Failed to write service unit: {}", service_path.display()))?;
        fs::write(&timer_path, &timer)
            .with_context(|| format!("Failed to write timer unit: {}", timer_path.display()))?;
        self.run_systemctl(&["--user", "daemon-reload"])?;
        self.run_systemctl(&["--user", "enable", "--now", "distill.timer"])?;
        Ok(())
    }

    fn uninstall(&self) -> Result<()> {
        let timer_path = self.timer_path();
        let service_path = self.service_path();
        let had_units = timer_path.exists() || service_path.exists();

        if had_units {
            self.run_systemctl(&["--user", "disable", "--now", "distill.timer"])?;
        }

        if timer_path.exists() {
            fs::remove_file(&timer_path).with_context(|| {
                format!("Failed to remove timer unit: {}", timer_path.display())
            })?;
        }
        if service_path.exists() {
            fs::remove_file(&service_path).with_context(|| {
                format!("Failed to remove service unit: {}", service_path.display())
            })?;
        }
        if had_units {
            self.run_systemctl(&["--user", "daemon-reload"])?;
        }

        Ok(())
    }

    fn status(&self) -> Result<SchedulerStatus> {
        if self.timer_path().exists() {
            Ok(SchedulerStatus::Installed)
        } else {
            Ok(SchedulerStatus::NotInstalled)
        }
    }
}

// ─── Factory ──────────────────────────────────────────────────────────────────

/// Create the platform-appropriate scheduler for the given home directory.
pub fn create_scheduler(home: PathBuf) -> Box<dyn Scheduler> {
    #[cfg(target_os = "linux")]
    {
        Box::new(SystemdScheduler::new(home))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Box::new(LaunchdScheduler::new(home))
    }
}

/// Create the platform-appropriate scheduler using the real home directory.
pub fn create_scheduler_default() -> Box<dyn Scheduler> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    create_scheduler(home)
}

#[cfg(test)]
pub(crate) fn create_scheduler_for_tests(home: PathBuf) -> Box<dyn Scheduler> {
    #[cfg(target_os = "linux")]
    {
        Box::new(SystemdScheduler::with_systemctl_path(
            home,
            PathBuf::from("true"),
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Box::new(LaunchdScheduler::with_launchctl_path(
            home,
            PathBuf::from("true"),
        ))
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn launchd_scheduler_for_command_tests(home: PathBuf) -> LaunchdScheduler {
        LaunchdScheduler::with_launchctl_path(home, PathBuf::from("true"))
    }

    fn systemd_scheduler_for_command_tests(home: PathBuf) -> SystemdScheduler {
        SystemdScheduler::with_systemctl_path(home, PathBuf::from("true"))
    }

    // ── LaunchdScheduler ─────────────────────────────────────────────────────

    #[test]
    fn test_launchd_plist_path() {
        let home = PathBuf::from("/Users/testuser");
        let scheduler = LaunchdScheduler::new(home.clone());
        let expected = home
            .join("Library")
            .join("LaunchAgents")
            .join("com.distill.agent.plist");
        assert_eq!(scheduler.plist_or_unit_path(), expected);
    }

    #[test]
    fn test_launchd_install_creates_plist() {
        let dir = tempdir().unwrap();
        let scheduler = launchd_scheduler_for_command_tests(dir.path().to_path_buf());
        scheduler.install(&Interval::Daily).unwrap();
        assert!(scheduler.plist_or_unit_path().exists());
    }

    #[test]
    fn test_launchd_plist_contains_interval() {
        let dir = tempdir().unwrap();
        let scheduler = launchd_scheduler_for_command_tests(dir.path().to_path_buf());

        scheduler.install(&Interval::Daily).unwrap();
        let content = fs::read_to_string(scheduler.plist_or_unit_path()).unwrap();
        assert!(
            content.contains("<integer>86400</integer>"),
            "daily plist missing 86400"
        );

        scheduler.install(&Interval::Weekly).unwrap();
        let content = fs::read_to_string(scheduler.plist_or_unit_path()).unwrap();
        assert!(
            content.contains("<integer>604800</integer>"),
            "weekly plist missing 604800"
        );

        scheduler.install(&Interval::Monthly).unwrap();
        let content = fs::read_to_string(scheduler.plist_or_unit_path()).unwrap();
        assert!(
            content.contains("<integer>2592000</integer>"),
            "monthly plist missing 2592000"
        );
    }

    #[test]
    fn test_launchd_uninstall_removes_plist() {
        let dir = tempdir().unwrap();
        let scheduler = launchd_scheduler_for_command_tests(dir.path().to_path_buf());
        scheduler.install(&Interval::Weekly).unwrap();
        assert!(scheduler.plist_or_unit_path().exists());
        scheduler.uninstall().unwrap();
        assert!(!scheduler.plist_or_unit_path().exists());
    }

    #[test]
    fn test_launchd_status_not_installed() {
        let dir = tempdir().unwrap();
        let scheduler = LaunchdScheduler::new(dir.path().to_path_buf());
        assert_eq!(scheduler.status().unwrap(), SchedulerStatus::NotInstalled);
    }

    #[test]
    fn test_launchd_status_installed() {
        let dir = tempdir().unwrap();
        let scheduler = launchd_scheduler_for_command_tests(dir.path().to_path_buf());
        scheduler.install(&Interval::Weekly).unwrap();
        assert_eq!(scheduler.status().unwrap(), SchedulerStatus::Installed);
    }

    // ── SystemdScheduler ─────────────────────────────────────────────────────

    #[test]
    fn test_systemd_unit_paths() {
        let home = PathBuf::from("/home/testuser");
        let scheduler = SystemdScheduler::new(home.clone());
        let expected_service = home
            .join(".config")
            .join("systemd")
            .join("user")
            .join("distill.service");
        let expected_timer = home
            .join(".config")
            .join("systemd")
            .join("user")
            .join("distill.timer");
        assert_eq!(scheduler.service_path(), expected_service);
        assert_eq!(scheduler.plist_or_unit_path(), expected_timer);
    }

    #[test]
    fn test_systemd_install_creates_files() {
        let dir = tempdir().unwrap();
        let scheduler = systemd_scheduler_for_command_tests(dir.path().to_path_buf());
        scheduler.install(&Interval::Daily).unwrap();
        assert!(scheduler.service_path().exists(), "service file missing");
        assert!(scheduler.timer_path().exists(), "timer file missing");
    }

    #[test]
    fn test_systemd_timer_contains_calendar() {
        let dir = tempdir().unwrap();
        let scheduler = systemd_scheduler_for_command_tests(dir.path().to_path_buf());

        scheduler.install(&Interval::Daily).unwrap();
        let content = fs::read_to_string(scheduler.timer_path()).unwrap();
        assert!(
            content.contains("OnCalendar=*-*-* 09:00:00"),
            "daily timer missing correct OnCalendar"
        );

        scheduler.install(&Interval::Weekly).unwrap();
        let content = fs::read_to_string(scheduler.timer_path()).unwrap();
        assert!(
            content.contains("OnCalendar=Mon *-*-* 09:00:00"),
            "weekly timer missing correct OnCalendar"
        );

        scheduler.install(&Interval::Monthly).unwrap();
        let content = fs::read_to_string(scheduler.timer_path()).unwrap();
        assert!(
            content.contains("OnCalendar=*-*-01 09:00:00"),
            "monthly timer missing correct OnCalendar"
        );
    }

    #[test]
    fn test_systemd_uninstall_removes_files() {
        let dir = tempdir().unwrap();
        let scheduler = systemd_scheduler_for_command_tests(dir.path().to_path_buf());
        scheduler.install(&Interval::Daily).unwrap();
        assert!(scheduler.service_path().exists());
        assert!(scheduler.timer_path().exists());
        scheduler.uninstall().unwrap();
        assert!(
            !scheduler.service_path().exists(),
            "service file still exists after uninstall"
        );
        assert!(
            !scheduler.timer_path().exists(),
            "timer file still exists after uninstall"
        );
    }

    #[test]
    fn test_systemd_status_not_installed() {
        let dir = tempdir().unwrap();
        let scheduler = SystemdScheduler::new(dir.path().to_path_buf());
        assert_eq!(scheduler.status().unwrap(), SchedulerStatus::NotInstalled);
    }

    #[test]
    fn test_systemd_status_installed() {
        let dir = tempdir().unwrap();
        let scheduler = systemd_scheduler_for_command_tests(dir.path().to_path_buf());
        scheduler.install(&Interval::Weekly).unwrap();
        assert_eq!(scheduler.status().unwrap(), SchedulerStatus::Installed);
    }

    #[test]
    fn test_launchd_plist_is_valid_xml() {
        let dir = tempdir().unwrap();
        let scheduler = launchd_scheduler_for_command_tests(dir.path().to_path_buf());
        scheduler.install(&Interval::Daily).unwrap();
        let content = fs::read_to_string(scheduler.plist_or_unit_path()).unwrap();
        assert!(
            content.starts_with("<?xml"),
            "plist does not start with XML declaration"
        );
        assert!(
            content.contains("<plist version=\"1.0\">"),
            "missing plist root element"
        );
        assert!(content.contains("</plist>"), "plist missing closing tag");
        assert!(
            content.contains("com.distill.agent"),
            "plist missing agent label"
        );
    }

    #[test]
    fn test_systemd_service_contains_exec_start() {
        let dir = tempdir().unwrap();
        let scheduler = systemd_scheduler_for_command_tests(dir.path().to_path_buf());
        scheduler.install(&Interval::Daily).unwrap();
        let content = fs::read_to_string(scheduler.service_path()).unwrap();
        assert!(
            content.contains("ExecStart="),
            "service missing ExecStart directive"
        );
        assert!(
            content.contains("scan"),
            "service ExecStart missing 'scan' subcommand"
        );
        assert!(
            content.contains("[Service]"),
            "service missing [Service] section"
        );
        assert!(content.contains("[Unit]"), "service missing [Unit] section");
    }

    #[test]
    fn test_uninstall_is_idempotent_launchd() {
        let dir = tempdir().unwrap();
        let scheduler = LaunchdScheduler::new(dir.path().to_path_buf());
        // Uninstall without prior install — must not panic or error.
        scheduler.uninstall().unwrap();
    }

    #[test]
    fn test_uninstall_is_idempotent_systemd() {
        let dir = tempdir().unwrap();
        let scheduler = systemd_scheduler_for_command_tests(dir.path().to_path_buf());
        // Uninstall without prior install — must not panic or error.
        scheduler.uninstall().unwrap();
    }

    #[test]
    fn test_launchd_install_fails_if_launchctl_fails() {
        let dir = tempdir().unwrap();
        let scheduler =
            LaunchdScheduler::with_launchctl_path(dir.path().to_path_buf(), PathBuf::from("false"));
        assert!(scheduler.install(&Interval::Daily).is_err());
    }

    #[test]
    fn test_systemd_install_fails_if_systemctl_fails() {
        let dir = tempdir().unwrap();
        let scheduler =
            SystemdScheduler::with_systemctl_path(dir.path().to_path_buf(), PathBuf::from("false"));
        assert!(scheduler.install(&Interval::Daily).is_err());
    }
}
