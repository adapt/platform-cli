use super::Sandbox;
#[cfg(unix)]
use crate::stream;
use crate::{
    api,
    cli::{ExecArgs, SandboxCreate},
    output,
};
use anyhow::{Context, Result, bail};
#[cfg(unix)]
use reqwest::header::HeaderMap;
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tokio::time::{Instant, sleep};
#[cfg(unix)]
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use url::Url;
use uuid::Uuid;

pub struct LocalBackend {
    client: Client,
    origin: Url,
    #[cfg(unix)]
    headers: HeaderMap,
    #[cfg(unix)]
    socket_path: PathBuf,
    pub state_dir: PathBuf,
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
            bail!("{flag} is not supported by local temporary sandboxes");
        }
    }
    Ok(())
}

impl LocalBackend {
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

    pub async fn create(&self, args: SandboxCreate) -> Result<Sandbox> {
        validate(&args)?;
        let sandbox = Sandbox {
            id: Uuid::new_v4(),
            #[cfg(unix)]
            env: args.env.into_iter().collect(),
        };
        eprintln!("Creating local sandbox: {}", sandbox.id);
        let body = json!({
            "id": sandbox.id.to_string(), "image": args.image.unwrap_or_else(|| "ubuntu:24.04".into()),
            "resources": {"vcpus": args.cpu.unwrap_or(1), "memory_mib": args.memory.unwrap_or(1024)},
            "state_size_mib": args.disk.unwrap_or(2048),
            "network": {"mode": "user", "egress": "public", "dns": "1.1.1.1"}
        });
        if let Err(error) = self.request(Method::POST, "/v1/vms", Some(&body)).await {
            // The UUID is known even if an accepted request loses its response.
            // Never delete a conflicting record owned by someone else.
            if !error
                .downcast_ref::<LocalError>()
                .is_some_and(|e| e.status == StatusCode::CONFLICT)
                && let Err(cleanup) = self.delete(sandbox.id).await
            {
                return Err(error.context(format!(
                    "Local create may have been accepted as {}; cleanup failed: {cleanup:#}",
                    sandbox.id
                )));
            }
            return Err(error);
        }
        Ok(sandbox)
    }

    pub async fn wait(&self, id: Uuid) -> Result<()> {
        eprintln!("Waiting for local sandbox {id}…");
        tokio::time::timeout(Duration::from_secs(600), async {
            loop {
                let value = self
                    .request(Method::GET, &format!("/v1/vms/{id}"), None)
                    .await?;
                match value["state"].as_str() {
                    Some("ready" | "busy") => return Ok(()),
                    Some("starting") => {}
                    state => bail!(
                        "Local sandbox {id} reached {} before readiness: {}",
                        state.unwrap_or("unknown"),
                        output::safe(
                            value["error"]
                                .as_str()
                                .unwrap_or("inspect the local runtime log")
                        )
                    ),
                }
                sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .context("Timed out waiting for the local sandbox")?
    }

    #[cfg(unix)]
    pub async fn execute(
        &self,
        sandbox: &Sandbox,
        args: ExecArgs,
        json_output: bool,
    ) -> Result<u8> {
        let session = Uuid::new_v4();
        let mut env = sandbox.env.clone();
        env.extend(args.env);
        let mut body = json!({"id":session.to_string(), "argv":args.argv, "env":env, "cwd":args.cwd,
            "stdin":true, "timeout_ms":args.timeout_ms});
        if args.tty {
            let (cols, rows) = crossterm::terminal::size()?;
            body["terminal"] =
                json!({"size":{"rows":rows.max(1),"cols":cols.max(1)},"term":"xterm-256color"});
        }
        let path = format!("/v1/vms/{}/sessions/{session}", sandbox.id);
        self.request(
            Method::POST,
            &format!("/v1/vms/{}/sessions", sandbox.id),
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
                input: true,
                tty: args.tty,
                json_output,
                resume: "The temporary local sandbox will be deleted.".into(),
                credit: Some(0),
            },
        )
        .await
    }

    #[cfg(not(unix))]
    pub async fn execute(&self, _: &Sandbox, _: ExecArgs, _: bool) -> Result<u8> {
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
                Ok(_) => return Ok(()),
                Err(error) => {
                    let status = error.downcast_ref::<LocalError>().map(|e| e.status);
                    if status == Some(StatusCode::NOT_FOUND) {
                        return Ok(());
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
