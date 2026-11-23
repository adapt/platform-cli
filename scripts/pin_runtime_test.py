import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from pin_runtime import TARGETS, pin_release, validate

REVISION = "a" * 40
BODY = b"test archive bytes"

def pin():
    return dict(format=1, revision=REVISION, api_version=1, runtime_abi=7, guest_protocol=10,
                targets={target: dict(size=len(BODY), sha256=hashlib.sha256(BODY).hexdigest(), manifest_sha256="b" * 64) for target in TARGETS})

class Opener:
    def __init__(self, contents): self.contents = list(contents)
    def open(self, request, **_):
        if not request.full_url.startswith("https://platform.adapt.com/runtime/goblin/" + REVISION + "/"):
            raise AssertionError(request.full_url)
        return io.BytesIO(self.contents.pop(0))

class PinRuntimeTest(unittest.TestCase):
    def test_release_requires_a_real_pin(self):
        validate(None, required=False)
        with self.assertRaises(ValueError): validate(None)
        validate(pin())
        bad = pin(); bad["targets"].pop(next(iter(TARGETS)))
        with self.assertRaises(ValueError): validate(bad)

    def test_both_targets_verify_before_replacing_existing_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "runtime-lock.json"
            path.write_text("null\n")
            for bodies in [(BODY, b"bad"), (b"bad", BODY)]:
                with self.assertRaises(ValueError): pin_release(REVISION, path, opener=Opener([json.dumps(pin()).encode(), *bodies]))
                self.assertEqual(path.read_text(), "null\n")
            self.assertEqual(pin_release(REVISION, path, opener=Opener([json.dumps(pin()).encode(), BODY, BODY])), pin())
            self.assertEqual(json.loads(path.read_text()), pin())

    def test_rejects_untrusted_urls_and_mismatched_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "pin.json"
            for base in ["http://localhost/", "https://user:password@example.com/", "https://example.com/?x=1"]:
                with self.assertRaises(ValueError): pin_release(REVISION, path, base=base)
            wrong = pin(); wrong["revision"] = "c" * 40
            with self.assertRaises(ValueError): pin_release(REVISION, path, opener=Opener([json.dumps(wrong).encode()]))
            self.assertFalse(path.exists())

if __name__ == "__main__": unittest.main()
