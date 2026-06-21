/* Benchmark harness for pyrocko's real parstackf seismic-stacking kernel, to
   measure whether OpenMP actually speeds up pyrocko compute in the pthread
   Pyodide. The kernel itself is NOT vendored here (it is GPL pyrocko code);
   bench/run.sh extracts parstackf + parstack_config from the fetched
   sources/pyrocko/src/ext/parstack_ext.c at build time and links them in. This
   file only synthesizes input, times the kernel at a given thread count, and
   exposes it as a Python C-extension.

   bench(nparallel, narrays, nshifts, nsamp, method, reps) ->
       (ms_per_rep, checksum, num_threads_seen) */
#define PY_SSIZE_T_CLEAN
#include <Python.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* defined in the extracted pyrocko kernel (kernel.c) */
extern int parstack_config(size_t narrays, int32_t *offsets, size_t *lengths,
                           size_t nshifts, int32_t *shifts, size_t *lengthout,
                           int32_t *offsetout);
extern int parstackf(size_t narrays, float **arrays, int32_t *offsets,
                     size_t *lengths, size_t nshifts, int32_t *shifts,
                     float *weights, int method, size_t lengthout,
                     int32_t offsetout, float *result, int nparallel);
/* libomp (in the main module) */
extern void omp_set_num_threads(int);
extern int omp_get_max_threads(void);

/* parstackf (method 1) calls fmaxf; pyodide's main module strips it (unused by
   core), so a thin side module can't resolve it. Provide it here for the
   benchmark. (A real pyrocko package build resolves libm against the full libc
   the package toolchain links.) */
float fmaxf(float a, float b) { return a > b ? a : b; }

static double now_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (double)t.tv_sec * 1e3 + (double)t.tv_nsec / 1e6;
}

static PyObject *bench(PyObject *self, PyObject *args) {
    int nparallel = 1, narrays = 40, nshifts = 20000, nsamp = 1000, method = 1,
        reps = 3;
    if (!PyArg_ParseTuple(args, "i|iiiii", &nparallel, &narrays, &nshifts,
                          &nsamp, &method, &reps))
        return NULL;

    float **arrays = malloc((size_t)narrays * sizeof(float *));
    int32_t *offsets = calloc((size_t)narrays, sizeof(int32_t));
    size_t *lengths = malloc((size_t)narrays * sizeof(size_t));
    for (int a = 0; a < narrays; a++) {
        lengths[a] = (size_t)nsamp;
        arrays[a] = malloc((size_t)nsamp * sizeof(float));
        uint32_t s = (uint32_t)(a * 2654435761u + 1u);
        for (int i = 0; i < nsamp; i++) {
            s = s * 1103515245u + 12345u;
            arrays[a][i] = (float)((s >> 16) & 0x7fff) / 32768.0f - 0.5f;
        }
    }
    size_t nsw = (size_t)nshifts * (size_t)narrays;
    int32_t *shifts = malloc(nsw * sizeof(int32_t));
    float *weights = malloc(nsw * sizeof(float));
    for (size_t k = 0; k < nsw; k++) {
        shifts[k] = (int32_t)(k % 53);
        weights[k] = 1.0f;
    }

    size_t lengthout;
    int32_t offsetout;
    parstack_config((size_t)narrays, offsets, lengths, (size_t)nshifts, shifts,
                    &lengthout, &offsetout);
    size_t resn =
        (method == 0) ? (size_t)nshifts * lengthout : (size_t)nshifts;
    float *result = calloc(resn, sizeof(float));

    omp_set_num_threads(nparallel);
    int seen = 0;
    double ms = 0.0;
    double checksum = 0.0;
    Py_BEGIN_ALLOW_THREADS
    /* warmup (spawns/parks the worker pool so we time compute, not spawn) */
    memset(result, 0, resn * sizeof(float));
    parstackf((size_t)narrays, arrays, offsets, lengths, (size_t)nshifts, shifts,
              weights, method, lengthout, offsetout, result, nparallel);
    double t0 = now_ms();
    for (int r = 0; r < reps; r++) {
        memset(result, 0, resn * sizeof(float));
        parstackf((size_t)narrays, arrays, offsets, lengths, (size_t)nshifts,
                  shifts, weights, method, lengthout, offsetout, result,
                  nparallel);
    }
    ms = (now_ms() - t0) / (double)reps;
    seen = omp_get_max_threads();
    Py_END_ALLOW_THREADS

    for (size_t i = 0; i < resn; i++) checksum += result[i];

    for (int a = 0; a < narrays; a++) free(arrays[a]);
    free(arrays);
    free(offsets);
    free(lengths);
    free(shifts);
    free(weights);
    free(result);
    return Py_BuildValue("(ddi)", ms, checksum, seen);
}

static PyMethodDef methods[] = {
    {"bench", bench, METH_VARARGS,
     "bench(nparallel[,narrays,nshifts,nsamp,method,reps]) -> (ms, checksum, max_threads)"},
    {NULL, NULL, 0, NULL}};
static struct PyModuleDef mod = {PyModuleDef_HEAD_INIT, "parbench", NULL, -1,
                                 methods};
PyMODINIT_FUNC PyInit_parbench(void) { return PyModule_Create(&mod); }
