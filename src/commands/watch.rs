use anyhow::Result;

pub fn run(install: bool, uninstall: bool) -> Result<()> {
    if install {
        println!("distill watch --install: scheduler installation not yet implemented.");
    } else if uninstall {
        println!("distill watch --uninstall: scheduler removal not yet implemented.");
    } else {
        println!("Usage: distill watch --install | --uninstall");
    }
    Ok(())
}
