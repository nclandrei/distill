use anyhow::Result;

use crate::config::Config;
use crate::run_history;

pub fn run() -> Result<()> {
    let history_path = Config::run_history_path();
    if !history_path.exists() {
        println!("No recorded runs yet.");
        return Ok(());
    }

    let history_dir = Config::history_dir();
    let records = run_history::load_run_records(&history_dir)?;
    if records.is_empty() {
        println!("No recorded runs yet.");
        return Ok(());
    }

    run_history::run_history_interactive(records)
}
