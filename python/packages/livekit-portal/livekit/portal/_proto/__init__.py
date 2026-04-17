"""Generated protobuf modules.

`protoc --python_out=` emits flat `import foo_pb2` statements, which fail
inside a package. Putting this directory on `sys.path` resolves those imports
without hand-patching the generated files.
"""
import os as _os
import sys as _sys

_HERE = _os.path.dirname(_os.path.abspath(__file__))
if _HERE not in _sys.path:
    _sys.path.insert(0, _HERE)

from . import ffi_pb2, handle_pb2, portal_pb2, types_pb2  # noqa: E402,F401
