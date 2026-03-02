use crate::config::Config;
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
    println!(
        "distill: {count} new proposal{} ready",
        if count == 1 { "" } else { "s" }
    );
    println!("         Run 'distill review' to review them.");
    Ok(())
}
