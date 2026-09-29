//! Decodes the window icon, so the launcher embeds its pixels rather than a
//! PNG decoder, and gives the Windows executable its icon and file details,
//! which Explorer, shortcuts and the task manager show.

use std::path::PathBuf;
use std::{env, fs};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo::rerun-if-changed=assets/old_launcher.png");
    println!("cargo::rerun-if-changed=assets/old_launcher.ico");
    println!("cargo::rerun-if-changed=Cargo.toml");

    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);

    let icon = fs::read("assets/old_launcher.png")?;
    let mut decoder = png::Decoder::new(std::io::Cursor::new(icon));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info()?;
    let mut pixels = vec![0; reader.output_buffer_size().ok_or("the icon is too big")?];
    let frame = reader.next_frame(&mut pixels)?;

    if (frame.color_type, frame.bit_depth) != (png::ColorType::Rgba, png::BitDepth::Eight) {
        return Err("assets/old_launcher.png must be 8-bit RGBA".into());
    }
    pixels.truncate(frame.line_size * frame.height as usize);

    fs::write(out.join("icon.rgba"), &pixels)?;
    fs::write(
        out.join("icon_size.rs"),
        format!(
            "const ICON_WIDTH: u32 = {};\nconst ICON_HEIGHT: u32 = {};\n",
            frame.width, frame.height
        ),
    )?;

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("assets/old_launcher.ico")
            .set("FileDescription", "Old Launcher")
            .set("ProductName", "Old Launcher");
        resource.compile()?;
    }

    Ok(())
}
