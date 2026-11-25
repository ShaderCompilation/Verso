/* Keep .rel.plt contiguous with .rel.dyn on 32-bit ARM glibc builds.
 *
 * LLD otherwise places .ARM.exidx between .rel.dyn and .rel.plt. Combined with
 * Zig's default -z now (DT_FLAGS BIND_NOW), glibc <= 2.23 (Kindle Oasis 3
 * ships glibc 2.20) merges DT_JMPREL onto DT_REL without checking contiguity
 * (glibc bug #14341, fixed in 2.24), reads .ARM.exidx as Elf32_Rel entries,
 * and SIGSEGVs inside ld.so before _start runs.
 */
SECTIONS {
  .rel.plt : { *(.rel.plt) }
}
INSERT AFTER .rel.dyn;
