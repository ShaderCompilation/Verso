// Verso's shim over Hunspell's C API: the two calls that parse files or
// allocate a lot catch every C++ exception (std::bad_alloc on a large
// dictionary, std::length_error from a malformed one), so none unwinds into
// Rust, where that would be undefined behaviour. See src/lib.rs.

#include <hunspell.h>

extern "C" {

// Hunspell_create, or NULL if it threw.
Hunhandle* verso_hunspell_create(const char* aff, const char* dic) {
  try {
    return Hunspell_create(aff, dic);
  } catch (...) {
    return nullptr;
  }
}

// Hunspell_stem, or -1 (with *list NULL) if it threw.
int verso_hunspell_stem(Hunhandle* h, char*** list, const char* word) {
  *list = nullptr;
  try {
    return Hunspell_stem(h, list, word);
  } catch (...) {
    *list = nullptr;
    return -1;
  }
}

}
