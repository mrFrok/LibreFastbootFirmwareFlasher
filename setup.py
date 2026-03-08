"""
setup.py — cx_Freeze build script for LibreFastbootFirmwareFlasher.

Used by: make build  (via python setup.py build)
"""

import sys
from pathlib import Path
from cx_Freeze import setup, Executable

PROJECT_ROOT = str(Path(__file__).parent.resolve())

# Include argcomplete only if installed — it is a soft dependency
_packages = ["flasher"]
try:
    import argcomplete  # noqa: F401
    _packages.append("argcomplete")
except ImportError:
    pass

build_exe_options = {
    "path": sys.path + [PROJECT_ROOT],
    "packages": _packages,
    "excludes": [
        "tkinter",
        "unittest",
        "email",
        "xml",
        "pydoc",
        "doctest",
        "difflib",
    ],
    "build_exe": "dist/lfff",
    "silent": True,
}

setup(
    name="LibreFastbootFirmwareFlasher",
    version="0.1.4",
    description="CLI tool for extracting and flashing Android firmware via fastboot",
    executables=[
        Executable(
            script="main.py",
            target_name="lfff",
        )
    ],
    options={"build_exe": build_exe_options},
)
