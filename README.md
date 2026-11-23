# Adapt Platform CLI

`adapt` is the command-line client for [Adapt Platform](https://platform.adapt.com).
Use it to manage agent infrastructure like sandboxes, MCP connections, and more.

## Install

On macOS or Linux, run:

```sh
curl -fsSL https://platform.adapt.com/install.sh | sh
```

The installer verifies the SHA-256 checksum and places `adapt` in `~/.local/bin`.
Add `~/.local/bin` to your `PATH` and restart your shell.

To update the CLI, run the same install command.

To change the install directory, set `ADAPT_INSTALL_DIR` on the `sh` command.

## Sign in

Start the login process:

```sh
adapt auth login
```

The CLI shows a code and opens an approval page in your browser.

1. Sign in to Adapt Platform in the browser.
2. Make sure that the browser code matches the CLI code.
3. Approve access.
4. If the CLI asks for an organization, select one.

Show your account and selected organization:

```sh
adapt whoami
```

If the machine has no browser, use `adapt auth login --no-browser`.

## Profiles

A profile stores an API URL, credentials, and a selected organization.
The default profile is `default`.

Sign in to another profile with `--profile`:

```sh
adapt --profile staging auth login --api-url https://platform.adaptstaging.com
```

To use a profile for every command in your shell, set `ADAPT_PROFILE`:

```sh
export ADAPT_PROFILE=staging
adapt whoami
```

The `--profile` option overrides `ADAPT_PROFILE`.
List saved profiles with `adapt profiles`.
For more information, see the [CLI reference](docs/cli-reference.md#profiles-and-api-urls).

## Use a temporary sandbox

Open a shell:

```sh
adapt sandbox sh
```

Or run a command:

```sh
adapt sandbox sh --image ubuntu:24.04 -- uname -a
```

The CLI creates a sandbox and waits for it to run. It deletes the sandbox and
its disk when the shell or command ends.

Sandboxes expire after one hour by default.
Use `--ttl SECONDS` to change this time.
Use `--no-expiry` to disable automatic expiry.

### Run sandboxes locally

On Apple Silicon macOS or x86_64 Linux with KVM, Adapt downloads and verifies
its pinned Goblin runtime on first local use:

```sh
adapt sandbox sh --local --image ubuntu:24.04 -- uname -m
adapt sandbox sh --local
adapt --local sandbox create --image ubuntu:24.04 --wait
adapt --local sandbox list
adapt --local exec SANDBOX_ID -- uname -m
adapt --local sandbox delete SANDBOX_ID
```

Adapt starts and reuses a private local manager without Platform login. The shell
or command from `sandbox sh` runs in a VM that is deleted when the session ends.
`sandbox create` keeps the VM for later commands until you stop or delete it. Existing
commands keep their cloud default; `sandbox sh --cloud` selects it explicitly.
Use `adapt runtime install` to download ahead of time and `adapt runtime status`
to inspect the pin and installation path. Local downloads need no login. Developers
can still supply `ADAPT_LOCAL_RUNTIME=/path/to/goblin-release/bin` or `--runtime-dir`.
See
[local sandbox options and limitations](docs/cli-reference.md#local-sandboxes).

## Create a sandbox

Create a sandbox that stays available after each command:

```sh
adapt sandbox create --image ubuntu:24.04 --cpu 2 --memory 2048 --wait
```

In the examples below, replace `SANDBOX_ID` with the ID from the response.

Run a command in the sandbox:

```sh
adapt exec SANDBOX_ID -- uname -a
```

Open an interactive shell:

```sh
adapt exec SANDBOX_ID -it -- /bin/sh
```

Press Ctrl-] to disconnect from this shell.
The remote execution continues after you disconnect.

List all sandboxes:

```sh
adapt sandbox list --all
```

Delete the sandbox when you no longer need it:

```sh
adapt sandbox delete SANDBOX_ID
```

Resource commands accept a full ID or a unique ID prefix.

## Transfer files

Upload a file to a sandbox:

```sh
adapt files put SANDBOX_ID ./input.txt /tmp/input.txt
```

Download a file from a sandbox:

```sh
adapt files get SANDBOX_ID /tmp/output.txt ./output.txt
```

## Call connected tools

Find tools on your connected Model Context Protocol (MCP) services:

```sh
adapt connections search "files"
```

In the following commands, use the `CONNECTION_ID` and `TOOL_NAME` from the search results.
Show the input schema for a tool from the results:

```sh
adapt connections describe CONNECTION_ID TOOL_NAME
```

Put the required arguments in `arguments.json`.
Then call the tool:

```sh
adapt connections call CONNECTION_ID TOOL_NAME --arguments @arguments.json
```

## Install the Skill

Install the Adapt skill for Codex, Claude Code, Pi, and OpenCode:

```sh
adapt skill install
```

To install it for one agent, add the agent name. For example:

```sh
adapt skill install codex
```

Show the skill text without installing:

```sh
adapt skill show
```

## Help

Show the commands and their options:

```sh
adapt --help
adapt sandbox --help
adapt sandbox create --help
```

## Build and test

To install from this repository, use a current stable Rust toolchain:

```sh
cargo install --path . --locked
```

Run the development checks:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
python3 -m unittest discover -s scripts -p '*_test.py'
```

## License

[MIT License](LICENSE).
