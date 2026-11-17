# CLI reference

See the [README](../README.md) for installation and common tasks.
Replace example names such as `SANDBOX_ID` and `EXEC_ID` with your resource IDs.

- [Commands](#commands)
- [Authentication and profiles](#authentication-and-profiles)
- [Coding agent guide](#coding-agent-guide)
- [Sandboxes and executions](#sandboxes-and-executions)
- [Files](#files)
- [Hosted applications](#hosted-applications)
- [Images and environment variables](#images-and-environment-variables)
- [Resource IDs](#resource-ids)
- [JSON requests](#json-requests)
- [Lists and filters](#lists-and-filters)
- [Output and retries](#output-and-retries)
- [API keys](#api-keys)
- [Configuration](#configuration)
- [Exit codes](#exit-codes)

## Commands

Run `adapt COMMAND --help` for options and examples of required JSON fields.
Every `list` command also accepts `ls`.
For example, use `adapt org members ls` or `adapt sandbox ls --all`.

| Command | Operations |
| --- | --- |
| `auth` | `login`, `status`, `logout` |
| `skill` | `install`, `show`, `uninstall` |
| `whoami` | Show your account, organization, and permissions |
| `org` | `list`, `use`, `current`, `create`, `update`, `delete` |
| `org members` | `list`, `role`, `remove` |
| `org invitations` | `list`, `create`, `cancel`, `accept`, `reject` |
| `sandbox` | `list`, `get`, `create`, `sh`, `start`, `stop`, `delete`, `wait` |
| `exec` | Run a command in a sandbox |
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
| `mcp` | `list`, `revoke` |
| `events` | Show organization changes, or personal changes with `--personal` |
| `health` | Test API availability, or database readiness with `--ready` |
| `profiles` | Show saved API URLs, organizations, and credential status |
| `api` | Send an authenticated request to a path on the selected API server |

`connections` manages upstream MCP services.
`mcp` manages clients with permission to connect to Platform.
Named commands do not cover internal compute administration, OAuth provider internals, or the `/mcp` transport.
See [API coverage](api-coverage.md) for routes and API version details.

## Authentication and profiles

### Browser login

```sh
adapt auth login
adapt auth status
adapt whoami
adapt org list
adapt org use my-organization-slug
```

Login opens the Platform approval page in your browser.
With `--no-browser`, the CLI prints the link without a browser launch.
The approval page can use a different host from the API server.

If your account has one organization, the CLI selects it automatically.
If your account has multiple organizations, the CLI asks you to select one.
In a noninteractive shell, use `--org ID` or run `adapt org use ID` after login.
`org use` accepts a full organization ID or an exact slug.

The CLI uses the `adapt-cli` device client and the
[device authorization protocol](https://www.rfc-editor.org/rfc/rfc8628.html).
It follows the server polling interval and `slow_down` responses.
It handles expiry, denial, and temporary failures.

Login returns a Platform user session without a refresh token.
If the session expires or an administrator revokes it, log in again.
A browser login needs a Platform deployment with device authentication and identity providers configured.

### Logout

```sh
adapt auth logout
```

Logout revokes the saved CLI session and removes the local credential.
Your browser session stays signed in.
If the API is unavailable, use `adapt auth logout --local` to remove only the local credential.
This command does not revoke the remote session.
See [API keys](#api-keys) for logout behavior with a saved key.

### Profiles and API URLs

A profile stores an API URL, credentials, and a selected organization.
The default profile name is `default`.
The default API URL is `https://platform.adapt.com`.

Use a separate profile for each API server.
For a local server, run:

```sh
adapt --profile local auth login --api-url http://localhost:3000
adapt --profile local sandbox list
```

For staging, run:

```sh
adapt --profile staging auth login --api-url https://platform.adaptstaging.com
```

Use the API origin without `/v1`.
An origin consists of the scheme, host, and optional port.
CLI updates keep saved origins and credentials.

To select a profile for subsequent commands, set `ADAPT_PROFILE`:

```sh
export ADAPT_PROFILE=staging
adapt profiles
```

An explicit `--profile` option overrides `ADAPT_PROFILE`.
If the selected profile does not exist, the CLI prints a warning to stderr and uses defaults.
The `default` profile does not cause this warning.
The `auth login`, `org use`, and `profiles` commands do not cause it either.

## Coding agent guide

```sh
adapt skill install
adapt skill install codex
adapt skill show
adapt skill uninstall codex
adapt skill uninstall
```

`install` and `uninstall` accept `codex`, `claude`, `claude-code`, `pi`, `opencode`, or `all`.
The default is `all`.
These commands work without login, API configuration, or a network connection.
`show` prints the included `SKILL.md` as Markdown, even with `--json`.

| Agent | Default file | Override |
| --- | --- | --- |
| Codex | `~/.agents/skills/adapt/SKILL.md` | `$CODEX_HOME/skills/adapt/SKILL.md` |
| Claude Code | `~/.claude/skills/adapt/SKILL.md` | `$CLAUDE_CONFIG_DIR/skills/adapt/SKILL.md` |
| Pi and OpenCode | `~/.agents/skills/adapt/SKILL.md` | None |

Override paths must be absolute.
An empty override uses the default path.
The CLI accepts directory symlinks but refuses a symlink at `SKILL.md`.

Codex, Pi, and OpenCode share the default file.
If you remove this file for one agent, the other agents also lose access to it.
Installation replaces edits to `SKILL.md` with the version included in the CLI.
If the file already matches, the CLI leaves it unchanged.
Removal deletes only `SKILL.md`.
It keeps the directory, lock file, and other files.
Repeated removal commands succeed.

With piped output or `--json`, these commands report `installed` or `uninstalled` entries.
Each entry contains `agent`, `path`, and `changed` fields.

| Agent | How to use the guide |
| --- | --- |
| Codex | Use `$adapt` |
| Claude Code | Use `/adapt` |
| Pi | Use `/skill:adapt` |
| OpenCode | Ask the agent to use the adapt skill |

If the agent does not find the skill, restart the agent.

## Sandboxes and executions

### Temporary sandboxes

```sh
adapt sandbox sh
adapt sandbox sh --image ubuntu:24.04 --cpu 2 --memory 2048 --env EDITOR=vim
adapt sandbox sh --image-id IMAGE_ID --variable-set VARIABLE_SET_ID
```

`sandbox sh` accepts every `sandbox create` option, including `--data`.
It always waits for the sandbox to run, even without `--wait`.
Without a command, it requires a terminal and opens an interactive login shell.
It selects the shell from the guest's `$SHELL` or passwd entry.
If neither is available, it uses Bash or `/bin/sh`.

To run a command, put the command after `--`:

```sh
adapt sandbox sh -- uname -a
adapt sandbox sh --image python:3.13 -- python -c 'print("hello")'
printf 'hello\n' | adapt sandbox sh -- cat
adapt sandbox sh -- /bin/sh -lc 'echo hello; uname -a'
```

The CLI passes arguments after `--` directly to the remote program.
For shell expressions, use `/bin/sh -lc`.
With a command, `sandbox sh` accepts stdin and streams stdout and stderr.
It works without a terminal and returns the remote exit code.
Use `--json` for JSON events.

The CLI deletes the sandbox and its disk when you exit, press Ctrl-], or lose the connection.
It also deletes them if you interrupt startup.
On Unix, SIGTERM and SIGHUP trigger deletion too.
Ctrl-C goes to the remote terminal after the shell starts.
If deletion fails, the CLI reports the sandbox ID and a command to retry deletion.

The default sandbox expiry is one hour.
The `--ttl` option sets expiry in seconds.
The `--no-expiry` option disables automatic expiry.
Shells have a maximum execution timeout of 24 hours.

### Sandbox options

Memory and disk values use MiB.
Use `--data @sandbox.json` for the full Platform request schema, including network policies and startup commands.
Explicit CLI options override the corresponding JSON fields.
The `--env` and `--label` options merge entries with the JSON data.
The `--no-expiry` option sets `ttlSeconds` to null.
The `--run-image` option runs the image's ENTRYPOINT/CMD at startup.

### Executions in an existing sandbox

```sh
adapt sandbox create --image ubuntu:24.04 --cpu 2 --memory 2048 --wait
adapt exec SANDBOX_ID -- /bin/sh -lc 'echo hello; uname -a'
adapt exec SANDBOX_ID -it -- /bin/sh
```

By default, `exec` streams saved stdout and stderr and returns the remote exit code.
Use these options to change its behavior:

| Option | Behavior |
| --- | --- |
| `--detach` | Return execution details immediately |
| `--stdin` or `-i` | Send stdin through a live WebSocket connection |
| `--tty` or `-t` | Create an interactive terminal and enable stdin |

In terminal mode, Ctrl-] disconnects and Ctrl-C goes to the remote terminal.
Outside terminal mode, Ctrl-C disconnects and exits with code 130.
The remote execution continues after the CLI closes.

```sh
adapt execs get EXEC_ID
adapt execs logs EXEC_ID
adapt execs logs EXEC_ID --cursor 42
adapt execs attach EXEC_ID --after 42
adapt execs cancel EXEC_ID
```

Saved log streams reconnect from their last consumed cursor.
Live connections report the last sequence number when they disconnect.
The CLI never sends input again automatically.
Saved logs use Server-Sent Events (SSE) cursors.
Live WebSocket connections use separate sequence numbers.

Platform removes sensitive values from saved logs.
Live WebSocket output does not have this protection.
For large input, use file transfers.
The server limits pending WebSocket input and can disconnect clients if the guest stops reading.

## Files

```sh
adapt files put SANDBOX_ID ./server.py /app/server.py
adapt files get SANDBOX_ID /app/result.bin ./result.bin
adapt files get SANDBOX_ID /app/result.bin - > result.bin
```

The transfer limit is 16 MiB per file.
Parent directories must exist in the sandbox.
Use `-` for the local path to read stdin or write stdout.
The CLI writes a download to its destination file only after the transfer completes.
To replace an existing local file, add `--force`.

## Hosted applications

Configure the application to listen on `0.0.0.0` inside the sandbox.
Create an endpoint:

```sh
adapt endpoints create "My app" --access authenticated
```

Connect the endpoint to the application's port:

```sh
adapt endpoints bind ENDPOINT_ID SANDBOX_ID --port 3000
adapt endpoints get ENDPOINT_ID
```

Create an access token:

```sh
adapt endpoints token ENDPOINT_ID
```

Use the header `X-Adapt-Authorization: Bearer TOKEN` for requests to the hosted application.
Endpoint tokens cannot authenticate Platform API requests.

To disconnect the endpoint from the sandbox, run:

```sh
adapt endpoints unbind ENDPOINT_ID
```

## Images and environment variables

Import an image:

```sh
adapt images import ubuntu:24.04 --name ubuntu --architecture arm64 --wait
```

To create an encrypted environment variable set, save this JSON as `variables.json`:

```json
{
  "name": "development",
  "env": { "API_URL": "https://example.com" }
}
```

Create the variable set:

```sh
adapt env create --data @variables.json
```

Create a sandbox with the image and variable set:

```sh
adapt sandbox create --image-id IMAGE_ID --variable-set VARIABLE_SET_ID --wait
```

## Resource IDs

These resources accept full UUIDs or unique ID prefixes:

- Sandboxes and executions.
- Images and registry credentials.
- Volumes and endpoints.
- Environment variable sets and tool connections.

This also applies to references such as `--image-id`, `--credential`, and both IDs in an endpoint binding.

```sh
adapt sandbox get a8f731bc
adapt images get b29f104e
adapt endpoints bind c31ab902 a8f731bc --port 8080
adapt sandbox create --image-id b29f104e --variable-set d48ae102
```

Platform resolves prefixes within your permissions and selected organization.
Matching ignores case and accepts UUIDs with or without hyphens.
If a prefix matches multiple resources, Platform returns `409 CONFLICT`.
Supply more characters to identify one resource.

Prefixes do not match deleted resources.
Use full IDs to access saved history for deleted resources.
Responses contain full UUIDs.
Waits and log reconnects use the resolved full ID.
Use full IDs in scripts.
The Platform API must support resource ID prefixes.

Authentication and organization commands require exact identifiers.
This includes `--org`, members, invitations, keys, sessions, and grant principals.
`org use` accepts a full organization ID or an exact slug.
Pagination cursors, idempotency keys, and credential tokens also require exact values.

## JSON requests

The `--data` option accepts a JSON object, `@file.json`, or `-` for stdin.

```sh
adapt registry create --data @registry-credential.json
adapt env update VARIABLE_SET_ID --data @variable-patch.json
adapt connections create --data '{"name":"Tools","url":"https://tools.example.com/mcp","authType":"oauth"}'
adapt connections oauth CONNECTION_ID
adapt connections search "files"
adapt connections call CONNECTION_ID read_file --arguments '{"path":"/report.txt"}'
```

Updates require these fields:

| Resource | Required fields |
| --- | --- |
| Registry credential | All credential fields and the current `version` |
| Environment variable set | `name`, `version`, and `env`; use null values to remove variables |
| Tool connection | `version`, `name`, `enabled`, and `allowedTools` |

Platform validates request data and permissions.
It reports conflicts and unsupported operations.
The current API does not support deletion of ready volumes or volume mounts in sandboxes.

For API operations without a named command, use `adapt api`:

```sh
adapt api GET /v1/whoami
adapt api GET /openapi.json
adapt api POST /v1/auth/update-user --data '{"name":"New name"}'
```

The `api` command accepts paths on the selected API origin.
It does not accept arbitrary target URLs.

## Lists and filters

```sh
adapt sandbox list --all --json
adapt sandbox list --limit 50 --cursor CURSOR
adapt sandbox list --query web --all --json
adapt images list --status ready
adapt images list --name ubuntu --all
```

`sandbox list --query TEXT` matches names that contain TEXT, without case sensitivity.
It also matches IDs that start with TEXT.
Names come from the `labels.name` field.
The table output does not show names.
Use `--json` to see them.

`images list --name NAME` returns all versions with that exact name.
The name match is case-sensitive.
Use `--status` to select `preparing`, `ready`, `failed`, or `deleting` images.
You can use both filters in the same command.

These filters apply to every page that `--all` retrieves.
If you use `--cursor` to request each page yourself, supply the same filters with each request.

Resource lists use cursor pagination.
Use `--all` to retrieve all pages.
Connection lists and call history use a page size that the server sets.
Do not supply `--limit` for those commands.
Member and API-key lists use `--offset` with pages of 50 entries.
Tool search returns a `nextOffset` value for the next page.

## Output and retries

### Output formats

| Command type | Output |
| --- | --- |
| Most commands | Tables in a terminal; JSON when piped or with `--json` |
| Execution and log commands | Raw remote output; newline-delimited JSON events with `--json` |
| Files sent to stdout | Raw bytes, including with `--json` |
| `events` | Newline-delimited JSON |
| `skill show` | Markdown, including with `--json` |

The CLI sends progress, login links, and idempotency keys to stderr.
The `events` command reports disconnections.
After a reconnect, it sends a reset event.
When you receive a reset event, request current resource data again.

### Retry a creation request

An idempotency key identifies a request so that a retry does not create the resource again.
Sandbox, volume, endpoint, image, image pull, and execution creation require an idempotency UUID.
The CLI generates a key and prints it before it sends the request.
If you lose the response, retry with the same key and the same input:

```sh
adapt sandbox create --image ubuntu:24.04 --idempotency-key YOUR_PREVIOUS_UUID
```

### Automatic retries and waits

JSON API reads (`GET` and `HEAD`) retry connection failures and interrupted response bodies.
They also retry HTTP 408, 429, 500, 502, 503, and 504 responses.
This behavior includes readiness polling.

Retry delays increase from 250 milliseconds to 8 seconds.
The CLI follows `Retry-After` when the server supplies it.
Retries share the original `--timeout` limit, which defaults to 60 seconds.
Authentication errors and invalid JSON fail immediately.
The CLI does not automatically retry requests that change data.

An asynchronous deletion returns `{"status":202}`.
An empty response returns its HTTP status, such as `{"status":204}`.
Acceptance does not mean that an asynchronous operation has completed.

Use `--wait` on create, start, or stop commands to wait up to ten minutes for the requested state.
The `sandbox wait` and `images wait` commands accept `--wait-timeout SECONDS`.
Retries do not extend that deadline.

## API keys

To use an organization API key without a saved credential, set `ADAPT_TOKEN`:

```sh
export ADAPT_TOKEN='your-organization-api-key'
adapt sandbox list
```

To save a key from stdin, run:

```sh
adapt auth login --token-stdin < /path/to/private/key-file
```

The CLI validates the key with `/v1/whoami` and saves its organization.
Logout removes a saved API key from the local configuration only.
To revoke the key, use `adapt keys delete KEY_ID`.

`ADAPT_TOKEN` overrides saved credentials for requests.
Logout always uses the saved credential.
It never revokes a key from `ADAPT_TOKEN`.

## Configuration

| Setting | Option | Environment variable |
| --- | --- | --- |
| API origin | `--api-url` | `ADAPT_API_URL` |
| Profile; default: `default` | `--profile` | `ADAPT_PROFILE` |
| Organization for this request | `--org` | `ADAPT_ORG_ID` |
| Bearer token | Import through stdin | `ADAPT_TOKEN` |
| Configuration directory | `--config-dir` | `ADAPT_CONFIG_DIR` |
| HTTP timeout; default: 60 seconds | `--timeout` | None |

CLI options override environment variables.
Environment variables override settings in the selected profile.

Without a directory override, the CLI uses these configuration paths:

| System | Path |
| --- | --- |
| With `XDG_CONFIG_HOME` set | `$XDG_CONFIG_HOME/adapt/config.json` |
| Other Unix systems | `~/.config/adapt/config.json` |
| Windows, without `XDG_CONFIG_HOME` | `%APPDATA%/adapt/config.json` |

The CLI stores credentials in this JSON file without encryption.
It does not use an OS keychain.
On Unix, it creates directories with mode `0700` and files with mode `0600`.
It refuses to read a configuration file that permits group or public access.
Do not share this file.
Do not commit it to version control.
For temporary CI credentials, use `ADAPT_TOKEN`.
The `profiles` and `auth status` commands never display saved tokens.

Saved credentials and organization defaults apply only to their API origin.
If you select a different origin, the CLI does not reuse them.
The CLI does not follow HTTP redirects.
It requires HTTPS, except for loopback development servers.

To update from `plat`, use the `adapt` executable.
Rename `PLAT_*` environment variables to `ADAPT_*`.
Run `adapt auth login` to sign in with the new configuration directory.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | CLI or API failure, or an unknown execution result |
| `2` | Invalid CLI arguments |
| `124` | Remote timeout |
| `130` | Interruption |

Completed remote commands return their exit codes from 0 through 255.
If the result is unknown or outside this range, the CLI returns 1.
Live signal exits return 128 plus the signal number.
