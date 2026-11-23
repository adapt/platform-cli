use super::Backend;
#[cfg(unix)]
use crate::stream;
use crate::{
    api,
    cli::{ExecArgs, SandboxCreate, SandboxList},
    input::ResourceRef,
    output,
};
use anyhow::{Context, Result, bail};
#[cfg(unix)]
use reqwest::header::HeaderMap;
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio::sync::OnceCell;
use tokio::time::{Instant, sleep};
#[cfg(unix)]
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use url::Url;
use uuid::Uuid;

pub struct Goblin {
    client: Client,
    origin: Url,
    #[cfg(unix)]
    headers: HeaderMap,
    #[cfg(unix)]
    socket_path: PathBuf,
    #[cfg(unix)]
    state_dir: PathBuf,
    timeout: Duration,
}

pub fn validate(args: &SandboxCreate) -> Result<()> {
    for (present, flag) in [
        (args.data.is_some(), "--data"),
        (args.image_id.is_some(), "--image-id"),
        (args.ttl.is_some(), "--ttl"),
        (!args.labels.is_empty(), "--label"),
        (!args.variable_set.is_empty(), "--variable-set"),
        (args.run_image, "--run-image"),
        (args.creation.idempotency_key.is_some(), "--idempotency-key"),
    ] {
        if present {
            bail!("{flag} is not supported by the local backend");
        }
    }
    Ok(())
}

impl Goblin {
    #[cfg(unix)]
    pub fn new(
        socket_path: PathBuf,
        token: &str,
        state_dir: PathBuf,
        timeout: Duration,
    ) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
        let client = Client::builder()
            .no_proxy()
            .unix_socket(socket_path.clone())
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers.clone())
            .timeout(timeout)
            .build()?;
        Ok(Self {
            client,
            origin: "http://localhost/".parse()?,
            headers,
            socket_path,
            state_dir,
            timeout,
        })
    }

    pub async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut request = self.client.request(method, self.origin.join(path)?);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await?;
        let status = response.status();
        let bytes = api::limited_bytes(response, 1024 * 1024).await?;
        if !status.is_success() {
            let message = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_owned))
                .unwrap_or_else(|| status.to_string());
            return Err(LocalError {
                status,
                message: output::safe(&message),
            }
            .into());
        }
        if bytes.is_empty() {
            Ok(Value::Null)
        } else {
            Ok(serde_json::from_slice(&bytes)?)
        }
    }

    pub async fn create(&self, args: SandboxCreate) -> Result<Value> {
        let id = Uuid::new_v4();
        // Save exec defaults before creating a VM; a separate `adapt exec` process
        // must see the same environment as the temporary shell workflow.
        #[cfg(unix)]
        metadata::save(&self.state_dir, id, &args.env)?;
        eprintln!("Creating local sandbox: {id}");
        let body = json!({
            "id": id.to_string(), "image": args.image.unwrap_or_else(|| "ubuntu:24.04".into()),
            "resources": {"vcpus": args.cpu.unwrap_or(1), "memory_mib": args.memory.unwrap_or(1024)},
            "state_size_mib": args.disk.unwrap_or(2048),
            "network": {"mode": "user", "egress": "public", "dns": "1.1.1.1"}
        });
        match self
            .request(Method::POST, "/v1/vms", Some(&body))
            .await
            .and_then(|value| snapshot(value, id))
        {
            Ok(value) => Ok(value),
            Err(error) => {
                // A lost response may hide an accepted create. Never delete a
                // conflicting VM owned by someone else.
                if error
                    .downcast_ref::<LocalError>()
                    .is_some_and(|e| e.status == StatusCode::CONFLICT)
                {
                    #[cfg(unix)]
                    metadata::remove(&self.state_dir, id)?;
                } else if let Err(cleanup) = self.delete(id).await {
                    return Err(error.context(format!(
                        "Local create may have been accepted as {id}; cleanup failed: {cleanup:#}"
                    )));
                }
                Err(error)
            }
        }
    }

    pub async fn wait(&self, id: Uuid, target: &str, seconds: u64) -> Result<Value> {
        eprintln!("Waiting for local sandbox {id} to become {target}…");
        tokio::time::timeout(Duration::from_secs(seconds), async {
            loop {
                let value = match self
                    .request(Method::GET, &format!("/v1/vms/{id}"), None)
                    .await
                {
                    Ok(value) => snapshot(value, id)?,
                    Err(error)
                        if target == "deleted"
                            && error
                                .downcast_ref::<LocalError>()
                                .is_some_and(|e| e.status == StatusCode::NOT_FOUND) =>
                    {
                        return Ok(json!({"id":id,"status":"deleted"}));
                    }
                    Err(error) => return Err(error),
                };
                let status = value["status"].as_str().unwrap_or("unknown");
                if status == target {
                    return Ok(value);
                }
                if matches!(
                    status,
                    "stopped" | "failed" | "completed" | "unknown" | "suspended"
                ) && target != "deleted"
                {
                    bail!(
                        "Local sandbox {id} reached {status} before {target}: {}",
                        output::safe(
                            value["error"]
                                .as_str()
                                .unwrap_or("inspect the local runtime log")
                        )
                    );
                }
                sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .context("Timed out waiting for the local sandbox; the operation continues locally")?
    }

    #[cfg(unix)]
    pub async fn execute(&self, args: ExecArgs, json_output: bool) -> Result<u8> {
        let session = Uuid::new_v4();
        let id = full_id(args.sandbox.clone())?;
        let mut env = metadata::load(&self.state_dir, id)?;
        env.extend(args.env);
        let mut body = json!({"id":session.to_string(), "argv":args.argv, "env":env, "cwd":args.cwd,
            "stdin":args.stdin || args.tty, "timeout_ms":args.timeout_ms});
        if args.tty {
            let (cols, rows) = crossterm::terminal::size()?;
            body["terminal"] =
                json!({"size":{"rows":rows.max(1),"cols":cols.max(1)},"term":"xterm-256color"});
        }
        let path = format!("/v1/vms/{}/sessions/{session}", id);
        self.request(
            Method::POST,
            &format!("/v1/vms/{}/sessions", id),
            Some(&body),
        )
        .await?;
        let mut url = self
            .origin
            .join(&format!("{path}/stream?after=0&read_only=false"))?;
        url.set_scheme("ws")
            .map_err(|_| anyhow::anyhow!("Invalid local stream URL"))?;
        let mut request = url.as_str().into_client_request()?;
        request.headers_mut().extend(self.headers.clone());
        let (socket, _) = tokio::time::timeout(self.timeout, async {
            let stream = tokio::net::UnixStream::connect(&self.socket_path).await?;
            tokio_tungstenite::client_async(request, stream).await
        })
        .await
        .context("Timed out attaching to local execution")??;
        stream::terminal(
            socket,
            stream::Attachment {
                after: 0,
                input: args.stdin || args.tty,
                tty: args.tty,
                json_output,
                resume: format!(
                    "Local sandbox: {id}. Use `adapt --local sandbox delete {id}` to remove it."
                ),
                credit: Some(0),
            },
        )
        .await
    }

    #[cfg(not(unix))]
    pub async fn execute(&self, _: ExecArgs, _: bool) -> Result<u8> {
        bail!("Local sandboxes require a Unix host")
    }

    pub async fn delete(&self, id: Uuid) -> Result<()> {
        eprintln!("Deleting local sandbox {id}…");
        // Deletion can briefly contend with image preparation or a worker launch.
        let deadline = Instant::now() + self.timeout;
        loop {
            match self
                .request(Method::DELETE, &format!("/v1/vms/{id}"), None)
                .await
            {
                Ok(_) => break,
                Err(error) => {
                    let status = error.downcast_ref::<LocalError>().map(|e| e.status);
                    if status == Some(StatusCode::NOT_FOUND) {
                        break;
                    }
                    if matches!(
                        status,
                        Some(StatusCode::CONFLICT | StatusCode::TOO_MANY_REQUESTS)
                    ) && Instant::now() < deadline
                    {
                        sleep(Duration::from_millis(100)).await;
                    } else {
                        return Err(error);
                    }
                }
            }
        }
        #[cfg(unix)]
        metadata::remove(&self.state_dir, id)?;
        Ok(())
    }
}

#[derive(Debug)]
struct LocalError {
    status: StatusCode,
    message: String,
}
impl std::fmt::Display for LocalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Local runtime: {} ({})", self.message, self.status)
    }
}
impl std::error::Error for LocalError {}

/// Runtime startup is lazy: an unsupported operation must not touch credentials,
/// create state directories or launch a manager.
pub struct LocalBackend {
    state_dir: PathBuf,
    runtime_dir: Option<PathBuf>,
    timeout: Duration,
    client: OnceCell<Goblin>,
}

impl LocalBackend {
    pub fn new(state_dir: PathBuf, runtime_dir: Option<PathBuf>, timeout: Duration) -> Self {
        Self {
            state_dir,
            runtime_dir,
            timeout,
            client: OnceCell::new(),
        }
    }

    async fn client(&self) -> Result<&Goblin> {
        self.client
            .get_or_try_init(|| {
                crate::local_runtime::connect(
                    &self.state_dir,
                    self.runtime_dir.as_deref(),
                    self.timeout,
                )
            })
            .await
    }
}

impl Backend for LocalBackend {
    fn name(&self) -> &'static str {
        "local"
    }

    async fn sandbox_list(&self, args: SandboxList) -> Result<Value> {
        if args.page.limit.is_some() || args.page.cursor.is_some() {
            return self.unsupported("sandbox list pagination (--limit/--cursor)");
        }
        let value = self
            .client()
            .await?
            .request(Method::GET, "/v1/vms", None)
            .await?;
        let query = args.query.as_deref().unwrap_or("").to_ascii_lowercase();
        let mut rows = Vec::new();
        for vm in value
            .as_array()
            .context("Invalid local sandbox inventory")?
        {
            // Adapt addresses UUID resources. Goblin also permits names for VMs
            // created outside Adapt; leave those to Goblin's API.
            if let Some(id) = vm["id"].as_str().and_then(|s| s.parse::<Uuid>().ok())
                && (id.to_string().starts_with(&query)
                    || id.simple().to_string().starts_with(&query))
            {
                rows.push(snapshot(vm.clone(), id)?);
            }
        }
        rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        Ok(json!({"sandboxes":rows,"nextCursor":null}))
    }
    async fn sandbox_get(&self, id: ResourceRef) -> Result<Value> {
        let id = full_id(id)?;
        snapshot(
            self.client()
                .await?
                .request(Method::GET, &format!("/v1/vms/{id}"), None)
                .await?,
            id,
        )
    }
    async fn sandbox_create(&self, args: SandboxCreate) -> Result<Value> {
        validate(&args)?;
        self.client().await?.create(args).await
    }
    async fn sandbox_stop(&self, id: ResourceRef) -> Result<Value> {
        let id = full_id(id)?;
        snapshot(
            self.client()
                .await?
                .request(
                    Method::POST,
                    &format!("/v1/vms/{id}/stop"),
                    Some(&json!({})),
                )
                .await?,
            id,
        )
    }
    async fn sandbox_delete(&self, id: ResourceRef) -> Result<Value> {
        let id = full_id(id)?;
        self.client().await?.delete(id).await?;
        Ok(json!({"id":id,"status":"deleted"}))
    }
    async fn sandbox_wait(&self, id: ResourceRef, status: &str, seconds: u64) -> Result<Value> {
        let id = full_id(id)?;
        if !matches!(
            status,
            "running" | "stopped" | "deleted" | "failed" | "completed"
        ) {
            return self.unsupported(&format!("sandbox wait --status {status}"));
        }
        self.client().await?.wait(id, status, seconds).await
    }
    async fn execute(&self, args: ExecArgs, json: bool) -> Result<u8> {
        full_id(args.sandbox.clone())?;
        if args.detach {
            return self.unsupported("exec --detach");
        }
        if args.creation.idempotency_key.is_some() {
            return self.unsupported("exec --idempotency-key");
        }
        if args.tty && json {
            bail!("--tty cannot be combined with --json");
        }
        if args.tty {
            crate::stream::require_terminal()?;
        }
        self.client().await?.execute(args, json).await
    }
    fn cleanup_hint(&self, id: Uuid) -> String {
        format!(
            "retry `adapt --local sandbox delete {id}` with --local-state-dir {}",
            output::safe(&self.state_dir.display().to_string())
        )
    }
}

fn full_id(id: ResourceRef) -> Result<Uuid> {
    id.uuid()
        .context("Sandbox ID prefixes are not supported by the local backend; use the full UUID")
}

/// Common ID/status/resource fields are consistent across backends. Preserve
/// Goblin's additional diagnostics, including its original state, in JSON.
fn snapshot(mut value: Value, id: Uuid) -> Result<Value> {
    if !value.is_object() {
        bail!("Invalid local sandbox response");
    }
    value["id"] = json!(id);
    value["status"] = json!(match value["state"].as_str() {
        Some("ready" | "busy") => "running",
        Some("starting") => "provisioning",
        Some(state) => state,
        None => "unknown",
    });
    if let Some(cpu) = value["vcpus"].as_u64() {
        value["cpu"] = json!(cpu);
    }
    if let Some(memory) = value["memory_mib"].as_u64() {
        value["memoryMiB"] = json!(memory);
    }
    Ok(value)
}

#[cfg(unix)]
mod metadata {
    use anyhow::{Context, Result, bail};
    use std::{
        collections::BTreeMap,
        fs::{self, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{MetadataExt, OpenOptionsExt},
        path::{Path, PathBuf},
    };
    use uuid::Uuid;

    fn path(state: &Path, id: Uuid) -> PathBuf {
        state.join(format!("adapt-{id}.json"))
    }

    pub fn save(state: &Path, id: Uuid, pairs: &[(String, String)]) -> Result<()> {
        let env: BTreeMap<_, _> = pairs.iter().cloned().collect();
        let bytes = serde_json::to_vec(&env)?;
        if bytes.len() > 65536 {
            bail!("Local sandbox environment exceeds 64 KiB");
        }
        // State was validated as private by discovery. Atomic publication keeps
        // a subsequent exec from reading a partially-written environment.
        let mut file = tempfile::NamedTempFile::new_in(state)?;
        file.write_all(&bytes)?;
        file.as_file().sync_all()?;
        file.persist_noclobber(path(state, id))?;
        Ok(())
    }

    pub fn load(state: &Path, id: Uuid) -> Result<BTreeMap<String, String>> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path(state, id))
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(error) => return Err(error.into()),
        };
        let meta = file.metadata()?;
        // SAFETY: geteuid has no arguments or memory effects.
        if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            bail!("Local sandbox environment must be a private, user-owned regular file");
        }
        let mut bytes = Vec::new();
        file.take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            bail!("Local sandbox environment exceeds 64 KiB");
        }
        serde_json::from_slice(&bytes).context("Invalid local sandbox environment")
    }

    pub fn remove(state: &Path, id: Uuid) -> Result<()> {
        match fs::remove_file(path(state, id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}
