---
name: adapt
description: >-
  Use the adapt CLI to manage Adapt Platform sandboxes, run remote commands,
  transfer files, and discover or execute tools from connected MCP servers.
  Use when the user asks to work with Platform resources or their connected tools.
---

# Adapt Platform

Use `adapt COMMAND --help` for exact arguments. `adapt skill show` prints this
guide offline. Discover resource IDs with list/search commands rather than guessing.
Core resource selectors (sandboxes, executions, images, registries, volumes,
endpoints, variable sets, and tool connections), including linked IDs such as
`--image-id`, `--credential`, and both IDs in `endpoints bind`, accept full UUIDs or
unambiguous hexadecimal prefixes.
Platform resolves them within the caller's authorized scope; ambiguity returns
409, so supply more characters. Responses retain full IDs. Prefer full IDs for
persistent scripts and deleted resources. Auth and organization selectors require
exact IDs; `org use` also accepts an exact slug. Cursors, idempotency keys, and
credential tokens require exact values. Every `list` command also accepts `ls`.

## Identity and organization

```sh
adapt auth status
adapt whoami
adapt org list
adapt org use ORGANIZATION_ID_OR_SLUG
```

Reuse the existing login. If authentication is missing or expired, run
`adapt auth login`; the user approves a short code in their browser. On a machine
without a browser, use `adapt auth login --no-browser` and give the user the
approval URL and code. A noninteractive login with multiple organizations needs
`--org ID`, or select the organization afterward with `adapt org use`.

The default API origin is `https://platform.adapt.com`. `--profile NAME`,
`--api-url ORIGIN`, and `--org ID` select a different context. Credentials and
saved organization defaults are scoped to the API origin. `ADAPT_TOKEN` overrides
saved credentials; never print it or the credential file. Use `adapt profiles`
to inspect saved connection settings without exposing tokens.

## Sandboxes and commands

```sh
adapt sandbox list --all --json
adapt sandbox list --query NAME_OR_ID_PREFIX --all --json
adapt images list --all --json
adapt images list --name IMAGE_NAME --status ready --all --json
adapt sandbox sh --image ubuntu:24.04 -- uname -a
adapt sandbox sh --image-id IMAGE_ID --cpu 2 --memory 2048 -- /bin/sh -lc 'echo hello; uname -a'
adapt sandbox create --image-id IMAGE_ID --wait
adapt sandbox usage SANDBOX_ID --hours 24 --json
adapt exec SANDBOX_ID --cwd /app -- /bin/sh -lc 'npm test'
```

`sandbox sh` creates a temporary sandbox, waits for it to run, executes the
command, and deletes the sandbox and its disk when finished. Copy needed
artifacts out before exit. Without a command it opens a login shell and requires
a terminal. It accepts all `sandbox create` options, including environment
variables (`--env KEY=VALUE`), encrypted sets (`--variable-set ID`), resource
limits, and `--data @request.json`. Memory and disk flags use MiB; `--ttl` uses
seconds. Sandboxes expire after one hour by default; `--no-expiry` disables expiry.

Arguments after `--` are literal argv. Use `/bin/sh -lc 'SCRIPT'` for shell
expansion, pipes, or redirection. Paths and programs run inside the sandbox.
`sandbox sh` forwards stdin automatically; `exec` needs `--stdin` for live input.
Use `exec --detach` to return execution metadata immediately, or `exec -it`
for an interactive terminal. Normal command execution streams output and returns
the remote exit code.

For a temporary local VM, use `adapt sandbox sh --local` on Apple Silicon macOS
or x86_64 Linux with KVM. Set `ADAPT_LOCAL_RUNTIME` to a compatible Goblin bundle's
`bin` directory, or pass `--runtime-dir`. This works without Platform login and
supports image, CPU, memory, disk, and environment flags. Local TTL, cloud image
IDs, variable sets, labels, `--data`, image startup, and idempotency keys are not
supported. Only `sandbox sh` currently accepts the local target; other resource
commands retain their cloud behavior. Local shells are deleted on exit and do not
mount host directories. `--cloud` explicitly selects the existing cloud shell.

```sh
adapt execs get EXEC_ID
adapt execs logs EXEC_ID
adapt execs attach EXEC_ID
adapt execs cancel EXEC_ID
adapt sandbox delete SANDBOX_ID
```

Detaching from an execution does not cancel it. Ctrl-] detaches an interactive
terminal; in `sandbox sh` this also triggers sandbox deletion. Retained logs are
redacted by Platform, while live WebSocket output is unredacted.

## Files and hosted apps

```sh
adapt files put SANDBOX_ID ./input.json /app/input.json
adapt files get SANDBOX_ID /app/result.json ./result.json
adapt files get SANDBOX_ID /app/output.txt -
adapt endpoints create "My app" --access authenticated
adapt endpoints bind ENDPOINT_ID SANDBOX_ID --port 3000
adapt endpoints get ENDPOINT_ID
adapt endpoints request ENDPOINT_ID /path --new-token
```

File transfers are limited to 16 MiB and guest parent directories must exist.
Downloads refuse to overwrite local files unless `--force` is given. Hosted apps
must listen on `0.0.0.0`. Endpoint tokens authenticate the hosted app with
`X-Adapt-Authorization: Bearer TOKEN`; they are not Platform API credentials.
`endpoints request` streams the app's response body to stdout (SSE included),
sends only `-H` headers plus the endpoint token from `--token`/`--new-token`,
never follows redirects, and exits 1 after printing the body of a non-2xx response.

## Connected MCP tools

```sh
adapt connections list --all --json
adapt connections search "slack channels"
adapt connections describe CONNECTION_ID TOOL_NAME
adapt connections call CONNECTION_ID TOOL_NAME --arguments @arguments.json
```

Search finds tools across the user's connections. Describe the chosen tool to
get its input schema before constructing arguments; provider tool names and
schemas vary. `--arguments` accepts a JSON object inline, `@file.json`, or `-`
for stdin. Use the returned connection ID and exact tool name.

For Slack, discover the channel-listing tool, inspect its schema, and call it
to resolve a channel name to its ID. When posting is requested, discover and
describe the message-posting tool and use that channel ID. Listing channels is
a separate action from sending a message. Inspect tool results for reported
errors as well as the CLI exit status.

## Models

```sh
adapt models list --json
adapt models providers --json
```

`models list` returns the organization's enabled models; IDs include the
provider prefix, such as `openai/gpt-5`. It needs `modelGateway` `use`
permission, which an API key has only if granted. `models providers` lists configured
providers without their keys. `models discover PROVIDER_ID` lists every model a
provider offers and requires browser sign-in as an owner or admin.

## Platform MCP server

```sh
adapt mcp tools
adapt mcp tools TOOL_NAME
```

`mcp tools` lists the built-in tools of Platform's own MCP server, and
`mcp tools TOOL_NAME` prints one tool's input schema. It requires an organization
API key (`adapt auth login --token-stdin` or `ADAPT_TOKEN`); browser sign-in
sessions are refused. These are not the user's connected tools; use
`connections search` for those. `mcp list` shows authorized MCP clients, not tools.

## Output, pagination, and retries

Normal commands print JSON when piped; `--json` requests it explicitly. Execution
and log commands stream raw output unless `--json` is supplied, in which case
they emit newline-delimited events. Progress and idempotency keys go to stderr.

Resource listings return objects such as `{"sandboxes": [...], "nextCursor": ...}`.
Use `--all` to follow cursor pages. Tool search uses `--offset` and its returned
`nextOffset`; connection listings have a server-defined page size.

JSON API reads retry transient connection and gateway failures with backoff,
honoring `Retry-After` within the original `--timeout` budget. Readiness waits keep
their overall deadline. Authentication errors and invalid JSON fail immediately.

Resource creation and execution print an idempotency UUID before sending the
request. If a response is lost, retry with the same inputs and
`--idempotency-key UUID` to avoid creating another resource. Mutations are not
automatically retried. `--wait` waits for readiness; an accepted asynchronous
operation is not necessarily complete. For endpoints without a dedicated
command, `adapt api METHOD /v1/PATH --data @request.json` uses the same profile
and authentication. Discover the schema with `adapt api GET /openapi.json`.
