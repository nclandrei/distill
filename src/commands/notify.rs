use crate::config::Config;
use crate::config::NotificationPref;
use crate::notify::print_terminal_branding;
use crate::notify::send_notification;
use anyhow::Result;

pub fn run(check: bool) -> Result<()> {
    if !check {
        println!("Usage: distill notify --check");
        return Ok(());
    }

    let proposals_dir = Config::proposals_dir();
    if !proposals_dir.exists() {
        return Ok(());
    }

    let entries: Vec<_> = std::fs::read_dir(&proposals_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();

    if entries.is_empty() {
        return Ok(());
    }

    let count = entries.len();
    if let Ok(config) = Config::load() {
        let body = format!("{count} new proposal(s) ready. Run 'distill review'.");
        return match config.notifications {
            NotificationPref::None => Ok(()),
            NotificationPref::Terminal => {
                print_pending_proposals(count, config.notification_icon.as_deref());
                Ok(())
            }
            NotificationPref::Native => send_notification(
                &NotificationPref::Native,
                "distill",
                &body,
                config.notification_icon.as_deref(),
            ),
            NotificationPref::Both => {
                print_pending_proposals(count, config.notification_icon.as_deref());
                send_notification(
                    &NotificationPref::Native,
                    "distill",
                    &body,
                    config.notification_icon.as_deref(),
                )
            }
        };
    }

    // Fallback for pre-onboarding setups where config is not available yet.
    print_pending_proposals(count, None);
    Ok(())
}

fn print_pending_proposals(count: usize, icon_path: Option<&str>) {
    let _ = print_terminal_branding(icon_path);
    println!(
        "[distill] {count} new proposal{} ready",
        if count == 1 { "" } else { "s" }
    );
    println!("          Run 'distill review' to review them.");
}

#[cfg(test)]
mod tests {
    use super::print_pending_proposals;

    #[test]
    fn test_print_pending_proposals_runs_for_single_and_plural() {
        print_pending_proposals(1, None);
        print_pending_proposals(3, None);
    }

    #[test]
    fn test_print_pending_proposals_zero_is_still_safe() {
        print_pending_proposals(0, None);
    }
}
