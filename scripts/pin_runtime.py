"""Verify an immutable published Goblin release and pin it in the CLI source."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import tempfile
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, Request, build_opener

TARGETS = {"aarch64-apple-darwin", "x86_64-unknown-linux-gnu"}
MAX_ARCHIVE = 512 * 1024 * 1024
DEFAULT_BASE = "https://platform.adapt.com/runtime/goblin/"


def validate(pin, required=True):
    if pin is None:
        if required:
            raise ValueError("Pin a published runtime before releasing this CLI: python scripts/pin_runtime.py REVISION")
        return
    if (not isinstance(pin, dict) or set(pin) != {"format", "revision", "api_version", "runtime_abi", "guest_protocol", "targets"}
            or type(pin["format"]) is not int or pin["format"] != 1
            or type(pin["api_version"]) is not int or pin["api_version"] != 1
            or not re.fullmatch(r"[a-f0-9]{40}", pin["revision"])
            or any(type(pin[k]) is not int or not 0 < pin[k] <= 0xffffffff for k in ("runtime_abi", "guest_protocol"))
            or set(pin["targets"]) != TARGETS):
        raise ValueError("Invalid runtime lock identity or target set")
    for record in pin["targets"].values():
        if (set(record) != {"sha256", "manifest_sha256", "size"}
                or any(not re.fullmatch(r"[a-f0-9]{64}", record[k]) for k in ("sha256", "manifest_sha256"))
                or type(record["size"]) is not int or not 0 < record["size"] <= MAX_ARCHIVE):
            raise ValueError("Invalid runtime artifact pin")


class NoRedirects(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def pin_release(revision, destination, base=DEFAULT_BASE, opener=None):
    if not re.fullmatch(r"[a-f0-9]{40}", revision):
        raise ValueError("Expected a full Goblin commit SHA")
    origin = urlsplit(base)
    if (origin.scheme != "https" or not origin.netloc or origin.username or origin.password
            or origin.query or origin.fragment or not origin.path.endswith("/")):
        raise ValueError("Expected an HTTPS directory URL without credentials")
    opener = opener or build_opener(NoRedirects())
    def get(name):
        return opener.open(Request(f"{base}{revision}/{name}", headers={"Accept-Encoding": "identity"}), timeout=300)
    with get("runtime-lock.json") as response:
        encoded = response.read(65537)
        if len(encoded) > 65536:
            raise ValueError("Oversized runtime lock")
    pin = json.loads(encoded)
    validate(pin)
    if pin["revision"] != revision:
        raise ValueError("Published runtime revision mismatch")
    for target, record in pin["targets"].items():
        h, size = hashlib.sha256(), 0
        with get(f"{target}.tar.gz") as response:
            while chunk := response.read(65536):
                size += len(chunk)
                if size > record["size"]:
                    raise ValueError("Runtime download exceeds pinned size")
                h.update(chunk)
        if size != record["size"] or h.hexdigest() != record["sha256"]:
            raise ValueError("Published runtime archive checksum mismatch")
    # Update only after both exact artifacts are available and verified.
    file = tempfile.NamedTemporaryFile(dir=destination.parent, delete=False)
    temporary = Path(file.name)
    try:
        with file:
            file.write((json.dumps(pin, sort_keys=True, indent=2) + "\n").encode())
        # Close before replacement so this also works on Windows.
        temporary.replace(destination)
    finally:
        temporary.unlink(missing_ok=True)
    return pin


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("revision", nargs="?")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--require", action="store_true")
    parser.add_argument("--lock", type=Path, default=Path("runtime-lock.json"))
    parser.add_argument("--base-url", default=DEFAULT_BASE)
    args = parser.parse_args()
    if args.check:
        validate(json.loads(args.lock.read_text()), required=args.require)
    elif args.revision:
        pin_release(args.revision, args.lock, args.base_url)
    else:
        parser.error("Supply a revision or --check")
