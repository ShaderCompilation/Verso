use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("arm")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu")
    {
        let script = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("../../.cargo/arm-glibc-relplt.ld")
            .canonicalize()
            .expect(".cargo/arm-glibc-relplt.ld missing");
        println!("cargo:rerun-if-changed={}", script.display());
        println!("cargo:rustc-link-arg-bins=-Wl,-T,{}", script.display());
    }

    let git_dir = Path::new("../../.git");
    let head = git_dir.join("HEAD");
    if head.exists() {
        println!("cargo:rerun-if-changed={}", head.display());
        if let Ok(contents) = std::fs::read_to_string(&head)
            && let Some(reference) = contents.trim().strip_prefix("ref: ")
        {
            let ref_path = git_dir.join(reference);
            if ref_path.exists() {
                println!("cargo:rerun-if-changed={}", ref_path.display());
            }
            let packed = git_dir.join("packed-refs");
            if packed.exists() {
                println!("cargo:rerun-if-changed={}", packed.display());
            }
        }
    }

    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=VERSO_GIT_COMMIT={commit}");
}
