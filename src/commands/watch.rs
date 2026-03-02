use anyhow::Result;

use crate::config::Config;
use crate::schedule;

pub fn run(install: bool, uninstall: bool) -> Result<()> {
    let scheduler = schedule::create_scheduler_default();
    if install {
        let config = Config::load()?;
        scheduler.install(&config.scan_interval)?;
        println!("Scheduler installed.");
    } else if uninstall {
        scheduler.uninstall()?;
        println!("Scheduler removed.");
    } else {
        let status = scheduler.status()?;
        println!("Scheduler status: {:?}", status);
    }
    Ok(())
}
