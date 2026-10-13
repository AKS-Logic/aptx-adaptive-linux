//! Runs the offline test of the WirePlumber guard script, `wireplumber/test-guard.lua`, with
//! a system `lua`, if there is one.

use std::process::Command;

#[test]
fn wireplumber_guard_script() {
    let test = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../wireplumber/test-guard.lua"
    );
    let out = match Command::new("lua").arg(test).output() {
        Ok(out) => out,
        Err(e) => {
            eprintln!("skipped: no lua ({e})");
            return;
        }
    };
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
