//! Verso — a calm, lightweight e-reader.
//!
//! Skeleton only. See docs/plan.md for the implementation plan.

#[cfg(all(feature = "desktop", feature = "kindle"))]
compile_error!("Enable exactly one backend: `desktop` (default) or `kindle`.");

fn main() {
    println!("Verso {} — backend: {}", env!("CARGO_PKG_VERSION"), backend_name());
}

#[cfg(feature = "desktop")]
fn backend_name() -> String {
    "desktop simulator".into()
}

#[cfg(feature = "kindle")]
fn backend_name() -> String {
    // Proves FBInk is compiled in and linked.
    let v = unsafe { std::ffi::CStr::from_ptr(fbink_sys::fbink_version()) };
    format!("kindle (FBInk {})", v.to_string_lossy())
}
