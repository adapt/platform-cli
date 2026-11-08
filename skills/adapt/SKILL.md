---
name: adapt
description: >-
  Use the adapt CLI to manage Adapt Platform sandboxes, run remote commands,
  transfer files, and discover or execute tools from connected MCP servers.
  Use when the user asks to work with Platform resources or their connected tools.
---

# Adapt Platform

Use `adapt COMMAND --help` for exact arguments. `adapt skill show` prints this
guide offline. Resource IDs are full UUIDs from API results; discover them with
list/search commands rather than guessing. Every `list` command also accepts `ls`.

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

The default API origin is `https://platform.adaptstaging.com`. `--profile NAME`,
`--api-url ORIGIN`, and `--org ID` select a different context. Credentials and
saved organization defaults are scoped to the API origin. `ADAPT_TOKEN` overrides
saved credentials; never print it or the credential file. Use `adapt profiles`
to inspect saved connection settings without exposing tokens.

## Sandboxes and commands

```sh
adapt sandbox list --all --json
adapt images list --all --json
adapt sandbox sh --image ubuntu:24.04 -- uname -a
adapt sandbox sh --image-id IMAGE_ID --cpu 2 --memory 2048 -- /bin/sh -lc 'echo hello; uname -a'
adapt sandbox create --image-id IMAGE_ID --wait
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
```

File transfers are limited to 16 MiB and guest parent directories must exist.
Downloads refuse to overwrite local files unless `--force` is given. Hosted apps
must listen on `0.0.0.0`. Endpoint tokens authenticate the hosted app with
`X-Adapt-Authorization: Bearer TOKEN`; they are not Platform API credentials.

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

## Output, pagination, and retries

Normal commands print JSON when piped; `--json` requests it explicitly. Execution
and log commands stream raw output unless `--json` is supplied, in which case
they emit newline-delimited events. Progress and idempotency keys go to stderr.

Resource listings return objects such as `{"sandboxes": [...], "nextCursor": ...}`.
Use `--all` to follow cursor pages. Tool search uses `--offset` and its returned
`nextOffset`; connection listings have a server-defined page size.

Resource creation and execution print an idempotency UUID before sending the
request. If a response is lost, retry with the same inputs and
`--idempotency-key UUID` to avoid creating another resource. Mutations are not
automatically retried. `--wait` waits for readiness; an accepted asynchronous
operation is not necessarily complete. For endpoints without a dedicated
command, `adapt api METHOD /v1/PATH --data @request.json` uses the same profile
and authentication. Discover the schema with `adapt api GET /openapi.json`.
