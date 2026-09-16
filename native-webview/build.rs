use std::{env, fs, path::PathBuf};

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let arch = match env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => panic!("unsupported arch: {other}"),
    };

    let metadata = cargo_metadata::MetadataCommand::new()
        .exec()
        .expect("failed to run cargo metadata");

    let pkg = metadata
        .packages
        .iter()
        .find(|p| p.name == "webview2-com-sys")
        .expect("webview2-com-sys not found");

    let src = pkg
        .manifest_path
        .parent()
        .unwrap()
        .join(arch)
        .join("WebView2Loader.dll");

    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out — walk up to target/<profile>/
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target_profile_dir = out_dir.ancestors().nth(3).unwrap();

    fs::copy(&src, target_profile_dir.join("WebView2Loader.dll"))
        .unwrap_or_else(|e| panic!("failed to copy {src:?}: {e}"));

    println!("cargo:rerun-if-changed=build.rs");
}
