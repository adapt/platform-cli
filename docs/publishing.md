# CLI publishing

Pull requests and pushes to `main` run formatting, Clippy, tests, and a release
build on native runners. A push to `main` publishes only after all five
targets pass:

| System | Artifact |
| --- | --- |
| macOS ARM64 | `adapt-aarch64-apple-darwin` |
| macOS x86-64 | `adapt-x86_64-apple-darwin` |
| Linux ARM64 | `adapt-aarch64-unknown-linux-musl` |
| Linux x86-64 | `adapt-x86_64-unknown-linux-musl` |
| Windows x86-64 | `adapt-x86_64-pc-windows-msvc.exe` |

macOS builds target macOS 11 or later. Linux builds use musl so they do not need
a matching host glibc version. The shell installer supports macOS and Linux;
Windows users can download and rename the `.exe` directly.

## Configuration

Configure these repository **Actions secrets**, including identifiers that are
public in Google Cloud, so deployment details stay out of source control:

| Secret | Value |
| --- | --- |
| `GCP_PROJECT_ID` | Project containing the release bucket and identity pool |
| `GCP_WORKLOAD_IDENTITY_PROVIDER` | Full `projects/NUMBER/locations/global/workloadIdentityPools/POOL/providers/PROVIDER` resource name |
| `GCP_SERVICE_ACCOUNT` | Publisher service account email |
| `CLI_RELEASE_BUCKET` | Public bucket name, without `gs://` |

The bucket needs uniform bucket-level access and `allUsers` granted
`roles/storage.objectViewer`. Grant the publisher `roles/storage.objectAdmin`
on this bucket only. Configure GitHub OIDC, grant the repository principal
`roles/iam.workloadIdentityUser` on the service account, and restrict the provider
to this repository and owner by numeric ID, `refs/heads/main`, the `push` event,
and `adapt/platform-cli/.github/workflows/ci.yml@refs/heads/main`.

There is no service account key. PR and branch builds cannot publish. Source,
debug symbols, and credentials are never included in the staged release files.
Platform supplies the same bucket name through its deployment configuration
when building `/install.sh`.
The served installer necessarily contains the public download location.

## Public download layout

Objects are available without credentials at
`https://storage.googleapis.com/BUCKET/`:

```text
COMMIT_SHA/
  adapt-aarch64-apple-darwin
  adapt-x86_64-apple-darwin
  adapt-aarch64-unknown-linux-musl
  adapt-x86_64-unknown-linux-musl
  adapt-x86_64-pc-windows-msvc.exe
  SHA256SUMS
latest/
  (the same binaries and SHA256SUMS)
  commit
```

Commit paths use the full 40-character Git SHA and immutable caching. Uploads
use create-only generation preconditions. Re-running a completed commit reuses
its original binaries; it cannot silently replace them with a new compiler's
output. An interrupted upload can resume only when existing bytes match.

Publishing is serialized. Each run publishes its commit snapshot, but promotes
`latest/` only if its SHA is still the head of `main`. Mutable objects use
`Cache-Control: no-store`. The publisher updates `latest/commit` last, after
all binaries and checksums are present.

The installer reads `latest/commit` once, then fetches the checksum and binary
from that immutable commit directory. This keeps downloads consistent during
publication or an interrupted promotion. Direct downloads from `latest/` are
convenient, but callers verifying checksums should use the commit pointer too.

## Validation

Run `python3 -m unittest discover -s scripts -p '*_test.py'`. Tests cover complete
and interrupted publication, stale runs, immutable retries, corrupt manifests,
and packaging only the intended executable. The first public build appears
after this workflow reaches `main` and its checks pass.
