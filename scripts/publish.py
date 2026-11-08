"""Publish tested binaries, then atomically point installers at a complete commit."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
from urllib.error import HTTPError
from urllib.parse import quote, urlencode
from urllib.request import Request, urlopen
import uuid

TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
)
IMMUTABLE = "public, max-age=31536000, immutable"
MUTABLE = "no-store"


def artifact_name(target):
    return f"adapt-{target}" + (".exe" if "windows" in target else "")


ARTIFACTS = tuple(artifact_name(target) for target in TARGETS)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def parse_checksums(data):
    result = {}
    for line in data.decode("ascii").splitlines():
        match = re.fullmatch(r"([a-f0-9]{64})  (adapt-[a-z0-9._-]+)", line)
        if not match or match[2] in result:
            raise ValueError("Invalid release checksum manifest")
        result[match[2]] = match[1]
    if set(result) != set(ARTIFACTS):
        raise ValueError("Release must contain every supported target")
    return result


class Storage:
    def __init__(self, bucket):
        if not re.fullmatch(r"[a-z0-9][a-z0-9._-]{1,220}[a-z0-9]", bucket):
            raise ValueError("Expected a Cloud Storage bucket name")
        self.bucket = bucket
        self.token = subprocess.check_output(
            ["gcloud", "auth", "print-access-token"], text=True
        ).strip()
        self.base = f"https://storage.googleapis.com/storage/v1/b/{bucket}/o"

    def request(self, url, method="GET", data=None, content_type=None):
        headers = {"Authorization": f"Bearer {self.token}"}
        if content_type:
            headers["Content-Type"] = content_type
        return urlopen(Request(url, data=data, headers=headers, method=method), timeout=120)

    def get(self, name):
        try:
            with self.request(f"{self.base}/{quote(name, safe='')}?alt=media") as response:
                return response.read()
        except HTTPError as error:
            if error.code == 404:
                return None
            raise

    def put(self, name, data, *, immutable=False):
        boundary = uuid.uuid4().hex
        metadata = json.dumps({
            "name": name,
            "cacheControl": IMMUTABLE if immutable else MUTABLE,
            "contentType": "application/octet-stream",
        }).encode()
        body = (
            f"--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n".encode()
            + metadata
            + f"\r\n--{boundary}\r\nContent-Type: application/octet-stream\r\n\r\n".encode()
            + data
            + f"\r\n--{boundary}--\r\n".encode()
        )
        query = {"uploadType": "multipart"}
        if immutable:
            query["ifGenerationMatch"] = "0"
        url = f"https://storage.googleapis.com/upload/storage/v1/b/{self.bucket}/o?{urlencode(query)}"
        try:
            with self.request(url, "POST", body, f"multipart/related; boundary={boundary}"):
                pass
        except HTTPError as error:
            if not immutable or error.code != 412:
                raise
            # A retry can finish an interrupted upload, but must never replace
            # different bytes that are already addressed by this commit.
            if self.get(name) != data:
                raise ValueError(f"Refusing to replace immutable artifact: {name}") from error

    def copy(self, source, destination):
        url = (
            f"{self.base}/{quote(source, safe='')}/copyTo/b/{self.bucket}/o/"
            f"{quote(destination, safe='')}"
        )
        with self.request(url, "POST", json.dumps({"cacheControl": MUTABLE}).encode(), "application/json"):
            pass


def publish(storage, directory, commit, main_commit):
    if not re.fullmatch(r"[a-f0-9]{40}", commit):
        raise ValueError("Expected a full commit SHA")
    prefix = f"{commit}/"
    manifest = storage.get(prefix + "SHA256SUMS")
    if manifest is None:
        # Validate the entire artifact set before uploading anything.
        files = {name: (directory / name).read_bytes() for name in ARTIFACTS}
        if any(not data for data in files.values()):
            raise ValueError("Release binaries must not be empty")
        manifest = "".join(f"{digest(data)}  {name}\n" for name, data in files.items()).encode()
        for name, data in files.items():
            storage.put(prefix + name, data, immutable=True)
        storage.put(prefix + "SHA256SUMS", manifest, immutable=True)
    else:
        # Re-running a completed commit reuses its original binaries, even if
        # today's compiler would produce different bytes from the same source.
        for name, checksum in parse_checksums(manifest).items():
            data = storage.get(prefix + name)
            if data is None or digest(data) != checksum:
                raise ValueError(f"Published release is incomplete or corrupt: {name}")

    if main_commit() != commit:
        print(f"Published {commit}; a newer main commit exists, leaving latest unchanged.")
        return False
    for name in (*ARTIFACTS, "SHA256SUMS"):
        storage.copy(prefix + name, f"latest/{name}")
    # This single object is the installer-facing commit point. The installer
    # reads it once and fetches the binary and checksum from the immutable SHA.
    storage.put("latest/commit", f"{commit}\n".encode())
    print(f"Published {commit} and promoted latest.")
    return True


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bucket", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--directory", type=Path, required=True)
    args = parser.parse_args()
    if args.repository != "adapt/platform-cli":
        parser.error("Publishing is restricted to adapt/platform-cli")
    publish(Storage(args.bucket), args.directory, args.commit, lambda: subprocess.check_output(
        ["gh", "api", f"repos/{args.repository}/commits/main", "--jq", ".sha"], text=True
    ).strip())
