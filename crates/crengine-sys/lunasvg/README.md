# LunaSVG (vendored, with KOReader's extensions)

crengine draws SVG (`<svg>` in a book, `<img src="x.svg">`) through LunaSVG.
Its `lvimg.cpp` expects KOReader's *extended* LunaSVG, which adds hooks for
`<text>` and `<image>` (crengine supplies the fonts and decodes the images).

## Where it came from

| Part | Source |
| --- | --- |
| `include/`, `source/`, `3rdparty/plutovg/`, `LICENSE` | [sammycage/lunasvg](https://github.com/sammycage/lunasvg) tag `v2.3.9` (commit `610b8bf5148a27489b4e3344b4f5617b81be38c7`), with KOReader's `extended.patch` applied |
| `xtended/`, `patches/extended.patch` | [koreader/koreader-base](https://github.com/koreader/koreader-base) `thirdparty/lunasvg/` at commit `a5d9be3dc77197a521e2a20bf7984ccf27a5d93f` ("LunaSVG xtended: fix handling of preserveAspectRatio", 2026-08-16, the last commit touching that folder when this was vendored on 2026-11-21) |

KOReader also applies `cmake_tweaks.patch` and `pkgconfig.patch`. They only
change `CMakeLists.txt`, which is not vendored: `../build.rs` compiles every
`.c` in `3rdparty/plutovg/` and every `.cpp` in `source/` and `xtended/`. So
keep only files that are built in those folders. Left out from upstream: CMake
files, `3rdparty/stb`, the `svg2png` example and the logo.

## Updating

1. Clone lunasvg at the tag koreader-base's `thirdparty/lunasvg/CMakeLists.txt`
   names, and apply its patches in the order that file lists them.
2. Replace `include/`, `source/` and `3rdparty/plutovg/` (sources, headers,
   `FTL.TXT`) with the patched copies, `xtended/` with koreader-base's
   `xtended/*.cpp` and `*.h`, and `patches/extended.patch` with the new patch.
3. Update the commits in the table above.

Do not edit the files here by hand. Verso-specific changes belong in
`../build.rs`, as for crengine.

## Licenses

- LunaSVG: MIT (`LICENSE`).
- plutovg: MIT, same author. Its rasterizer and stroker (`plutovg-ft-*`) come
  from FreeType under the FreeType License (`3rdparty/plutovg/FTL.TXT`).
- `xtended/` and `extended.patch`: part of koreader-base, which is
  AGPL-3.0. The files carry no headers of their own. Verso is AGPL-3.0 too.
