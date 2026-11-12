//! With the `viewer` feature, the binary carries web/dist/site: refuse to build without it, and
//! rebuild when any file in it changes, a new one included.

use std::path::Path;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_VIEWER").is_none() {
        return;
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let site = Path::new(&manifest).join("../../web/dist/site");
    if !site.join("open.html").is_file() {
        println!(
            "cargo::error=the viewer feature builds {} into the binary, and it has no build: \
             run crates/wasm/build.sh and crates/convert-wasm/build.sh, then `npm run build` in web/",
            site.display()
        );
        return;
    }
    println!("cargo::rerun-if-changed={}", site.display());
}
