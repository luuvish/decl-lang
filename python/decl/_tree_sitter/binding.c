/* Python binding for the Decl tree-sitter grammar: exposes the language
 * as a PyCapsule the `tree_sitter` package accepts. The grammar sources
 * (parser.c, scanner.c) are synced from ../../tree-sitter-decl/src. */
#include <Python.h>

typedef struct TSLanguage TSLanguage;
TSLanguage *tree_sitter_decl(void);

static PyObject *binding_language(PyObject *self, PyObject *args) {
    (void)self; (void)args;
    return PyCapsule_New(tree_sitter_decl(), "tree_sitter.Language", NULL);
}

static PyMethodDef methods[] = {
    {"language", binding_language, METH_NOARGS, "Get the tree-sitter language for Decl."},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef module = {
    PyModuleDef_HEAD_INIT, "_binding", "Decl tree-sitter grammar", -1, methods,
    NULL, NULL, NULL, NULL
};

PyMODINIT_FUNC PyInit__binding(void) {
    return PyModule_Create(&module);
}
