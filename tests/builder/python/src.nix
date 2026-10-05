{
  "pyproject.toml" = ''
    [build-system]
    requires      = [ "setuptools" ]
    build-backend = "setuptools.build_meta"

    [project]
    name    = "tpy"
    version = "0.3"

    [project.scripts]
    tpy-hello = "tpy:main"

    [tool.setuptools]
    packages    = [ "tpy" ]
    ext-modules = [ { name = "tpy._speed", sources = [ "tpy/_speed.c" ] } ]
  '';
  "tpy/__init__.py" = ''
    from tpy._speed import answer


    def main() -> None:
        print(f"tpy-hello 0.3 {answer()}")
  '';
  "tpy/_speed.c" = ''
    #define PY_SSIZE_T_CLEAN
    #include <Python.h>
    static PyObject *answer(PyObject *self, PyObject *args) { return PyLong_FromLong(42); }
    static PyMethodDef methods[] = {{"answer", answer, METH_NOARGS, ""}, {NULL, NULL, 0, NULL}};
    static struct PyModuleDef mod = {PyModuleDef_HEAD_INIT, "_speed", NULL, -1, methods};
    PyMODINIT_FUNC PyInit__speed(void) { return PyModule_Create(&mod); }
  '';
}
