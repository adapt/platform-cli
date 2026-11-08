use crate::{
    api::{self, Api},
    auth,
    cli::*,
    config::{self, Config, Credential, CredentialKind},
    input, output, shell, stream,
};
use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    path::Path,
    time::Duration,
};
use uuid::Uuid;

pub async fn run(cli: Cli) -> Result<u8> {
    let dir = config::directory(cli.config_dir.as_deref())?;
    let mut config = Config::load(&dir)?;
    let saved = config
        .profiles
        .get(&cli.profile)
        .cloned()
        .unwrap_or_default();
    let origin = config::origin(cli.api_url.as_deref().unwrap_or(&saved.api_url))?;
    let same_origin = config::origin(&saved.api_url)?.origin() == origin.origin();
    let env_token = std::env::var("ADAPT_TOKEN").ok().filter(|s| !s.is_empty());
    let token = env_token.as_deref().or_else(|| {
        same_origin
            .then_some(saved.credential.as_ref())
            .flatten()
            .map(|c| c.token.as_str())
    });
    let organization = cli.org.map(|id| id.to_string()).or_else(|| {
        same_origin
            .then_some(saved.organization_id.clone())
            .flatten()
    });
    let token = if matches!(
        &cli.command,
        Command::Auth(AuthCommand::Login { .. } | AuthCommand::Logout { .. })
    ) {
        None
    } else {
        token
    };
    let api = Api::new(
        origin.clone(),
        token,
        organization,
        Duration::from_secs(cli.timeout),
    )?;
    let result = match cli.command {
        Command::Auth(AuthCommand::Login { no_browser, token_stdin }) => {
            let anonymous = Api::new(origin.clone(), None, None, api.timeout)?;
            let credential = if token_stdin {
                let token = String::from_utf8(input::bytes(Path::new("-"), 65536)?).context("Credential must be UTF-8")?.trim().to_owned();
                if token.is_empty() { bail!("No API key received on stdin"); }
                Credential { token, kind: CredentialKind::ApiKey, expires_at: None }
            } else { auth::device_login(&anonymous, no_browser).await? };
            let mut authenticated = Api::new(origin.clone(), Some(&credential.token), cli.org.map(|id| id.to_string()), api.timeout)?;
            let (identity, organizations) = if credential.kind == CredentialKind::ApiKey {
                let identity = authenticated.get("/v1/whoami").await?;
                if identity["type"] != "apiKey" { bail!("--token-stdin expects an organization API key; use browser login for user sessions"); }
                (identity, None)
            } else {
                // Save the new session before identity and organization discovery so a transient
                // failure after single-use redemption cannot strand the credential.
                config.profiles.insert(cli.profile.clone(), config::Profile { api_url: origin.to_string(), organization_id: None, credential: Some(credential.clone()) });
                config.save(&dir)?;
                let identity = auth::status(&authenticated).await?;
                (identity, Some(auth::organizations(&authenticated).await?))
            };
            let selected = match organizations {
                Some(orgs) => auth::select_organization(&orgs, cli.org.map(|id| id.to_string()).as_deref(), !cli.json).await?,
                None => identity["organizationId"].as_str().map(str::to_owned),
            };
            authenticated.organization = selected.clone();
            config.profiles.insert(cli.profile.clone(), config::Profile { api_url: origin.to_string(), organization_id: selected.clone(), credential: Some(credential) });
            config.save(&dir)?;
            if same_origin && let Some(previous) = saved.credential && previous.kind == CredentialKind::Session {
                let previous_api = Api::new(origin, Some(&previous.token), None, api.timeout)?;
                if let Err(error) = auth::revoke(&previous_api).await { eprintln!("Signed in, but the previous CLI session could not be revoked: {error}. Manage it with `adapt sessions list`."); }
            }
            eprintln!("Signed in. Profile: {}", output::safe(&cli.profile));
            json!({"authenticated":true,"profile":cli.profile,"apiUrl":authenticated.origin.as_str(),"organizationId":selected,"user":identity["user"]})
        },
        Command::Auth(AuthCommand::Logout { local }) => {
            if !same_origin { bail!("This API URL does not match the saved profile; choose that profile's API URL to log out"); }
            // ADAPT_TOKEN must never change which saved credential logout revokes.
            if let Some(credential) = saved.credential.as_ref() {
                if !local && credential.kind == CredentialKind::Session {
                    let saved_api = Api::new(origin, Some(&credential.token), None, api.timeout)?;
                    auth::revoke(&saved_api).await?;
                }
                if !local && credential.kind == CredentialKind::ApiKey {
                    eprintln!("Removed the local API key. Revoke the key with `adapt keys delete ID` if needed.");
                }
            }
            if let Some(profile) = config.profiles.get_mut(&cli.profile) { profile.credential = None; config.save(&dir)?; }
            if env_token.is_some() { eprintln!("ADAPT_TOKEN is still set in your shell."); }
            json!({"loggedOut":true,"profile":cli.profile})
        },
        Command::Auth(AuthCommand::Status) => auth::status(&api).await?,
        Command::Whoami => api.get("/v1/whoami").await?,
        Command::Profiles => Value::Array(config.profiles.iter().map(|(name, profile)| json!({"name":name,"apiUrl":profile.api_url,"organizationId":profile.organization_id,"hasCredential":profile.credential.is_some()})).collect()),
        Command::Org(command) => match command {
            OrgCommand::List => Value::Array(auth::organizations(&api).await?),
            OrgCommand::Use { organization } => {
                let organizations = auth::organizations(&api).await?;
                let selected = auth::select_organization(&organizations, Some(&organization), false).await?.context("No organization selected")?;
                let profile = config.profiles.entry(cli.profile.clone()).or_default();
                if !same_origin { profile.credential = None; }
                profile.api_url = origin.to_string();
                profile.organization_id = Some(selected.clone());
                config.save(&dir)?;
                json!({"profile":cli.profile,"organizationId":selected})
            },
            OrgCommand::Current => json!({"profile":cli.profile,"organizationId":api.organization}),
            OrgCommand::Create { name, slug } => api.post("/v1/auth/organization/create", &json!({"name":name,"slug":slug})).await?,
            OrgCommand::Update { data } => {
                api.post("/v1/auth/organization/update", &json!({"organizationId":org(&api)?,"data":input::object(Some(&data.data))?})).await?
            },
            OrgCommand::Delete => api.post("/v1/auth/organization/delete", &json!({"organizationId":org(&api)?})).await?,
            OrgCommand::Members(command) => members(&api, command).await?,
            OrgCommand::Invitations(command) => invitations(&api, command).await?,
        },
        Command::Sandbox(SandboxCommand::Sh(args)) => return shell::run(&api, args, cli.json).await,
        Command::Sandbox(command) => sandbox(&api, command).await?,
        Command::Exec(args) => return execute(&api, args, cli.json).await,
        Command::Execs(command) => match command {
            ExecCommand::Get { id } => api.get(&format!("/v1/execs/{id}")).await?,
            ExecCommand::Cancel { id } => api.post(&format!("/v1/execs/{id}/cancel"), &json!({})).await?,
            ExecCommand::Logs { id, cursor } => return stream::logs(&api, id, cursor, cli.json).await,
            ExecCommand::Attach { id, after, stdin, tty } => return stream::attach(&api, id, after, stdin || tty, tty, cli.json).await,
        },
        Command::Files(command) => files(&api, command).await?,
        Command::Volumes(command) => volumes(&api, command).await?,
        Command::Endpoints(command) => endpoints(&api, command).await?,
        Command::Images(command) => images(&api, command).await?,
        Command::Env(command) => match command {
            EnvCommand::List(page) => api.list("/v1/environment-variable-sets", "sets", &page, true).await?,
            EnvCommand::Get { id } => api.get(&format!("/v1/environment-variable-sets/{id}")).await?,
            EnvCommand::Create(data) => api.post("/v1/environment-variable-sets", &input::object(Some(&data.data))?).await?,
            EnvCommand::Update { id, data } => put(&api, &format!("/v1/environment-variable-sets/{id}"), input::object(Some(&data.data))?).await?,
            EnvCommand::Delete { id } => delete(&api, &format!("/v1/environment-variable-sets/{id}")).await?,
        },
        Command::Registry(command) => match command {
            RegistryCommand::List(page) => api.list("/v1/registry-credentials", "credentials", &page, true).await?,
            RegistryCommand::Create(data) => api.post("/v1/registry-credentials", &input::object(Some(&data.data))?).await?,
            RegistryCommand::Update { id, data } => put(&api, &format!("/v1/registry-credentials/{id}"), input::object(Some(&data.data))?).await?,
            RegistryCommand::Delete { id } => delete(&api, &format!("/v1/registry-credentials/{id}")).await?,
        },
        Command::Connections(command) => connections(&api, command).await?,
        Command::Keys(command) => keys(&api, command).await?,
        Command::Sessions(command) => match command {
            SessionCommand::List => api.get("/v1/auth/sessions").await?,
            SessionCommand::Revoke { id } => api.post("/v1/auth/sessions/revoke", &json!({"id":id.to_string()})).await?,
            SessionCommand::RevokeOthers => api.post("/v1/auth/sessions/revoke-others", &json!({})).await?,
        },
        Command::Mcp(command) => match command {
            McpCommand::List => api.get("/v1/auth/mcp/connections").await?,
            McpCommand::Revoke { id } => api.post("/v1/auth/mcp/revoke", &json!({"id":id.to_string()})).await?,
        },
        Command::Events { personal } => return stream::events(&api, personal).await,
        Command::Health { ready } => api.get(if ready { "/ready" } else { "/health" }).await?,
        Command::Api(args) => api.json(args.method.parse()?, &args.path, args.data.as_deref().map(input::json).transpose()?.as_ref(), &args.query, args.idempotency_key.as_ref().map(|id| id.to_string()).as_deref()).await?,
    };
    if !result.is_null() {
        output::print(&result, cli.json)?;
    }
    Ok(0)
}

fn choice(value: impl ValueEnum) -> String {
    value
        .to_possible_value()
        .expect("clap enum variant")
        .get_name()
        .to_owned()
}
fn org(api: &Api) -> Result<&str> {
    api.organization
        .as_deref()
        .context("Select an organization with `adapt org use ID` or --org ID")
}
async fn delete(api: &Api, path: &str) -> Result<Value> {
    api.json(Method::DELETE, path, None, &[], None).await
}
async fn put(api: &Api, path: &str, body: Value) -> Result<Value> {
    api.json(Method::PUT, path, Some(&body), &[], None).await
}

async fn create(api: &Api, path: &str, body: Value, creation: Creation) -> Result<Value> {
    let key = creation
        .idempotency_key
        .unwrap_or_else(Uuid::new_v4)
        .to_string();
    eprintln!("Idempotency key: {key}");
    api.json(Method::POST, path, Some(&body), &[], Some(&key))
        .await
        .with_context(|| {
            format!("Creation failed; retry the same inputs with --idempotency-key {key}")
        })
}

async fn sandbox(api: &Api, command: SandboxCommand) -> Result<Value> {
    match command {
        SandboxCommand::List(page) => api.list("/v1/sandboxes", "sandboxes", &page, true).await,
        SandboxCommand::Get { id } => api.get(&format!("/v1/sandboxes/{id}")).await,
        SandboxCommand::Delete { id } => delete(api, &format!("/v1/sandboxes/{id}")).await,
        SandboxCommand::Start { id, wait } => {
            let result = api
                .post(&format!("/v1/sandboxes/{id}/start"), &json!({}))
                .await?;
            if wait {
                wait_for(api, "sandboxes", id, "running", 600).await
            } else {
                Ok(result)
            }
        }
        SandboxCommand::Stop { id, wait } => {
            let result = api
                .post(&format!("/v1/sandboxes/{id}/stop"), &json!({}))
                .await?;
            if wait {
                wait_for(api, "sandboxes", id, "stopped", 600).await
            } else {
                Ok(result)
            }
        }
        SandboxCommand::Wait {
            id,
            status,
            wait_timeout,
        } => wait_for(api, "sandboxes", id, &status, wait_timeout).await,
        SandboxCommand::Create(args) => {
            let wait = args.wait;
            let result = create_sandbox(api, args).await?;
            if wait {
                wait_for(api, "sandboxes", resource_id(&result)?, "running", 600).await
            } else {
                Ok(result)
            }
        }
        SandboxCommand::Sh(_) => unreachable!("Interactive shell is dispatched separately"),
    }
}

pub(crate) async fn create_sandbox(api: &Api, args: SandboxCreate) -> Result<Value> {
    let mut body = input::object(args.data.as_deref())?;
    if args.image.is_some() {
        body.as_object_mut().unwrap().remove("imageId");
    }
    if args.image_id.is_some() {
        body.as_object_mut().unwrap().remove("image");
    }
    input::put(&mut body, "image", args.image)?;
    input::put(&mut body, "imageId", args.image_id.map(|id| id.to_string()))?;
    input::put(&mut body, "cpu", args.cpu)?;
    input::put(&mut body, "memoryMiB", args.memory)?;
    input::put(&mut body, "diskMiB", args.disk)?;
    input::put(&mut body, "ttlSeconds", args.ttl)?;
    if args.no_expiry {
        body["ttlSeconds"] = Value::Null;
    }
    for (field, pairs) in [("env", &args.env), ("labels", &args.labels)] {
        if !pairs.is_empty() {
            if body[field].is_null() {
                body[field] = json!({});
            }
            let map = body[field]
                .as_object_mut()
                .context("env and labels must be JSON objects")?;
            for (k, v) in pairs {
                map.insert(k.clone(), json!(v));
            }
        }
    }
    if !args.variable_set.is_empty() {
        body["variableSetIds"] = json!(
            args.variable_set
                .iter()
                .map(Uuid::to_string)
                .collect::<Vec<_>>()
        );
    }
    if args.run_image {
        body["startup"] = json!({"mode":"image"});
    }
    create(api, "/v1/sandboxes", body, args.creation).await
}

pub(crate) fn resource_id(value: &Value) -> Result<Uuid> {
    value["id"]
        .as_str()
        .context("Response is missing a resource ID")?
        .parse()
        .context("Invalid response resource ID")
}

pub(crate) async fn wait_for(
    api: &Api,
    resource: &str,
    id: Uuid,
    target: &str,
    seconds: u64,
) -> Result<Value> {
    eprintln!("Waiting for {id} to become {target}…");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    tokio::time::timeout_at(deadline, async {
        loop {
            let snapshot = api.get(&format!("/v1/{resource}/{id}")).await?;
            let status = snapshot["status"]
                .as_str()
                .context("Resource response has no status")?;
            if status == target {
                return Ok(snapshot);
            }
            if matches!(status, "failed" | "deleted" | "unresolved" | "completed") {
                bail!(
                    "{resource}/{id} reached {status} before {target}: {}",
                    output::safe(
                        snapshot["error"]
                            .as_str()
                            .or(snapshot["lifecycleError"].as_str())
                            .unwrap_or("inspect the resource for details")
                    )
                );
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })
    .await
    .with_context(|| format!("Timed out waiting for {id}; the operation continues on Platform"))?
}

pub(crate) async fn execute(api: &Api, args: ExecArgs, json_output: bool) -> Result<u8> {
    if args.tty && json_output {
        bail!("--tty cannot be combined with --json");
    }
    if args.tty {
        stream::require_terminal()?;
    }
    let mut body =
        json!({"argv":args.argv,"env":input::pairs(&args.env),"stdin":args.stdin || args.tty});
    input::put(&mut body, "cwd", args.cwd)?;
    input::put(&mut body, "timeoutMs", args.timeout_ms)?;
    if args.tty {
        let (cols, rows) = crossterm::terminal::size()?;
        body["terminal"] = json!({"rows":rows.max(1),"cols":cols.max(1),"term":"xterm-256color"});
    }
    let result = create(
        api,
        &format!("/v1/sandboxes/{}/exec", args.sandbox),
        body,
        args.creation,
    )
    .await?;
    if args.detach {
        output::print(&result, json_output)?;
        return Ok(0);
    }
    let id = resource_id(&result)?;
    eprintln!("Execution: {id}");
    if args.stdin || args.tty {
        stream::attach_new(api, id, args.tty, json_output).await
    } else {
        stream::logs(api, id, 0, json_output).await
    }
}

async fn files(api: &Api, command: FileCommand) -> Result<Value> {
    match command {
        FileCommand::Put {
            sandbox,
            local,
            remote,
        } => {
            let data = input::bytes(&local, input::MAX_FILE_BYTES)?;
            let response = api
                .request(Method::PUT, &format!("/v1/sandboxes/{sandbox}/files"))?
                .query(&[("path", &remote)])
                .header("Content-Type", "application/octet-stream")
                .body(data)
                .timeout(api.timeout)
                .send()
                .await?;
            api::checked(response).await?;
            Ok(json!({"uploaded":remote,"sandboxId":sandbox.to_string()}))
        }
        FileCommand::Get {
            sandbox,
            remote,
            local,
            force,
        } => {
            let stdout = local == Path::new("-");
            if !stdout && local.exists() && !force {
                bail!(
                    "{} already exists; pass --force to replace it",
                    local.display()
                );
            }
            let response = api
                .request(Method::GET, &format!("/v1/sandboxes/{sandbox}/files"))?
                .query(&[("path", &remote)])
                .timeout(api.timeout)
                .send()
                .await?;
            let data = api::limited_bytes(
                api::ensure_success(response).await?,
                input::MAX_FILE_BYTES as usize,
            )
            .await?;
            if stdout {
                io::stdout().lock().write_all(&data)?;
                return Ok(Value::Null);
            }
            let parent = local
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(&data)?;
            file.as_file().sync_all()?;
            if force {
                file.persist(&local)?;
            } else {
                file.persist_noclobber(&local)?;
            }
            Ok(json!({"downloaded":local,"bytes":data.len()}))
        }
    }
}

async fn volumes(api: &Api, command: VolumeCommand) -> Result<Value> {
    match command {
        VolumeCommand::List(page) => api.list("/v1/volumes", "volumes", &page, true).await,
        VolumeCommand::Get { id } => api.get(&format!("/v1/volumes/{id}")).await,
        VolumeCommand::Delete { id } => delete(api, &format!("/v1/volumes/{id}")).await,
        VolumeCommand::Create {
            quota_bytes,
            creation,
        } => {
            create(
                api,
                "/v1/volumes",
                json!({"quotaBytes":quota_bytes}),
                creation,
            )
            .await
        }
        VolumeCommand::Quota {
            id,
            bytes,
            unlimited: _,
        } => {
            put(
                api,
                &format!("/v1/volumes/{id}/quota"),
                json!({"quotaBytes":bytes}),
            )
            .await
        }
    }
}

async fn endpoints(api: &Api, command: EndpointCommand) -> Result<Value> {
    match command {
        EndpointCommand::List(page) => api.list("/v1/ingresses", "ingresses", &page, true).await,
        EndpointCommand::Get { id } => api.get(&format!("/v1/ingresses/{id}")).await,
        EndpointCommand::Create {
            name,
            access,
            creation,
        } => {
            create(
                api,
                "/v1/ingresses",
                json!({"name":name,"accessMode":choice(access)}),
                creation,
            )
            .await
        }
        EndpointCommand::Delete { id } => delete(api, &format!("/v1/ingresses/{id}")).await,
        EndpointCommand::Bind { id, sandbox, port } => {
            put(
                api,
                &format!("/v1/ingresses/{id}/binding"),
                json!({"sandboxId":sandbox.to_string(),"port":port}),
            )
            .await
        }
        EndpointCommand::Unbind { id } => delete(api, &format!("/v1/ingresses/{id}/binding")).await,
        EndpointCommand::Access { id, access } => {
            put(
                api,
                &format!("/v1/ingresses/{id}/access"),
                json!({"accessMode":choice(access)}),
            )
            .await
        }
        EndpointCommand::Token { id } => {
            api.post(&format!("/v1/ingresses/{id}/access-tokens"), &json!({}))
                .await
        }
        EndpointCommand::RevokeTokens { id } => {
            delete(api, &format!("/v1/ingresses/{id}/access-tokens")).await
        }
    }
}

async fn images(api: &Api, command: ImageCommand) -> Result<Value> {
    match command {
        ImageCommand::List(page) => api.list("/v1/images", "images", &page, true).await,
        ImageCommand::Get { id } => api.get(&format!("/v1/images/{id}")).await,
        ImageCommand::Delete { id } => delete(api, &format!("/v1/images/{id}")).await,
        ImageCommand::Wait { id, wait_timeout } => {
            wait_for(api, "images", id, "ready", wait_timeout).await
        }
        ImageCommand::Import {
            source,
            name,
            architecture,
            credential,
            wait,
            creation,
        } => {
            let mut body = json!({"source":source,"name":name,"architecture":choice(architecture)});
            input::put(
                &mut body,
                "registryCredentialId",
                credential.map(|id| id.to_string()),
            )?;
            let result = create(api, "/v1/images", body, creation).await?;
            if wait {
                wait_for(api, "images", resource_id(&result)?, "ready", 600).await
            } else {
                Ok(result)
            }
        }
        ImageCommand::Pull {
            id,
            credential,
            public,
            wait,
            creation,
        } => {
            let mut body = json!({});
            input::put(
                &mut body,
                "registryCredentialId",
                credential.map(|id| id.to_string()),
            )?;
            if public {
                body["registryCredentialId"] = Value::Null;
            }
            let result = create(api, &format!("/v1/images/{id}/pull"), body, creation).await?;
            if wait {
                wait_for(api, "images", resource_id(&result)?, "ready", 600).await
            } else {
                Ok(result)
            }
        }
    }
}

async fn connections(api: &Api, command: ConnectionCommand) -> Result<Value> {
    let root = "/v1/gateway/connections";
    match command {
        ConnectionCommand::List(page) => api.list(root, "connections", &page, false).await,
        ConnectionCommand::Get { id } => api.get(&format!("{root}/{id}")).await,
        ConnectionCommand::Create(data) => api.post(root, &input::object(Some(&data.data))?).await,
        ConnectionCommand::Update { id, data } => put(api, &format!("{root}/{id}"), input::object(Some(&data.data))?).await,
        ConnectionCommand::Delete { id } => delete(api, &format!("{root}/{id}")).await,
        ConnectionCommand::Refresh { id } => api.post(&format!("{root}/{id}/refresh"), &json!({})).await,
        ConnectionCommand::Grant { id, data } => put(api, &format!("{root}/{id}/grants"), input::object(Some(&data.data))?).await,
        ConnectionCommand::Oauth { id, no_browser } => {
            let result = api.post(&format!("{root}/{id}/oauth"), &json!({})).await?;
            let url = result["url"].as_str().context("Missing OAuth URL")?;
            if !no_browser && let Err(error) = auth::open_browser(url) { eprintln!("{error}. Open the returned URL manually."); }
            Ok(result)
        },
        ConnectionCommand::OauthComplete { id, data } => api.post(&format!("{root}/{id}/oauth/complete"), &input::object(Some(&data.data))?).await,
        ConnectionCommand::Calls { id, page } => api.list(&format!("{root}/{id}/calls"), "calls", &page, false).await,
        ConnectionCommand::Principals => api.get("/v1/gateway/principals").await,
        ConnectionCommand::Search { query, offset } => api.json(Method::GET, "/v1/gateway/search", None, &[("query".into(),query),("offset".into(),offset.to_string())], None).await,
        ConnectionCommand::Describe { connection, tool } => api.post("/v1/gateway/describe", &json!({"connectionId":connection.to_string(),"tool":tool})).await,
        ConnectionCommand::Call { connection, tool, arguments } => api.post("/v1/gateway/call", &json!({"connectionId":connection.to_string(),"tool":tool,"arguments":input::object(Some(&arguments))?})).await,
    }
}

async fn keys(api: &Api, command: KeyCommand) -> Result<Value> {
    match command {
        KeyCommand::List { offset } => {
            api.json(
                Method::GET,
                "/v1/auth/api-key/list",
                None,
                &[
                    ("configId".into(), "platform".into()),
                    ("organizationId".into(), org(api)?.into()),
                    ("limit".into(), "50".into()),
                    ("offset".into(), offset.to_string()),
                ],
                None,
            )
            .await
        }
        KeyCommand::Create {
            name,
            expires_in,
            permissions,
        } => {
            let mut body = json!({"configId":"platform","organizationId":org(api)?,"name":name});
            input::put(&mut body, "expiresIn", expires_in)?;
            input::put(
                &mut body,
                "permissions",
                permissions
                    .as_deref()
                    .map(|s| input::object(Some(s)))
                    .transpose()?,
            )?;
            api.post("/v1/auth/api-key/create", &body).await
        }
        KeyCommand::Delete { id } => {
            api.post(
                "/v1/auth/api-key/delete",
                &json!({"configId":"platform","keyId":id}),
            )
            .await
        }
    }
}

async fn members(api: &Api, command: MemberCommand) -> Result<Value> {
    match command {
        MemberCommand::List { offset } => {
            api.json(
                Method::GET,
                "/v1/auth/organization/list-members",
                None,
                &[
                    ("organizationId".into(), org(api)?.into()),
                    ("limit".into(), "50".into()),
                    ("offset".into(), offset.to_string()),
                ],
                None,
            )
            .await
        }
        MemberCommand::Role { id, role } => {
            api.post(
                "/v1/auth/organization/update-member-role",
                &json!({"organizationId":org(api)?,"memberId":id.to_string(),"role":choice(role)}),
            )
            .await
        }
        MemberCommand::Remove { id_or_email } => {
            api.post(
                "/v1/auth/organization/remove-member",
                &json!({"organizationId":org(api)?,"memberIdOrEmail":id_or_email}),
            )
            .await
        }
    }
}

async fn invitations(api: &Api, command: InvitationCommand) -> Result<Value> {
    match command {
        InvitationCommand::List => api.json(Method::GET, "/v1/auth/organization/list-invitations", None, &[("organizationId".into(),org(api)?.into())], None).await,
        InvitationCommand::Create { email, role, link, resend } => api.json(Method::POST, "/v1/auth/organization/invite-member", Some(&json!({"organizationId":org(api)?,"email":email,"role":choice(role),"resend":resend})), &[("delivery".into(),if link {"link"} else {"email"}.into())], None).await,
        InvitationCommand::Cancel { id } => api.post("/v1/auth/organization/cancel-invitation", &json!({"invitationId":id.to_string()})).await,
        InvitationCommand::Accept { id } => api.post("/v1/auth/organization/accept-invitation", &json!({"invitationId":id.to_string()})).await,
        InvitationCommand::Reject { id } => api.post("/v1/auth/organization/reject-invitation", &json!({"invitationId":id.to_string()})).await,
    }
}
