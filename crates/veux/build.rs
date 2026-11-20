//! Embed static helper binaries when the build names them.
//!
//! `just release` sets `VEUX_EMBED_HELPER_AARCH64` and
//! `VEUX_EMBED_HELPER_X86_64` to cross-built musl binaries. Without them the
//! binary has no embedded helper, and remote hosts need `helper_path` or
//! `VEUX_HELPER_<ARCH>` at run time.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let mut code = String::new();
    for (name, var) in [
        ("AARCH64", "VEUX_EMBED_HELPER_AARCH64"),
        ("X86_64", "VEUX_EMBED_HELPER_X86_64"),
    ] {
        println!("cargo:rerun-if-env-changed={var}");
        match env::var(var) {
            Ok(path) => {
                let path = fs::canonicalize(&path).unwrap_or_else(|e| panic!("{var}={path}: {e}"));
                println!("cargo:rerun-if-changed={}", path.display());
                code.push_str(&format!(
                    "pub static {name}: Option<&[u8]> = Some(include_bytes!({:?}));\n",
                    path.display().to_string()
                ));
            }
            Err(_) => code.push_str(&format!("pub static {name}: Option<&[u8]> = None;\n")),
        }
    }
    let out = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    fs::write(out.join("helpers.rs"), code).expect("write helpers.rs");
}
