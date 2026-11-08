"""Stage only the compiled executable; no source or credentials enter releases."""

import argparse
from pathlib import Path
import shutil

from publish import TARGETS, artifact_name


def package(target, root=Path(".")):
    if target not in TARGETS:
        raise ValueError("Unsupported release target")
    executable = "adapt.exe" if "windows" in target else "adapt"
    source = root / "target" / target / "release" / executable
    if not source.is_file() or source.stat().st_size == 0:
        raise ValueError(f"Missing compiled executable: {source}")
    output = root / "dist"
    output.mkdir(exist_ok=True)
    destination = output / artifact_name(target)
    shutil.copyfile(source, destination)
    return destination


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=TARGETS)
    args = parser.parse_args()
    print(package(args.target))
