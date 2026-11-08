import hashlib
from pathlib import Path
import tempfile
import unittest

from package_release import package
from publish import ARTIFACTS, TARGETS, parse_checksums, publish

SCRATCH = Path(__file__).resolve().parents[1] / ".tmp"
SCRATCH.mkdir(exist_ok=True)
COMMIT = "a" * 40


class Storage:
    def __init__(self):
        self.objects = {}
        self.writes = []
        self.fail_copy = None

    def get(self, name):
        return self.objects.get(name)

    def put(self, name, data, *, immutable=False):
        if immutable and name in self.objects and self.objects[name] != data:
            raise ValueError("Immutable artifact conflict")
        self.objects[name] = data
        self.writes.append(name)

    def copy(self, source, destination):
        if destination == self.fail_copy:
            raise OSError("Interrupted promotion")
        self.objects[destination] = self.objects[source]
        self.writes.append(destination)


class PublishingTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(dir=SCRATCH)
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.storage = Storage()
        for name in ARTIFACTS:
            (self.directory / name).write_bytes(f"binary: {name}".encode())

    def test_complete_release_is_promoted_only_after_every_file(self):
        self.assertTrue(publish(self.storage, self.directory, COMMIT, lambda: COMMIT))
        self.assertEqual(self.storage.writes[-1], "latest/commit")
        self.assertEqual(self.storage.objects["latest/commit"], f"{COMMIT}\n".encode())
        checksums = parse_checksums(self.storage.objects[f"{COMMIT}/SHA256SUMS"])
        for name in ARTIFACTS:
            binary = (self.directory / name).read_bytes()
            self.assertEqual(self.storage.objects[f"latest/{name}"], binary)
            self.assertEqual(checksums[name], hashlib.sha256(binary).hexdigest())

    def test_missing_or_empty_target_never_starts_publication(self):
        (self.directory / ARTIFACTS[-1]).unlink()
        with self.assertRaises(FileNotFoundError):
            publish(self.storage, self.directory, COMMIT, lambda: COMMIT)
        self.assertFalse(self.storage.writes)
        (self.directory / ARTIFACTS[-1]).touch()
        with self.assertRaises(ValueError):
            publish(self.storage, self.directory, COMMIT, lambda: COMMIT)
        self.assertFalse(self.storage.writes)

    def test_older_main_run_cannot_roll_latest_back(self):
        previous = b"b" * 40 + b"\n"
        self.storage.objects["latest/commit"] = previous
        self.assertFalse(publish(self.storage, self.directory, COMMIT, lambda: "c" * 40))
        self.assertEqual(self.storage.objects["latest/commit"], previous)
        self.assertIn(f"{COMMIT}/SHA256SUMS", self.storage.objects)
        self.assertFalse(any(name.startswith("latest/") for name in self.storage.writes))

    def test_interrupted_promotion_keeps_installer_on_previous_commit(self):
        previous = b"b" * 40 + b"\n"
        self.storage.objects["latest/commit"] = previous
        self.storage.fail_copy = "latest/" + ARTIFACTS[-1]
        with self.assertRaises(OSError):
            publish(self.storage, self.directory, COMMIT, lambda: COMMIT)
        self.assertEqual(self.storage.objects["latest/commit"], previous)
        self.storage.fail_copy = None
        publish(self.storage, self.directory, COMMIT, lambda: COMMIT)
        self.assertEqual(self.storage.objects["latest/commit"], f"{COMMIT}\n".encode())

    def test_reruns_reuse_original_completed_commit(self):
        publish(self.storage, self.directory, COMMIT, lambda: COMMIT)
        original = self.storage.objects.copy()
        for name in ARTIFACTS:
            (self.directory / name).write_bytes(b"rebuilt with a newer compiler")
        self.storage.writes.clear()
        publish(self.storage, self.directory, COMMIT, lambda: COMMIT)
        self.assertEqual(self.storage.objects, original)
        self.assertTrue(all(name.startswith("latest/") for name in self.storage.writes))

    def test_partial_upload_can_resume_but_cannot_replace_different_bytes(self):
        name = ARTIFACTS[0]
        self.storage.objects[f"{COMMIT}/{name}"] = (self.directory / name).read_bytes()
        publish(self.storage, self.directory, COMMIT, lambda: COMMIT)
        self.storage = Storage()
        self.storage.objects[f"{COMMIT}/{name}"] = b"different build"
        with self.assertRaises(ValueError):
            publish(self.storage, self.directory, COMMIT, lambda: COMMIT)
        self.assertNotIn("latest/commit", self.storage.objects)
        self.assertNotIn(f"{COMMIT}/SHA256SUMS", self.storage.objects)

    def test_corrupt_completed_release_cannot_be_promoted(self):
        publish(self.storage, self.directory, COMMIT, lambda: COMMIT)
        self.storage.objects[f"{COMMIT}/{ARTIFACTS[0]}"] = b"corrupt"
        self.storage.writes.clear()
        with self.assertRaises(ValueError):
            publish(self.storage, self.directory, COMMIT, lambda: COMMIT)
        self.assertFalse(self.storage.writes)

    def test_commit_and_manifest_are_strict(self):
        for commit in ["main", "../latest", "a" * 39, "a" * 41]:
            with self.assertRaises(ValueError):
                publish(self.storage, self.directory, commit, lambda: commit)
        for manifest in [b"", b"invalid", b"a" * 64 + b"  ../adapt\n"]:
            with self.assertRaises(ValueError):
                parse_checksums(manifest)

    def test_packages_only_the_expected_executable_for_every_platform(self):
        for target in TARGETS:
            binary = "adapt.exe" if "windows" in target else "adapt"
            source = self.directory / "target" / target / "release"
            source.mkdir(parents=True)
            (source / binary).write_bytes(b"compiled binary")
            (source / "private-source.rs").write_text("private source")
            output = package(target, self.directory)
            self.assertEqual(output.read_bytes(), b"compiled binary")
        self.assertEqual({file.name for file in (self.directory / "dist").iterdir()}, set(ARTIFACTS))


if __name__ == "__main__":
    unittest.main()
