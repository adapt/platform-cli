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

The staging publisher uses these repository **Actions secrets**:

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
Platform supplies the same bucket name through its runtime deployment
configuration and proxies published files at `/cli/`. Its static `/install.sh`
downloads through that proxy on the CLI's default API origin.

### Production

After all target checks and staging publication succeed, `publish-production`
downloads the same five tested artifacts and publishes them to
`gs://adapt-platform-prod-cli` in `adapt-platform-prod`. Both environments use
the same production-default binary; use an explicit staging profile when needed.
Production's public identifiers are configured in the workflow and require no
additional GitHub secrets:

| Setting | Value |
| --- | --- |
| Publisher | `platform-cli-publish@adapt-platform-prod.iam.gserviceaccount.com` |
| OIDC provider | `projects/57849980830/locations/global/workloadIdentityPools/github/providers/platform-cli` |
| Bucket | `adapt-platform-prod-cli`, `us-central1` |
| GitHub environment | `production` |

The publisher has object-admin access only to this bucket. The bucket uses
uniform access and public access prevention. Platform's production runtime has
object-viewer access and serves the allowed binaries through its public `/cli/`
proxy. The project's organization policy does not permit public bucket grants.
OIDC trust requires repository ID `1447622020`, owner ID `219891555`, a push to
`main`, this repository's `ci.yml` workflow, and the `production` environment.
Its immutable subject is
`repo:adapt@219891555/platform-cli@1447622020:environment:production`.
GitHub creates the environment on first use if it does not already exist.

Platform must set its production `CLI_RELEASE_BUCKET=adapt-platform-prod-cli`
and serve an installer downloading from `https://platform.adapt.com/cli`.
Merge and publish the CLI first, then enable those Platform download settings.
The bucket starts empty; the first successful main publication populates it.

## Public download layout

Production downloads are available without caller credentials at
`https://platform.adapt.com/cli/`. Its backing bucket is private. Staging's
existing public bucket also supports direct downloads at
`https://storage.googleapis.com/BUCKET/`. Both use this object layout:

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
