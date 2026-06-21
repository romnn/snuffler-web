/* Force the wasm-SJLJ __c_longjmp tag to be materialised + exported by the main
   module. Wasm-SJLJ side modules (matplotlib ft2font/_qhull, Pillow _imagingft,
   and the scipy/Pillow exts once rebuilt wasm-SJLJ) import env.__c_longjmp (a
   WebAssembly.Tag) from main; without a main TU that uses wasm setjmp/longjmp the
   tag is dead-code-eliminated and those .so fail to link. This tiny TU
   (compiled -sSUPPORT_LONGJMP=wasm -fwasm-exceptions) materialises it. */
#include <setjmp.h>
static jmp_buf __snuffler_jb;
__attribute__((used)) int __snuffler_force_sjlj(volatile int x) {
    if (setjmp(__snuffler_jb)) return 1;
    if (x) longjmp(__snuffler_jb, 1);
    return 0;
}
