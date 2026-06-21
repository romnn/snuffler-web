/* Smoke test: does an *imported* C-extension module (not a ctypes CDLL) run
   real, concurrent OS threads in the pthread-enabled Pyodide?

   Builds as an emscripten SIDE_MODULE, is imported as `pthreadext`, and spawns
   N pthreads that each bump a shared atomic while sleeping, tracking the peak
   number running at once. A max_concurrent == N proves genuine parallelism (not
   serialized execution). See run.sh / README.md. */
#define PY_SSIZE_T_CLEAN
#include <Python.h>
#include <pthread.h>
#include <stdatomic.h>
#include <unistd.h>

static atomic_int completed;
static atomic_int current;
static atomic_int max_concurrent;

static void *worker(void *arg) {
    (void)arg;
    int c = atomic_fetch_add(&current, 1) + 1;
    int m = atomic_load(&max_concurrent);
    while (c > m && !atomic_compare_exchange_weak(&max_concurrent, &m, c)) {}
    usleep(80000); /* hold so siblings overlap */
    atomic_fetch_add(&completed, 1);
    atomic_fetch_sub(&current, 1);
    return NULL;
}

static PyObject *run(PyObject *self, PyObject *args) {
    int n = 4;
    if (!PyArg_ParseTuple(args, "|i", &n)) return NULL;
    if (n > 64) n = 64;
    atomic_store(&completed, 0);
    atomic_store(&current, 0);
    atomic_store(&max_concurrent, 0);
    pthread_t th[64];
    int made = 0;
    Py_BEGIN_ALLOW_THREADS
    for (int i = 0; i < n; i++) {
        if (pthread_create(&th[i], NULL, worker, NULL) == 0) made++;
    }
    for (int i = 0; i < made; i++) pthread_join(th[i], NULL);
    Py_END_ALLOW_THREADS
    return Py_BuildValue("(iii)", made, atomic_load(&completed),
                         atomic_load(&max_concurrent));
}

static PyMethodDef methods[] = {
    {"run", run, METH_VARARGS,
     "spawn n pthreads; return (created, completed, max_concurrent)"},
    {NULL, NULL, 0, NULL}};
static struct PyModuleDef mod = {PyModuleDef_HEAD_INIT, "pthreadext", NULL, -1,
                                 methods};
PyMODINIT_FUNC PyInit_pthreadext(void) { return PyModule_Create(&mod); }
