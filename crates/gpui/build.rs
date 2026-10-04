#![allow(clippy::disallowed_methods, reason = "build scripts are exempt")]

fn main() {
    println!("cargo::rustc-check-cfg=cfg(gles)");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os == "windows" {
        #[cfg(feature = "windows-manifest")]
        embed_resource();
    }
}

#[cfg(feature = "windows-manifest")]
fn embed_resource() {
    let manifest = std::path::Path::new("resources/windows/gpui.manifest.xml");
    let rc_file = std::path::Path::new("resources/windows/gpui.rc");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed={}", rc_file.display());
    let rc_file = if cfg!(windows) {
        rc_file.to_path_buf()
    } else {
        rc_file_with_absolute_manifest_path(rc_file, manifest)
    };
    embed_resource::compile(rc_file, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}

// When cross-compiling, llvm-rc resolves paths relative to the .rc file.
#[cfg(feature = "windows-manifest")]
fn rc_file_with_absolute_manifest_path(
    rc_file: &std::path::Path,
    manifest: &std::path::Path,
) -> std::path::PathBuf {
    let relative_manifest = format!("\"{}\"", manifest.display());
    let absolute_manifest = format!("\"{}\"", std::fs::canonicalize(manifest).unwrap().display());
    let contents = std::fs::read_to_string(rc_file).unwrap();
    assert!(
        contents.contains(&relative_manifest),
        "{} must reference {relative_manifest}",
        rc_file.display()
    );
    let generated = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("gpui.rc");
    std::fs::write(
        &generated,
        contents.replace(&relative_manifest, &absolute_manifest),
    )
    .unwrap();
    generated
}
