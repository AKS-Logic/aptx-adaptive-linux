//! Extracts the codec tables from the user's copy of the vendor encoder library and writes
//! them to `$OUT_DIR/tables.rs` (`spec/table-extraction.md`).

#[path = "build/elf.rs"]
mod elf;
#[path = "build/emit.rs"]
mod emit;
#[path = "build/extract.rs"]
mod extract;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

use sha2::{Digest, Sha256};

const ENV: &str = "APTX_ADAPTIVE_SO";

const HOW_TO_GET: &str = "\
Set APTX_ADAPTIVE_SO to the absolute path of libaptXAdaptiveEnc.so. README.md \"install\" says \
where to get it; with mise, .scratch/libaptXAdaptiveEnc.so in the repo is the default.";

/// §5: `<table id> <SHA-256 of its canonical form>` per line.
const HASHES: &str = include_str!("build/hashes.txt");

const KNOWN_GOOD: &str = "1ac881571cd1db4d256113d4448304016de73fedebc059824b1b2ed09599f0c9";

fn sha256(data: &[u8]) -> String {
    Sha256::digest(data).iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn tables(path: &Path) -> Result<String, String> {
    let data = fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let known_good = sha256(&data) == KNOWN_GOOD;
    let elf = elf::Elf::parse(&data).map_err(|e| format!("{}: {e}", path.display()))?;
    let tables = extract::extract(&elf, known_good).map_err(|e| {
        format!(
            "{}: {e}. Only libaptXAdaptiveEnc.so (aptX Adaptive R1/R2) is supported, not the R3 \
             or R4 libraries (spec/table-extraction.md §1)",
            path.display()
        )
    })?;

    let mut mismatched = Vec::new();
    for (id, want) in HASHES.lines().filter_map(|l| l.split_once(' ')) {
        let values = tables
            .get(id)
            .ok_or_else(|| format!("internal error: table `{id}` was not extracted"))?;
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        if sha256(&bytes) != want {
            mismatched.push(id);
        }
    }
    if !mismatched.is_empty() {
        let kind = if known_good {
            "the known-good build"
        } else {
            "this unrecognised build"
        };
        return Err(format!(
            "{}: {} of {} tables don't match the hashes in spec/table-extraction.md §5 for {kind}: {}. \
             The file is a different or modified encoder and can't be used",
            path.display(),
            mismatched.len(),
            HASHES.lines().count(),
            mismatched.join(", ")
        ));
    }
    Ok(emit::rust(&tables))
}

fn run() -> Result<(), String> {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=build");
    println!("cargo::rerun-if-env-changed={ENV}");
    let path = match env::var_os(ENV) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => return Err(format!("{ENV} is not set. {HOW_TO_GET}")),
    };
    println!("cargo::rerun-if-changed={}", path.display());
    if !path.is_absolute() {
        return Err(format!(
            "{ENV}={} is relative; give an absolute path. {HOW_TO_GET}",
            path.display()
        ));
    }
    if !path.is_file() {
        return Err(format!(
            "{ENV}={} doesn't exist. {HOW_TO_GET}",
            path.display()
        ));
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
    fs::write(out.join("tables.rs"), tables(&path)?)
        .map_err(|e| format!("can't write tables.rs: {e}"))
}

fn main() {
    if let Err(e) = run() {
        println!("cargo::error={e}");
    }
}
