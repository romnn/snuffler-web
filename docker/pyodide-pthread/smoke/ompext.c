/* Smoke test: does `#pragma omp` (OpenMP) work in an imported C-extension under
   the pthread-enabled Pyodide? This is the path pyrocko's kernels actually use
   (they gate parallel loops behind PYROCKO_OPENMP).

   Result (emscripten 4.0.9, see README.md): clang accepts -fopenmp and lowers
   the pragmas to the libomp ABI (__kmpc_*), but emscripten ships NO OpenMP
   runtime, so this module fails to load with `undefined symbol __kmpc_*`.
   Kept as a regression marker for if/when an emscripten libomp exists.

   This emsdk's LLVM build also omits omp.h from the clang resource dir, so we
   declare the runtime API ourselves; the #pragma omp directives are lowered by
   the compiler, not the header. */
#define PY_SSIZE_T_CLEAN
#include <Python.h>

extern int omp_get_num_threads(void);
extern int omp_get_max_threads(void);

static PyObject *run(PyObject *self, PyObject *args) {
    int want = 4;
    if (!PyArg_ParseTuple(args, "|i", &want)) return NULL;
    int observed = 0;
    long long acc = 0;
    Py_BEGIN_ALLOW_THREADS
    #pragma omp parallel num_threads(want) reduction(+ : acc)
    {
        #pragma omp single
        observed = omp_get_num_threads();
        for (long long i = 0; i < 2000000LL; i++) acc += i % 7;
    }
    Py_END_ALLOW_THREADS
    return Py_BuildValue("(iiL)", observed, omp_get_max_threads(), acc);
}

static PyMethodDef methods[] = {
    {"run", run, METH_VARARGS,
     "OpenMP parallel region; return (num_threads, max_threads, checksum)"},
    {NULL, NULL, 0, NULL}};
static struct PyModuleDef mod = {PyModuleDef_HEAD_INIT, "ompext", NULL, -1,
                                 methods};
PyMODINIT_FUNC PyInit_ompext(void) { return PyModule_Create(&mod); }
