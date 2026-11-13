# Platform CLI

`adapt` is the Rust command-line client for [Adapt Platform](https://platform.adapt.com).
It covers the user-facing resource API, account and organization management, browser login,
execution streams, and file transfers. No Platform backend changes are required.

## Install and sign in

On macOS or Linux, install the latest build:

```sh
curl -fsSL https://platform.adapt.com/install.sh | sh
```

The installer supports Intel and Apple Silicon Macs (macOS 11 or later), and
x86-64 and ARM64 Linux.
It verifies SHA-256 and installs to `~/.local/bin` without `sudo`. Add that
directory to your `PATH` if needed. Run the same command to update, or set
`ADAPT_INSTALL_DIR` on the `sh` command to choose another directory.

To build from source instead, use a current stable Rust toolchain:

```sh
cargo install --path . --locked
```

Then sign in:

```sh
adapt auth login
adapt whoami
```

Login displays a short code and opens Platform's approval page in your browser. Sign in,
verify the code, and approve access. The CLI waits for approval and helps you choose an
organization. A sole organization is selected automatically. With multiple organizations
in a noninteractive shell, select one with `--org ID` or run `adapt org use ID` afterward.

For local development:

```sh
adapt --profile local auth login --api-url http://localhost:3000
adapt --profile local sandbox list
```

The API URL defaults to `https://platform.adapt.com`. Saved profiles, `ADAPT_API_URL`,
or `--api-url` can select another origin. Use the API origin, without `/v1`.
The verification URL returned by the API may point at
a separate dashboard origin. `--no-browser` prints the link for use on another machine.

For staging, use a separate profile:

```sh
adapt --profile staging auth login --api-url https://platform.adaptstaging.com
```

Existing profiles keep their saved origin and credentials when upgrading.

To use a profile without repeating `--profile`, set `ADAPT_PROFILE` (for example
`export ADAPT_PROFILE=staging`). An explicit `--profile` always takes precedence over the
variable. If the selected profile name has not been saved, `adapt` warns on stderr and
continues with defaults; `default` is exempt, and `auth login`, `org use`, and `profiles`
do not warn because they create or list profiles. `adapt profiles` lists saved names.

```sh
adapt auth status
adapt org list
adapt org use my-organization-slug
adapt profiles
adapt auth logout
```

Login uses Platform's existing `adapt-cli` device client and its
[device authorization protocol](https://www.rfc-editor.org/rfc/rfc8628.html).
Polling respects the server interval, `slow_down`, expiry, denial, and transient failures.
The returned credential is a Platform user session; there is no refresh token. Log in again
if it expires or is revoked. Logout revokes the saved CLI session, leaving your browser
session signed in. `auth logout --local` removes the local credential when the API is
unreachable; it does not revoke the remote session.

## Coding agent skill

Install the bundled Adapt guide for Codex, Claude Code, Pi, and OpenCode:

```sh
adapt skill install
adapt skill install codex
adapt skill show
adapt skill uninstall codex
adapt skill uninstall
```

`install` and `uninstall` accept `codex`, `claude` (also `claude-code`), `pi`,
`opencode`, or `all` (the default). They work offline without login or API
configuration. `show` prints the bundled `SKILL.md` as Markdown, even with `--json`.

| Agent | Skill path |
| --- | --- |
| Codex | `~/.agents/skills/adapt/SKILL.md`, or `$CODEX_HOME/skills/adapt/SKILL.md` when set |
| Claude Code | `~/.claude/skills/adapt/SKILL.md`, or `$CLAUDE_CONFIG_DIR/skills/adapt/SKILL.md` when set |
| Pi and OpenCode | `~/.agents/skills/adapt/SKILL.md` |

Overrides must be absolute paths; empty overrides use the default. Codex, Pi,
and OpenCode share the default installation, so uninstalling for one also
removes the guide for the others using that directory. Directory symlinks are
supported; symlinks at `SKILL.md` are refused.

Installing again updates the guide to match the CLI, replacing any edits to
`SKILL.md`; an unchanged guide is left untouched. Uninstall removes only
`SKILL.md`, preserving the directory, its lock file, and any neighboring files.
Repeated uninstalls succeed. Piped output and `--json` report `installed` or
`uninstalled` entries with `agent`, `path`, and `changed` fields.

Use `$adapt` in Codex, `/adapt` in Claude Code, `/skill:adapt` in Pi, or ask
OpenCode to use the adapt skill. Restart the agent if the change does not appear.

## Common workflows

Open a login shell in a temporary sandbox:

```sh
adapt sandbox sh
adapt sandbox sh --image ubuntu:24.04 --cpu 2 --memory 2048 --env EDITOR=vim
adapt sandbox sh --image-id IMAGE_ID --variable-set VARIABLE_SET_ID
```

Run a one-off command in a temporary sandbox instead:

```sh
adapt sandbox sh -- uname -a
adapt sandbox sh --image python:3.13 -- python -c 'print("hello")'
printf 'hello\n' | adapt sandbox sh -- cat
adapt sandbox sh -- /bin/sh -lc 'echo hello; uname -a'
```

Arguments after `--` are passed literally as the command's argv. Use `/bin/sh -lc`
for shell expressions. Commands forward stdin, stream stdout/stderr, and return the
remote exit code; they also work without a terminal and support `--json` events.

`sandbox sh` accepts every `sandbox create` option, including `--data`, and always
waits for the sandbox to run (`--wait` is optional). Without a command, it requires
a terminal and opens an interactive login shell using the guest's `$SHELL` or passwd
entry, falling back to Bash or `/bin/sh`. It deletes the sandbox and its disk when you exit,
press Ctrl-], or the connection fails, and returns the shell's exit code. Ctrl-C
goes to the remote terminal; interrupting startup also cleans up the sandbox.
On Unix, SIGTERM and SIGHUP trigger cleanup too. Cleanup errors report the sandbox
ID and a retry command. Automatic expiry still follows the create options (one hour
by default); shells have the API's maximum execution timeout of 24 hours.

Create a sandbox, run a command, and open a shell:

```sh
adapt sandbox create --image ubuntu:24.04 --cpu 2 --memory 2048 --wait
adapt exec SANDBOX_ID -- /bin/sh -lc 'echo hello; uname -a'
adapt exec SANDBOX_ID -it -- /bin/sh
```

`exec` streams retained stdout and stderr and returns the remote exit code. `--detach`
returns execution metadata immediately. `--stdin` forwards input through a live WebSocket;
`--tty` allocates a terminal and implies stdin. Ctrl-] detaches a terminal; Ctrl-C goes to
the remote terminal. Outside terminal mode, Ctrl-C detaches and exits with 130. Closing
the CLI does not cancel a remote execution.

```sh
adapt execs get EXEC_ID
adapt execs logs EXEC_ID
adapt execs logs EXEC_ID --cursor 42
adapt execs attach EXEC_ID --after 42
adapt execs cancel EXEC_ID
```

Retained log streams reconnect with their last consumed cursor. Live attachments report
the last sequence on disconnect; input is never replayed automatically. SSE cursors and
WebSocket sequences are different counters. Retained logs are redacted by Platform; live
WebSocket output is unredacted. Use file transfers for bulk input: the server limits the
amount of pending WebSocket stdin and can disconnect an attachment if the guest stops reading.

Transfer files (up to 16 MiB; parent directories must exist in the guest):

```sh
adapt files put SANDBOX_ID ./server.py /app/server.py
adapt files get SANDBOX_ID /app/result.bin ./result.bin
adapt files get SANDBOX_ID /app/result.bin - > result.bin
```

`-` reads stdin or writes stdout. Downloads to a file are written atomically and refuse
to overwrite an existing file unless `--force` is supplied.

Publish an application listening on `0.0.0.0` inside a sandbox:

```sh
adapt endpoints create "My app" --access authenticated
adapt endpoints bind ENDPOINT_ID SANDBOX_ID --port 3000
adapt endpoints get ENDPOINT_ID
adapt endpoints token ENDPOINT_ID
adapt endpoints unbind ENDPOINT_ID
```

Endpoint tokens use `X-Adapt-Authorization: Bearer TOKEN` when calling the hosted app.
They cannot authenticate Platform API requests.

Import an image and use encrypted environment variable sets:

```sh
adapt images import ubuntu:24.04 --name ubuntu --architecture arm64 --wait
adapt env create --data @variables.json
adapt sandbox create --image-id IMAGE_ID --variable-set VARIABLE_SET_ID --wait
```

`variables.json`:

```json
{
  "name": "development",
  "env": { "API_URL": "https://example.com" }
}
```

For advanced sandbox configuration, `--data @sandbox.json` accepts the full Platform
request schema, including network policies and startup commands. Explicit CLI flags
override corresponding fields; `--env` and `--label` merge entries. `--no-expiry` sets
`ttlSeconds` to null. `--run-image` runs the image's ENTRYPOINT/CMD at startup.

## Commands

Run `adapt COMMAND --help` for flags and examples of required JSON fields.
Every `list` subcommand also accepts `ls`, including nested commands such as
`adapt org members ls` and resource commands such as `adapt sandbox ls --all`.

| Command | Operations |
| --- | --- |
| `auth` | `login`, `status`, `logout` |
| `whoami` | Effective caller, organization, and permissions |
| `org` | `list`, `use`, `current`, `create`, `update`, `delete` |
| `org members` | `list`, `role`, `remove` |
| `org invitations` | `list`, `create`, `cancel`, `accept`, `reject` |
| `sandbox` | `list`, `get`, `create`, `start`, `stop`, `delete`, `wait` |
| `exec` | Execute argv; optional `--detach`, `--stdin`, `--tty` |
| `execs` | `get`, `cancel`, `logs`, `attach` |
| `files` | `put`, `get` |
| `volumes` | `list`, `get`, `create`, `quota`, `delete` |
| `endpoints` | `list`, `get`, `create`, `access`, `bind`, `unbind`, `token`, `revoke-tokens`, `delete` |
| `images` | `list`, `get`, `import`, `pull`, `wait`, `delete` |
| `env` | `list`, `get`, `create`, `update`, `delete` |
| `registry` | `list`, `create`, `update`, `delete` |
| `connections` | `list`, `get`, `create`, `update`, `delete`, `refresh`, `grant`, `oauth`, `oauth-complete`, `calls`, `principals`, `search`, `describe`, `call` |
| `keys` | `list`, `create`, `delete` |
| `sessions` | `list`, `revoke`, `revoke-others` |
| `mcp` | `list`, `revoke` for clients connected to Platform |
| `events` | Organization changes, or personal changes with `--personal` |
| `health` | Liveness, or database readiness with `--ready` |
| `profiles` | Saved origins, organizations, and credential presence |
| `api` | Authenticated request to any path on the configured origin |

Resource operations match `platform` commit `f20ded3`; see [API coverage](docs/api-coverage.md).
Internal compute administration, OAuth provider internals, and the `/mcp` transport are
outside the named command surface. `connections` manages upstream MCP services;
`mcp` manages clients authorized to connect to Platform.

Complex mutations use `--data` with a JSON object, `@file.json`, or `-` for stdin:

```sh
adapt registry create --data @registry-credential.json
adapt env update VARIABLE_SET_ID --data @variable-patch.json
adapt connections create --data '{"name":"Tools","url":"https://tools.example.com/mcp","authType":"oauth"}'
adapt connections oauth CONNECTION_ID
adapt connections search "files"
adapt connections call CONNECTION_ID read_file --arguments '{"path":"/report.txt"}'
```

Registry updates require all credential fields and their current `version`. Environment
set updates require `name`, `version`, and `env`; use null values to remove variables.
Connection updates require `version`, `name`, `enabled`, and `allowedTools`. Platform
validates resource schemas and permissions and reports conflicts or unsupported operations.
In particular, ready volume deletion and sandbox volume mounts are currently unsupported.

## Scripting and configuration

Normal commands emit tables on a terminal and JSON when piped. `--json` always requests
JSON. Progress, login links, and idempotency keys go to stderr. Execution/log commands
write raw remote output unless `--json` is explicit; then they emit newline-delimited
JSON events. Files sent to stdout are always raw bytes. `events` emits newline-delimited
JSON and reports disconnections; reconnecting yields a reset event, so refresh snapshots.

```sh
adapt sandbox list --all --json
adapt sandbox list --limit 50 --cursor CURSOR
adapt api GET /v1/whoami
adapt api GET /openapi.json
adapt api POST /v1/auth/update-user --data '{"name":"New name"}'
```

Resource lists support cursor pagination with `--all`. Gateway connections and call
history have a server-defined page size; omit `--limit` there. Member and API-key lists
use `--offset` in pages of 50; tool search uses its returned `nextOffset`.

Creation of sandboxes, volumes, endpoints, images, image pulls, and executions requires
an idempotency UUID. The CLI generates and prints one before sending the request. To
retry after a lost response, reuse that key **and the same input**:

```sh
adapt sandbox create --image ubuntu:24.04 --idempotency-key YOUR_PREVIOUS_UUID
```

JSON API reads (`GET`/`HEAD`, including readiness polling) retry connection failures,
interrupted response bodies, and HTTP 408, 429, 500, 502, 503, and 504 responses.
Retries back off from 250 ms to 8 seconds, honor `Retry-After`, and share the original
`--timeout` budget (60 seconds by default). Progress goes to stderr so JSON stdout
stays parseable. Authentication errors and invalid JSON fail immediately.

Mutations are not automatically retried. Asynchronous deletions return `{"status":202}`;
confirmed empty responses return their HTTP status, such as `{"status":204}`. `--wait`
on creation/start/stop waits for readiness for up to ten minutes. `sandbox wait` and
`images wait` accept `--wait-timeout SECONDS`; retries cannot extend that deadline.

Use an organization API key in automation without saving it:

```sh
export ADAPT_TOKEN='your-organization-api-key'
adapt sandbox list
```

Or save a key using stdin:

```sh
adapt auth login --token-stdin < /path/to/private/key-file
```

An imported key is validated against `/v1/whoami` and its organization is saved. Logging
out removes a saved API key locally; revoke it explicitly with `keys delete` when needed.
`ADAPT_TOKEN` overrides saved credentials for requests, but logout always targets the
saved credential and never revokes an environment override.

| Setting | Flag | Environment variable |
| --- | --- | --- |
| API origin | `--api-url` | `ADAPT_API_URL` |
| Profile (default: `default`; the flag overrides the variable) | `--profile` | `ADAPT_PROFILE` |
| Organization for this request | `--org` | `ADAPT_ORG_ID` |
| Bearer token | Use stdin to import | `ADAPT_TOKEN` |
| Configuration directory | `--config-dir` | `ADAPT_CONFIG_DIR` |
| HTTP timeout (default: 60 seconds) | `--timeout` | — |

Flags override environment variables; both override the selected profile. Configuration
lives at `$XDG_CONFIG_HOME/adapt/config.json`, otherwise `~/.config/adapt/config.json`
(on Windows, `%APPDATA%/adapt/config.json`). Credentials are stored in this local JSON
file, **not encrypted or stored in an OS keychain**. On Unix, the CLI creates directories
with mode 0700 and files with mode 0600 and refuses to read a group/world-accessible
configuration file. Do not commit or share this file. Use `ADAPT_TOKEN` for ephemeral CI
credentials. Profiles and auth status never display saved tokens.

When upgrading from `plat`, use the `adapt` executable and rename any `PLAT_*`
environment variables to `ADAPT_*`. Run `adapt auth login` to sign in using the
new configuration directory.

Saved credentials and organization defaults are scoped to their API origin. Overriding
the origin does not reuse them. HTTP redirects are never followed, and HTTPS is required
except for loopback development servers. `api` accepts paths, not arbitrary target URLs.

Exit codes: 0 for success, 1 for CLI/API failures, 2 for invalid CLI arguments, 124 for a
remote timeout, 130 for interruption. Completed commands preserve exit codes 0–255;
unrepresentable or unknown results return 1. Live signal exits return 128 plus the signal.

## Development

See [CLI publishing](docs/publishing.md) for CI builds, repository secrets, and
the public download layout. Publishing uses plain Cargo and Cloud Storage.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
python3 -m unittest discover -s scripts -p '*_test.py'
```

Tests start local HTTP/WebSocket servers and temporary configuration directories; they
do not need Platform, PostgreSQL, real accounts, or credentials. The integration suite
checks auth polling and revocation, credential isolation, request contracts, pagination,
idempotency, binary transfers, SSE reconnection, and WebSocket input/output. CI runs on
Linux, macOS, and Windows. A real browser sign-in still requires a running Platform
deployment with its device-auth migration and identity providers configured.

## License

Licensed under the [MIT License](LICENSE).
