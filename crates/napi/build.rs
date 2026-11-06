//! Embed a build identity into the addon.
//!
//! The addon must be able to prove which checkout it came from, otherwise a
//! stale copy silently reproduces old behaviour. Both values are optional at
//! compile time (`option_env!`), so a build without git still works.

use std::{
    env,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn git_commit() -> String {
    let manifest_dir = match env::var("CARGO_MANIFEST_DIR") {
        Ok(dir) => dir,
        Err(_) => return "unknown".to_string(),
    };
    let head = match Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&manifest_dir).output() {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout).trim().to_string(),
        _ => return "unknown".to_string(),
    };
    if head.is_empty() {
        return "unknown".to_string();
    }
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(&manifest_dir)
        .output()
        .map(|output| output.status.success() && !output.stdout.is_empty())
        .unwrap_or(false);
    if dirty { format!("{head}-dirty") } else { head }
}

fn main() {
    let epoch = SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_secs()).unwrap_or(0);
    println!("cargo:rustc-env=WAE_APP_BUILD_COMMIT={}", git_commit());
    println!("cargo:rustc-env=WAE_APP_BUILD_EPOCH={epoch}");
}
