//! Gives the Windows executable its icon and file details, which Explorer,
//! shortcuts and the task manager show. Other targets need nothing here.

fn main() -> std::io::Result<()> {
    println!("cargo::rerun-if-changed=assets/old_launcher.ico");
    println!("cargo::rerun-if-changed=Cargo.toml");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("assets/old_launcher.ico")
            .set("FileDescription", "Old Launcher")
            .set("ProductName", "Old Launcher");
        resource.compile()?;
    }

    Ok(())
}
