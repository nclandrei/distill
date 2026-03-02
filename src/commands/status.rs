use crate::config::Config;
use anyhow::Result;

pub fn run() -> Result<()> {
    if !Config::exists() {
        println!("distill is not configured. Run 'distill' to start onboarding.");
        return Ok(());
    }

    let config = Config::load()?;

    println!("=== distill status ===");
    println!();
    println!(
        "Scan interval:  {:?}",
        config.scan_interval
    );
    println!("Proposal agent: {}", config.proposal_agent);
    println!("Shell:          {:?}", config.shell);
    println!("Notifications:  {:?}", config.notifications);
    println!();

    // Agents
    println!("Monitored agents:");
    for agent in &config.agents {
        let status = if agent.enabled { "enabled" } else { "disabled" };
        println!("  - {} ({})", agent.name, status);
    }
    println!();

    // Pending proposals
    let proposals_dir = Config::proposals_dir();
    let pending = if proposals_dir.exists() {
        std::fs::read_dir(&proposals_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .is_some_and(|ext| ext == "md")
            })
            .count()
    } else {
        0
    };
    println!("Pending proposals: {pending}");

    // Accepted skills
    let skills_dir = Config::skills_dir();
    let skills = if skills_dir.exists() {
        std::fs::read_dir(&skills_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .is_some_and(|ext| ext == "md")
            })
            .count()
    } else {
        0
    };
    println!("Accepted skills:   {skills}");

    // Last scan
    let last_scan_path = Config::last_scan_path();
    if last_scan_path.exists() {
        let contents = std::fs::read_to_string(&last_scan_path)?;
        println!("Last scan:         {contents}");
    } else {
        println!("Last scan:         never");
    }

    Ok(())
}
