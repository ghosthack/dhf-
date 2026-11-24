#[path = "aot/spec.rs"]
mod spec;
#[path = "aot/translate.rs"]
mod translate;

use sha2::{Digest, Sha256};
use std::{env, fs, path::PathBuf};

fn main() {
    const LIBRARY: &str = "../app/src/main/libs/armeabi-v7a/libdhunter.so";
    println!("cargo:rerun-if-changed={LIBRARY}");
    println!("cargo:rerun-if-changed=aot/spec.rs");
    println!("cargo:rerun-if-changed=aot/translate.rs");

    let library = match fs::read(LIBRARY) {
        Ok(bytes) => Some(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => panic!("read the bundled ARM library: {err}"),
    };
    let generated = match library {
        Some(bytes) if format!("{:x}", Sha256::digest(&bytes)) == spec::LIB_SHA256 => {
            translate::generate(&bytes)
        }
        _ => "const TRANSLATIONS: &[Translation] = &[];\n".into(),
    };
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    fs::write(out.join("aot_blocks.rs"), generated).expect("write AOT translation");
}
