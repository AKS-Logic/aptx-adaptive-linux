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

struct Section {
    name: &'static str,
    addr: u32,
    size: u32,
    sha256: &'static str,
}

/// §1 known-good build identity.
#[rustfmt::skip]
const IDENTITY: [Section; 4] = [
    Section { name: ".text", addr: 0x089A0, size: 96548, sha256: "bfa68d93528ec45041b56b4d008d6ea7e5441ddc97c7a436b33a7667da127788" },
    Section { name: ".rodata", addr: 0x20328, size: 100_980, sha256: "7eff359c762a4c44282848de93047a032ad004a5202c08448ac40f6936dbff9d" },
    Section { name: ".data.rel.ro", addr: 0x390A0, size: 572, sha256: "a88ed7e1052559407036e7b66303d72607ec2f53d4e3d9ea5a2d36c16a475727" },
    Section { name: ".data", addr: 0x392E0, size: 1920, sha256: "6f75e1707084bc195bcdb98f80618ce5d847afd5cfb54cb5831c165590b9121e" },
];

fn sha256(data: &[u8]) -> String {
    Sha256::digest(data).iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn is_known_good(elf: &elf::Elf) -> bool {
    IDENTITY.iter().all(|want| {
        elf.section(want.name).is_some_and(|s| {
            s.addr == want.addr
                && s.size == want.size
                && s.bytes.len() == want.size as usize
                && sha256(s.bytes) == want.sha256
        })
    })
}

fn tables(path: &Path) -> Result<String, String> {
    let data = fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let elf = elf::Elf::parse(&data).map_err(|e| format!("{}: {e}", path.display()))?;
    let known_good = is_known_good(&elf);
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
            "a file matching the known-good build identity"
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
