/* Symbols the LLVM OpenMP runtime normally gets from z_Linux_asm.S, which is
   assembled only for x86/ARM/PPC/etc. — never for wasm (every block in that
   file is guarded by KMP_ARCH_<native>). The wasm microtask invoker is provided
   in C in z_Linux_util.cpp, but the GOMP-compat critical-section lock pointer is
   not, so kmp_gsupport.cpp's GOMP_atomic_start/critical helpers reference an
   undefined __kmp_unnamed_critical_addr. Without this the .so links but fails to
   load ("bad export type for '__kmp_unnamed_critical_addr': undefined").

   Mirror exactly what the native asm emits: a pointer to a 32-byte,
   zero-initialised lock (kmp_critical_name is kmp_int32[8]). C linkage, so the
   symbol name matches the (extern "C") reference in kmp_gsupport.cpp. */
typedef int kmp_critical_name[8];
static kmp_critical_name __kmp_gomp_critical_user;
kmp_critical_name *__kmp_unnamed_critical_addr = &__kmp_gomp_critical_user;
