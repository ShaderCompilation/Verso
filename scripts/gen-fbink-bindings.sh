#!/usr/bin/env bash
# Regenerates crates/fbink-sys/src/bindings.rs from FBInk's header.
# Generated for 32-bit ARM (the Kindle), since struct layouts differ from x86_64.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ZIG_LIB="$(zig env | sed -n 's/.*"lib_dir": *"\([^"]*\)".*/\1/p; s/.*\.lib_dir = "\([^"]*\)".*/\1/p' | head -1)"
bindgen "$ROOT/third_party/FBInk/fbink.h" \
  --allowlist-function 'fbink_.*' \
  --allowlist-type 'FBInk.*|.*_E|.*_T' \
  --allowlist-var 'FBINK_.*|WFM_.*|HWD_.*|NIGHTMODE_.*|INPUT_.*' \
  --no-layout-tests \
  --use-core \
  -- --target=arm-linux-gnueabihf \
     -DFBINK_FOR_KINDLE \
     -isystem "$ZIG_LIB/include" \
     -isystem "$ZIG_LIB/libc/include/arm-linux-gnu" \
     -isystem "$ZIG_LIB/libc/include/generic-glibc" \
     -isystem "$ZIG_LIB/libc/include/arm-linux-any" \
     -isystem "$ZIG_LIB/libc/include/any-linux-any" \
  > "$ROOT/crates/fbink-sys/src/bindings.rs"
echo "wrote crates/fbink-sys/src/bindings.rs ($(wc -l < "$ROOT/crates/fbink-sys/src/bindings.rs") lines)"
