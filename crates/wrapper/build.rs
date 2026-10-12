//! Embeds the application icon into `wrapper.exe` (no-op on other targets).

fn main() {
    println!("cargo:rerun-if-changed=wrapper.rc");
    println!("cargo:rerun-if-changed=wrapper.ico");
    embed_resource::compile("wrapper.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
