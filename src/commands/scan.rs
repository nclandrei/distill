use anyhow::Result;

pub fn run(now: bool) -> Result<()> {
    if now {
        println!("distill scan: running immediate scan...");
    } else {
        println!("distill scan: scheduled scan not yet implemented.");
    }
    println!("Scan engine not yet implemented.");
    Ok(())
}
