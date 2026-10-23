//! Embed `assets/app-icon.ico` as the Windows executable icon.
//! No-op on other platforms.
fn main() {
    #[cfg(target_os = "windows")]
    {
        embed_resource::compile("assets/app-icon.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
        // Recompile when the icon or the .rc changes.
        println!("cargo:rerun-if-changed=assets/app-icon.rc");
        println!("cargo:rerun-if-changed=assets/app-icon.ico");
    }
}
