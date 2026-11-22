use std::path::Path;
use std::process::Command;

fn main() {
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
