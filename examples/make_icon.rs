//! One-off tool: encode `assets/app-icon.png` into the multi-size
//! `assets/app-icon.ico` embedded in the Windows executable.
//! Run with `cargo run --example make_icon`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let png = image::load_from_memory(include_bytes!("../assets/app-icon.png"))?
        .to_rgba8();

    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16u32, 32, 48, 64, 128, 256] {
        let resized = image::imageops::resize(
            &png,
            size,
            size,
            image::imageops::FilterType::Lanczos3,
        );
        let icon = ico::IconImage::from_rgba_data(size, size, resized.into_raw());
        dir.add_entry(ico::IconDirEntry::encode(&icon)?);
    }

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/app-icon.ico");
    let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
    dir.write(&mut file)?;
    println!("wrote {path}");
    Ok(())
}
