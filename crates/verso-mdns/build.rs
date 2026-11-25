use std::path::PathBuf;

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
}
