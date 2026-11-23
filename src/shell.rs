use crate::{
    api::{Api, ApiError},
    backend::{Sandbox, SandboxBackend},
    cli::{Creation, ExecArgs, SandboxShell},
    commands::wait_for,
    stream,
};
use anyhow::{Context, Result, bail};
use reqwest::{Method, StatusCode};
use uuid::Uuid;

// Honor an explicitly configured SHELL, then the guest user's passwd entry.
// The bootstrap uses only /bin/sh plus optional tools found in common images.
const LOGIN_SHELL: &str = r#"shell=${SHELL:-}
if [ -z "$shell" ] && command -v getent >/dev/null 2>&1; then
    entry=$(getent passwd "$(id -u)") || :
    shell=${entry##*:}
fi
if [ -z "$shell" ] || [ ! -x "$shell" ]; then
    if [ -x /bin/bash ]; then shell=/bin/bash; else shell=/bin/sh; fi
fi
exec "$shell" -il"#;

pub fn validate(args: &SandboxShell, json_output: bool) -> Result<()> {
    let interactive = args.argv.is_empty();
    if interactive && json_output {
        bail!("sandbox sh cannot be combined with --json");
    }
    if interactive {
        stream::require_terminal().context("sandbox sh requires an interactive terminal")?;
        // Fail before creating a sandbox if terminal dimensions cannot be read.
        crossterm::terminal::size()?;
    }
    Ok(())
}

pub async fn run(
    backend: &SandboxBackend<'_>,
    args: SandboxShell,
    json_output: bool,
) -> Result<u8> {
    validate(&args, json_output)?;
    let interactive = args.argv.is_empty();
    let mut shutdown = Shutdown::new()?;

    let creation = backend.create(args.create);
    tokio::pin!(creation);
    let (created, interrupted) = tokio::select! {
        biased;
        code = shutdown.wait() => {
            // Cancelling an in-flight POST can lose the new sandbox's ID. Let
            // the bounded request finish so we can clean up its exact resource.
            eprintln!("\nCancelled. Waiting for sandbox creation before cleanup…");
            (creation.await, Some(code))
        }
        created = &mut creation => (created, None),
    };
    let sandbox = created?;
    let id = sandbox.id;
    eprintln!("Temporary sandbox: {id}");
    if interactive {
        eprintln!("Exiting the shell or pressing Ctrl-] deletes this sandbox.");
    }

    // Scope the session future so cancellation drops its raw-terminal guard
    // before printing diagnostics or making cleanup requests.
    let result = if let Some(code) = interrupted {
        Ok(code)
    } else {
        tokio::select! {
            biased;
            code = shutdown.wait() => Ok(code),
            result = session(backend, &sandbox, args.argv, json_output) => result,
        }
    };
    let cleanup = backend.delete(id).await.with_context(|| {
        format!(
            "Could not delete temporary sandbox {id}; {}",
            backend.cleanup_hint(id)
        )
    });
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Ok(0), Err(error)) => Err(error),
        (Ok(code), Err(error)) => {
            eprintln!("error: {error:#}");
            Ok(code)
        }
        (Err(error), Err(cleanup)) => Err(error.context(format!("{cleanup:#}"))),
    }
}

async fn session(
    backend: &SandboxBackend<'_>,
    sandbox: &Sandbox,
    argv: Vec<String>,
    json_output: bool,
) -> Result<u8> {
    let id = sandbox.id;
    let interactive = argv.is_empty();
    backend.wait(id).await?;
    backend
        .execute(
            sandbox,
            ExecArgs {
                sandbox: id.into(),
                cwd: None,
                env: vec![],
                // The API's ordinary command default is ten minutes. An interactive
                // shell should instead last up to the API maximum or sandbox expiry.
                timeout_ms: Some(86_400_000),
                detach: false,
                stdin: true,
                tty: interactive,
                creation: Creation::default(),
                argv: if interactive {
                    vec!["/bin/sh".into(), "-c".into(), LOGIN_SHELL.into()]
                } else {
                    argv
                },
            },
            json_output,
        )
        .await
}

pub(crate) async fn cleanup_cloud(api: &Api, id: Uuid) -> Result<()> {
    eprintln!("Deleting temporary sandbox {id}…");
    let result: Result<()> = async {
        let response = api
            .json(
                Method::DELETE,
                &format!("/v1/sandboxes/{id}"),
                None,
                &[],
                None,
            )
            .await?;
        if response["status"] != 204 {
            wait_for(api, "sandboxes", id, "deleted", 600).await?;
        }
        Ok(())
    }
    .await;
    // Expiry or another client may already have removed the sandbox.
    match result {
        Err(error)
            if error
                .downcast_ref::<ApiError>()
                .is_some_and(|e| e.status == StatusCode::NOT_FOUND) =>
        {
            Ok(())
        }
        result => result,
    }
}

#[cfg(unix)]
struct Shutdown {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Shutdown {
    fn new() -> Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    async fn wait(&mut self) -> u8 {
        tokio::select! {
            _ = self.interrupt.recv() => 130,
            _ = self.terminate.recv() => 143,
            _ = self.hangup.recv() => 129,
        }
    }
}

#[cfg(windows)]
struct Shutdown(tokio::signal::windows::CtrlC);

#[cfg(windows)]
impl Shutdown {
    fn new() -> Result<Self> {
        Ok(Self(tokio::signal::windows::ctrl_c()?))
    }

    async fn wait(&mut self) -> u8 {
        self.0.recv().await;
        130
    }
}
