# Minimal stub for the pyrocko.gui package. The bridge only does `import
# pyrocko.gui` and reads `pyrocko.gui.__file__` (implicitly typed by mypy) to
# locate the snuffler package directory; the actual Qt snuffler submodules
# (gui.snuffler.marker, gui.snuffler) are imported dynamically at runtime via
# importlib and are never statically type-checked. A real .pyi is required
# because git cannot track the empty placeholder directory — it would vanish in a
# fresh CI checkout and mypy would fail with "Cannot find implementation or
# library stub for module named pyrocko.gui".
