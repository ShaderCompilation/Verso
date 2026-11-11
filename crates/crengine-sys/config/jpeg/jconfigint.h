/* libjpeg-turbo internal configuration (replaces the CMake-generated file). */
#define BUILD  "verso"
#define HIDDEN  __attribute__((visibility("hidden")))
#undef inline
#define INLINE  __inline__ __attribute__((always_inline))
#define THREAD_LOCAL  __thread
#define PACKAGE_NAME  "libjpeg-turbo"
#define VERSION  "3.2.0"
#if defined(__SIZEOF_SIZE_T__) && __SIZEOF_SIZE_T__ == 8
#define SIZEOF_SIZE_T  8
#else
#define SIZEOF_SIZE_T  4
#endif
#define HAVE_BUILTIN_CTZL
#if defined(__has_attribute)
#if __has_attribute(fallthrough)
#define FALLTHROUGH  __attribute__((fallthrough));
#else
#define FALLTHROUGH
#endif
#else
#define FALLTHROUGH
#endif
#ifndef BITS_IN_JSAMPLE
#define BITS_IN_JSAMPLE  8
#endif
#undef C_ARITH_CODING_SUPPORTED
#undef D_ARITH_CODING_SUPPORTED
#undef WITH_SIMD
#if BITS_IN_JSAMPLE == 8
#define D_ARITH_CODING_SUPPORTED 1
#endif
