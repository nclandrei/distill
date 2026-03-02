use anyhow::Result;

use crate::config::Config;
use crate::review;

pub fn run() -> Result<()> {
    let proposals_dir = Config::proposals_dir();
    let skills_dir = Config::skills_dir();
    let history_dir = Config::history_dir();

    // Quick early-exit: avoid entering the interactive loop when there is
    // nothing to review.
    let proposals = review::load_proposals(&proposals_dir)?;
    if proposals.is_empty() {
        println!("No pending proposals to review.");
        return Ok(());
    }

    review::run_review_interactive(&proposals_dir, &skills_dir, &history_dir)?;
    Ok(())
}
