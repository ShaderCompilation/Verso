//! Raw FFI bindings to FBInk. Regenerate with `scripts/gen-fbink-bindings.sh`.
//! Only available on ARM (e-reader) targets.
#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code)]
#![no_std]

#[cfg(target_arch = "arm")]
include!("bindings.rs");
