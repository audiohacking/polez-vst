//! Ensures the `third_party/polez` submodule is present before Cargo resolves path deps.

use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let polez_manifest = Path::new(&manifest_dir).join("third_party/polez/Cargo.toml");

    if polez_manifest.is_file() {
        println!("cargo:rerun-if-changed=third_party/polez/Cargo.toml");
        return;
    }

    println!("cargo:warning=polez submodule not found; initializing third_party/polez via git");

    let status = Command::new("git")
        .args([
            "submodule",
            "update",
            "--init",
            "--depth",
            "1",
            "third_party/polez",
        ])
        .current_dir(&manifest_dir)
        .status();

    match status {
        Ok(s) if s.success() && polez_manifest.is_file() => {
            println!("cargo:rerun-if-changed=third_party/polez/Cargo.toml");
        }
        _ => {
            panic!(
                "third_party/polez is missing.\n\
                 Clone this repository with submodules:\n\
                   git clone --recurse-submodules <repo-url>\n\
                 Or initialize manually:\n\
                   git submodule update --init --recursive"
            );
        }
    }
}
