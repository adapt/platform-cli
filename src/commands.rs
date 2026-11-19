use crate::{
    api::{self, Api},
    auth,
    cli::*,
    config::{self, Config, Credential, CredentialKind},
    input, mcp, output, shell, skill, stream,
};
use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use futures_util::StreamExt;
use reqwest::{
    Method,
    header::{HeaderMap, HeaderValue, LOCATION},
};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    path::Path,
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use url::Url;
use uuid::Uuid;

pub async fn run(cli: Cli) -> Result<u8> {
    if let Command::Skill(command) = &cli.command {
        skill::run(command, cli.json)?;
        return Ok(0);
    }
    let dir = config::directory(cli.config_dir.as_deref())?;
    let mut config = Config::load(&dir)?;
    // --profile and ADAPT_PROFILE share one value (the flag wins), so one check covers both.
    // "default" is exempt because a fresh install has no saved profiles. These commands
    // create or list profiles, so a missing name is expected there.
    if cli.profile != "default"
        && !config.profiles.contains_key(&cli.profile)
        && !matches!(
            &cli.command,
            Command::Auth(AuthCommand::Login { .. })
                | Command::Org(OrgCommand::Use { .. })
                | Command::Profiles
        )
    {
        eprintln!(
            "Warning: profile '{}' does not exist; using defaults. Run `adapt profiles` to list saved profiles or `adapt --profile <name> auth login` to create one.",
            output::safe(&cli.profile)
        );
    }
    let saved = config
        .profiles
        .get(&cli.profile)
        .cloned()
        .unwrap_or_default();
    let origin = config::origin(cli.api_url.as_deref().unwrap_or(&saved.api_url))?;
    let same_origin = config::origin(&saved.api_url)?.origin() == origin.origin();
    let env_token = std::env::var("ADAPT_TOKEN").ok().filter(|s| !s.is_empty());
    // Platform's MCP server rejects sign-in sessions, so `mcp tools` refuses one up front.
    let session_credential = env_token.is_none()
        && same_origin
        && saved
            .credential
            .as_ref()
            .is_some_and(|c| c.kind == CredentialKind::Session);
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
        Command::Skill(_) => unreachable!("Skill commands run before API configuration"),
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
        Command::Sandbox(SandboxCommand::List(args)) => return list_sandboxes(&api, args, cli.json).await,
        Command::Sandbox(SandboxCommand::Usage { id, hours }) => return sandbox_usage(&api, id, hours, cli.json).await,
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
        Command::Endpoints(EndpointCommand::Request(args)) => return request_endpoint(&api, args).await,
        Command::Endpoints(command) => endpoints(&api, command).await?,
        Command::Images(ImageCommand::List(args)) => return list_images(&api, args, cli.json).await,
        Command::Images(command) => images(&api, command).await?,
        Command::Env(command) => match command {
            EnvCommand::List(page) => api.list("/v1/environment-variable-sets", "sets", &page, true, &[]).await?,
            EnvCommand::Get { id } => api.get(&format!("/v1/environment-variable-sets/{id}")).await?,
            EnvCommand::Create(data) => api.post("/v1/environment-variable-sets", &input::object(Some(&data.data))?).await?,
            EnvCommand::Update { id, data } => put(&api, &format!("/v1/environment-variable-sets/{id}"), input::object(Some(&data.data))?).await?,
            EnvCommand::Delete { id } => delete(&api, &format!("/v1/environment-variable-sets/{id}")).await?,
        },
        Command::Registry(command) => match command {
            RegistryCommand::List(page) => api.list("/v1/registry-credentials", "credentials", &page, true, &[]).await?,
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
        Command::Mcp(McpCommand::Tools { name }) => return mcp::tools(&api, name, session_credential, cli.json).await,
        Command::Mcp(command) => match command {
            McpCommand::Tools { .. } => unreachable!("MCP tool lists are dispatched separately"),
            McpCommand::List => api.get("/v1/auth/mcp/connections").await?,
            McpCommand::Revoke { id } => api.post("/v1/auth/mcp/revoke", &json!({"id":id.to_string()})).await?,
        },
        Command::Usage(UsageCommand::List(args)) => return list_usage(&api, args, cli.json).await,
        Command::Usage(UsageCommand::Summary(args)) => return usage_summary(&api, args, cli.json).await,
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

async fn list_sandboxes(api: &Api, args: SandboxList, json: bool) -> Result<u8> {
    let filters: Vec<_> = args
        .query
        .iter()
        .map(|text| ("query".to_owned(), text.clone()))
        .collect();
    let no_match = args
        .query
        .map(|text| format!("No sandboxes match \"{}\".", output::safe(&text)));
    filtered_list(
        api,
        "/v1/sandboxes",
        "sandboxes",
        &args.page,
        &filters,
        no_match,
        json,
    )
    .await
}

async fn list_images(api: &Api, args: ImageList, json: bool) -> Result<u8> {
    let status = args.status.map(choice);
    let mut filters = Vec::new();
    if let Some(name) = &args.name {
        filters.push(("name".to_owned(), name.clone()));
    }
    if let Some(status) = &status {
        filters.push(("status".to_owned(), status.clone()));
    }
    let no_match = (!filters.is_empty()).then(|| {
        let mut message = match &status {
            Some(status) => format!("No {status} images"),
            None => "No images".to_owned(),
        };
        if let Some(name) = &args.name {
            message += &format!(" named \"{}\"", output::safe(name));
        }
        message + "."
    });
    filtered_list(
        api,
        "/v1/images",
        "images",
        &args.page,
        &filters,
        no_match,
        json,
    )
    .await
}

/// Lists a resource with filters that Platform applies to every page. When nothing
/// matches, `no_match` names the active filters instead of a bare "No results."
async fn filtered_list(
    api: &Api,
    path: &str,
    field: &str,
    page: &Page,
    filters: &[(String, String)],
    no_match: Option<String>,
    json: bool,
) -> Result<u8> {
    let result = api.list(path, field, page, true, filters).await?;
    if let Some(message) = no_match
        && result[field].as_array().is_some_and(Vec::is_empty)
    {
        if output::tables(json) {
            println!("{message}");
            return Ok(0);
        }
        // Scripts still receive the empty JSON envelope on stdout.
        eprintln!("{message}");
    }
    output::print(&result, json)?;
    Ok(0)
}

async fn sandbox(api: &Api, command: SandboxCommand) -> Result<Value> {
    match command {
        SandboxCommand::List(_) => unreachable!("Sandbox lists are dispatched separately"),
        SandboxCommand::Get { id } => api.get(&format!("/v1/sandboxes/{id}")).await,
        SandboxCommand::Delete { id } => delete(api, &format!("/v1/sandboxes/{id}")).await,
        SandboxCommand::Start { id, wait } => {
            let result = api
                .post(&format!("/v1/sandboxes/{id}/start"), &json!({}))
                .await?;
            if wait {
                wait_for(api, "sandboxes", resource_id(&result)?, "running", 600).await
            } else {
                Ok(result)
            }
        }
        SandboxCommand::Stop { id, wait } => {
            let result = api
                .post(&format!("/v1/sandboxes/{id}/stop"), &json!({}))
                .await?;
            if wait {
                wait_for(api, "sandboxes", resource_id(&result)?, "stopped", 600).await
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
        SandboxCommand::Usage { .. } => unreachable!("Sandbox usage is dispatched separately"),
    }
}

async fn sandbox_usage(api: &Api, id: input::ResourceRef, hours: String, json: bool) -> Result<u8> {
    let path = format!("/v1/sandboxes/{id}/usage");
    let usage = api
        .json(
            Method::GET,
            &path,
            None,
            &[("hours".into(), hours.clone())],
            None,
        )
        .await?;
    if output::tables(json) {
        write_usage(&mut io::stdout().lock(), &usage, &hours)?;
    } else {
        output::print(&usage, json)?;
    }
    Ok(0)
}

const USAGE_COLUMNS: [&str; 7] = [
    "status",
    "cpu observed",
    "cpu",
    "avg cpu",
    "memory observed",
    "memory",
    "avg memory",
];

/// Writes one table row, then notes any migrated allocation estimates.
fn write_usage(out: &mut impl Write, usage: &Value, hours: &str) -> io::Result<()> {
    output::table(out, &USAGE_COLUMNS, &[usage_row(usage, hours)])?;
    estimated_note(out, &usage["totals"])
}

/// Notes totals that include migrated allocation estimates, which would otherwise
/// look the same as measurements.
fn estimated_note(out: &mut impl Write, totals: &Value) -> io::Result<()> {
    match totals["estimatedIntervals"].as_u64() {
        None | Some(0) => Ok(()),
        Some(n) => {
            let plural = if n == 1 { "" } else { "s" };
            writeln!(
                out,
                "\nIncludes {n} interval{plural} estimated from allocated resources, not measured."
            )
        }
    }
}

const GIB: f64 = (1u64 << 30) as f64;

/// Reads one of Platform's exact decimal totals; f64 precision suffices for display.
fn exact(totals: &Value, key: &str) -> f64 {
    totals[key]
        .as_str()
        .and_then(|text| text.parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// CPU time, or None (shown as —) without observed time, because zero coverage
/// is not evidence of idling.
fn cpu_time(totals: &Value) -> Option<String> {
    (totals["observedMs"].as_f64().unwrap_or(0.0) > 0.0)
        .then(|| resource_time(exact(totals, "cpuNanoseconds") / 1e9, "CPU"))
}

/// Resident memory time, or None (shown as —) without observed memory time.
fn memory_time(totals: &Value) -> Option<String> {
    (totals["memoryObservedMs"].as_f64().unwrap_or(0.0) > 0.0).then(|| {
        resource_time(
            exact(totals, "residentByteMilliseconds") / (GIB * 1e3),
            "GiB",
        )
    })
}

/// Formats resource-seconds in hours, or in seconds below 0.01 hours so short
/// runs don't round to zero, e.g. 1.25 CPU-h or 19.5 CPU-s.
fn resource_time(seconds: f64, unit: &str) -> String {
    if seconds >= 36.0 {
        format!("{:.2} {unit}-h", seconds / 3600.0)
    } else {
        format!("{seconds:.1} {unit}-s")
    }
}

/// Converts exact usage totals to readable units.
fn usage_row(usage: &Value, hours: &str) -> Value {
    let totals = &usage["totals"];
    let (cpu, ram) = (
        exact(totals, "cpuNanoseconds"),
        exact(totals, "residentByteMilliseconds"),
    );
    let observed = totals["observedMs"].as_f64().unwrap_or(0.0);
    let ram_observed = totals["memoryObservedMs"].as_f64().unwrap_or(0.0);
    json!({
        "status": usage["status"],
        "cpu observed": format!("{} of {hours}h", duration(observed)),
        "cpu": cpu_time(totals),
        "avg cpu": (observed > 0.0).then(|| format!("{:.2} vCPU", cpu / (observed * 1e6))),
        "memory observed": format!("{} of {hours}h", duration(ram_observed)),
        "memory": memory_time(totals),
        "avg memory": (ram_observed > 0.0).then(|| {
            let bytes = ram / ram_observed;
            if bytes < GIB {
                format!("{:.0} MiB", bytes / f64::from(1u32 << 20))
            } else {
                format!("{:.1} GiB", bytes / GIB)
            }
        }),
    })
}

fn duration(ms: f64) -> String {
    let seconds = (ms / 1000.0) as u64;
    match (seconds / 3600, seconds % 3600 / 60) {
        (0, 0) => format!("{seconds}s"),
        (0, minutes) => format!("{minutes}m"),
        (hours, 0) => format!("{hours}h"),
        (hours, minutes) => format!("{hours}h {minutes}m"),
    }
}

async fn list_usage(api: &Api, args: UsageList, json: bool) -> Result<u8> {
    let mut filters = usage_scope(&args.scope);
    if let Some(days) = args.days {
        filters.push(("days".into(), days));
    }
    if let Some(sandbox) = args.sandbox {
        filters.push(("sandboxId".into(), sandbox.to_string()));
    }
    let result = api
        .list("/v1/usage", "events", &args.page, true, &filters)
        .await?;
    if output::tables(json) {
        write_usage_events(&mut io::stdout().lock(), &result)?;
    } else {
        output::print(&result, json)?;
    }
    Ok(0)
}

async fn usage_summary(api: &Api, args: UsageSummary, json: bool) -> Result<u8> {
    let mut query = usage_scope(&args.scope);
    query.push(("days".into(), args.days));
    let summary = api
        .json(Method::GET, "/v1/usage/summary", None, &query, None)
        .await?;
    if output::tables(json) {
        write_usage_summary(&mut io::stdout().lock(), &summary)?;
    } else {
        output::print(&summary, json)?;
    }
    Ok(0)
}

/// Query parameters shared by usage lists and summaries.
fn usage_scope(scope: &UsageScope) -> Vec<(String, String)> {
    let mut query = vec![("kind".to_owned(), choice(scope.kind))];
    if let Some(user) = scope.user {
        query.push(("userId".into(), user.to_string()));
    }
    query
}

/// Writes one row per sandbox run or model call; missing measurements show as —.
fn write_usage_events(out: &mut impl Write, result: &Value) -> io::Result<()> {
    let events = result["events"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let Some(first) = events.first() else {
        return writeln!(out, "No usage.");
    };
    let took = |event: &Value| event["usage"]["durationMs"].as_f64().map(duration);
    if first["kind"] == "model" {
        let rows: Vec<Value> = events
            .iter()
            .map(|event| {
                json!({
                    "created (utc)": minute(&event["createdAt"]),
                    "model": event["details"]["model"],
                    "credentials": event["details"]["credentialSource"],
                    "status": event["details"]["status"],
                    "duration": took(event),
                    "input tokens": event["usage"]["inputTokens"],
                    "output tokens": event["usage"]["outputTokens"],
                })
            })
            .collect();
        let keys = [
            "created (utc)",
            "model",
            "credentials",
            "status",
            "duration",
            "input tokens",
            "output tokens",
        ];
        output::table(out, &keys, &rows)?;
    } else {
        let rows: Vec<Value> = events
            .iter()
            .map(|event| {
                json!({
                    "created (utc)": minute(&event["createdAt"]),
                    "sandbox": event["sandboxId"],
                    "status": event["status"],
                    "duration": took(event),
                    "cpu": cpu_time(&event["usage"]),
                    "memory": memory_time(&event["usage"]),
                })
            })
            .collect();
        let keys = [
            "created (utc)",
            "sandbox",
            "status",
            "duration",
            "cpu",
            "memory",
        ];
        output::table(out, &keys, &rows)?;
    }
    output::next_page(out, result)
}

/// Writes one row per member plus a total row, then notes incomplete data.
fn write_usage_summary(out: &mut impl Write, summary: &Value) -> io::Result<()> {
    let members = summary["members"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let named = members
        .iter()
        .map(|member| (member["name"].clone(), member))
        .chain([(json!("Total"), &summary["totals"])]);
    if summary["kind"] == "model" {
        let rows: Vec<Value> = named
            .map(|(name, m)| {
                json!({
                    "member": name,
                    "calls": m["calls"],
                    "input tokens": m["inputTokens"],
                    "output tokens": m["outputTokens"],
                })
            })
            .collect();
        output::table(
            out,
            &["member", "calls", "input tokens", "output tokens"],
            &rows,
        )?;
        match summary["missingTokenCalls"].as_u64() {
            None | Some(0) => Ok(()),
            Some(n) => {
                let plural = if n == 1 { "" } else { "s" };
                writeln!(
                    out,
                    "\n{n} call{plural} did not report input or output token counts."
                )
            }
        }
    } else {
        let rows: Vec<Value> = named
            .map(|(name, m)| {
                json!({
                    "member": name,
                    "runs": m["runs"],
                    "cpu": cpu_time(m),
                    "memory": memory_time(m),
                })
            })
            .collect();
        output::table(out, &["member", "runs", "cpu", "memory"], &rows)?;
        estimated_note(out, &summary["totals"])
    }
}

/// Shortens an ISO timestamp to the minute, e.g. 2026-11-18 14:02.
fn minute(at: &Value) -> Option<String> {
    let at = at.as_str()?;
    Some(format!("{} {}", at.get(..10)?, at.get(11..16)?))
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
                .map(ToString::to_string)
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
    id: impl Into<input::ResourceRef>,
    target: &str,
    seconds: u64,
) -> Result<Value> {
    let mut id = id.into();
    eprintln!("Waiting for {id} to become {target}…");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
    tokio::time::timeout_at(deadline, async {
        loop {
            let snapshot = api.get(&format!("/v1/{resource}/{id}")).await?;
            // Once selected, follow this resource even if the prefix later
            // becomes ambiguous or the sandbox reaches the deleted state.
            if id.uuid().is_none() {
                id = resource_id(&snapshot)?.into();
            }
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
            let response = api::ensure_success(response).await?;
            let id = response
                .headers()
                .get("X-Sandbox-Id")
                .map(|value| -> Result<Uuid> { Ok(value.to_str()?.parse()?) })
                .transpose()?
                .or_else(|| sandbox.uuid())
                .context("File upload response is missing the resolved sandbox ID")?;
            api::checked(response).await?;
            Ok(json!({"uploaded":remote,"sandboxId":id.to_string()}))
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
        VolumeCommand::List(page) => api.list("/v1/volumes", "volumes", &page, true, &[]).await,
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
        EndpointCommand::List(page) => {
            api.list("/v1/ingresses", "ingresses", &page, true, &[])
                .await
        }
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
        EndpointCommand::Request(_) => unreachable!("Endpoint requests are dispatched separately"),
    }
}

/// Sends one request to an endpoint's hosted application and streams the body to
/// stdout. Platform credentials only look up the endpoint and create a token; the
/// application receives the caller's headers and, at most, the endpoint token.
async fn request_endpoint(api: &Api, args: EndpointRequest) -> Result<u8> {
    let path = &args.path;
    if !path.starts_with('/') || path.starts_with("//") || path.contains('\\') || path.contains('#')
    {
        bail!("Use an absolute path such as /events, without a host or fragment");
    }
    let mut headers = HeaderMap::new();
    for header in &args.headers {
        let (name, value) = input::header(header)?;
        headers.append(name, value);
    }
    let endpoint = api.get(&format!("/v1/ingresses/{}", args.id)).await?;
    let base = Url::parse(endpoint["url"].as_str().context("Endpoint has no URL")?)
        .context("Endpoint has an invalid URL")?;
    config::validate_http_url(&base)?;
    if base.query().is_some() || base.fragment().is_some() {
        bail!("Endpoint URL has an unexpected query or fragment");
    }
    // Append like `curl "${ENDPOINT_URL}events"`, keeping any path prefix in the URL.
    let url = Url::parse(&format!("{}{path}", base.as_str().trim_end_matches('/')))?;
    if url.origin() != base.origin() {
        bail!("Requests must stay on the endpoint's origin");
    }
    let token = if args.new_token {
        let id = resource_id(&endpoint)?;
        let created = api
            .post(&format!("/v1/ingresses/{id}/access-tokens"), &json!({}))
            .await?;
        let token = created["token"].as_str();
        Some(
            token
                .context("Token response is missing the token")?
                .to_owned(),
        )
    } else {
        args.token
    };
    if let Some(token) = token {
        if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c == ',') {
            bail!("Invalid endpoint token");
        }
        let mut value =
            HeaderValue::from_str(&format!("Bearer {token}")).context("Invalid endpoint token")?;
        value.set_sensitive(true);
        headers.insert("X-Adapt-Authorization", value);
    }
    // Never api.request(): it attaches Platform credentials. This client follows no redirects.
    let mut request = api
        .client
        .request(args.method.parse()?, url)
        .headers(headers);
    if let Some(source) = args.data {
        // Even @file can block on a FIFO or device. Runtime shutdown does not wait
        // for this worker after Ctrl-C, so neither open nor read can delay exit.
        let body = tokio::task::spawn_blocking(move || input::data(&source))
            .await
            .context("Request body reader failed")??;
        request = request.body(body);
    }
    // Error URLs can carry credentials in their query, so they are removed.
    let response = tokio::time::timeout(api.timeout, request.send())
        .await
        .context("Timed out waiting for the endpoint to respond")?
        .map_err(reqwest::Error::without_url)
        .context("Endpoint request failed")?;
    let status = response.status();
    let location = response.headers().get(LOCATION).cloned();
    let mut stream = response.bytes_stream();
    let mut out = tokio::io::stdout();
    while let Some(chunk) = tokio::time::timeout(api.timeout, stream.next())
        .await
        .context("Endpoint response was idle for longer than --timeout")?
    {
        let chunk = chunk
            .map_err(reqwest::Error::without_url)
            .context("Endpoint response was interrupted")?;
        out.write_all(&chunk).await?;
        out.flush().await?;
    }
    if status.is_success() {
        return Ok(0);
    }
    let mut message = format!("Endpoint returned HTTP {status}");
    if status.is_redirection() {
        message += "; redirects are not followed";
        if let Some(location) = location {
            let location = String::from_utf8_lossy(location.as_bytes());
            message += &format!(" (Location: {})", output::safe(&location));
        }
    }
    bail!(message)
}

async fn images(api: &Api, command: ImageCommand) -> Result<Value> {
    match command {
        ImageCommand::List(_) => unreachable!("Image lists are dispatched separately"),
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
        ConnectionCommand::List(page) => api.list(root, "connections", &page, false, &[]).await,
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
        ConnectionCommand::Calls { id, page } => api.list(&format!("{root}/{id}/calls"), "calls", &page, false, &[]).await,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn render(usage: Value, hours: &str) -> String {
        let mut out = Vec::new();
        write_usage(&mut out, &usage, hours).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn usage_converts_exact_totals_to_readable_units() {
        let usage = json!({"status":"complete","totals":{
            "cpuNanoseconds":"27007200000000",
            "residentByteMilliseconds":"43138651521024000",
            "observedMs":22320000,
            "memoryObservedMs":22320000
        }});
        assert_eq!(
            render(usage, "24"),
            concat!(
                "STATUS    CPU OBSERVED   CPU         AVG CPU    MEMORY OBSERVED  MEMORY       AVG MEMORY\n",
                "complete  6h 12m of 24h  7.50 CPU-h  1.21 vCPU  6h 12m of 24h    11.16 GiB-h  1.8 GiB   \n",
            )
        );
    }

    #[test]
    fn usage_flags_allocation_estimates_under_the_table() {
        let usage = |estimated: u64| {
            json!({"status":"complete","totals":{
                "cpuNanoseconds":"3600000000000","residentByteMilliseconds":"0",
                "observedMs":3600000,"memoryObservedMs":0,"estimatedIntervals":estimated
            }})
        };
        let note = "\nIncludes 3 intervals estimated from allocated resources, not measured.\n";
        assert!(render(usage(3), "24").ends_with(note));
        assert!(
            render(usage(1), "24").ends_with(
                "Includes 1 interval estimated from allocated resources, not measured.\n"
            )
        );
        assert_eq!(render(usage(0), "24").lines().count(), 2);
    }

    #[test]
    fn usage_shows_memory_coverage_separately_from_cpu() {
        let usage = json!({"status":"complete","totals":{
            "cpuNanoseconds":"3600000000000",
            "residentByteMilliseconds":"386547056640000",
            "observedMs":3600000,
            "memoryObservedMs":360000
        }});
        assert_eq!(
            render(usage, "1"),
            concat!(
                "STATUS    CPU OBSERVED  CPU         AVG CPU    MEMORY OBSERVED  MEMORY      AVG MEMORY\n",
                "complete  1h of 1h      1.00 CPU-h  1.00 vCPU  6m of 1h         0.10 GiB-h  1.0 GiB   \n",
            )
        );
    }

    #[test]
    fn usage_without_observations_shows_dashes_instead_of_zero() {
        let empty = json!({"status":"pending","totals":{
            "cpuNanoseconds":"0","residentByteMilliseconds":"0","observedMs":0,"memoryObservedMs":0
        }});
        assert_eq!(
            render(empty, "1").lines().nth(1),
            Some("pending  0s of 1h      —    —        0s of 1h         —       —         ")
        );
        // CPU can be observed while memory sampling is still unavailable.
        let no_memory = json!({"status":"incomplete","totals":{
            "cpuNanoseconds":"1513600000000","residentByteMilliseconds":"0",
            "observedMs":3520000,"memoryObservedMs":0
        }});
        assert_eq!(
            render(no_memory, "1").lines().nth(1),
            Some(
                "incomplete  58m of 1h     0.42 CPU-h  0.43 vCPU  0s of 1h         —       —         "
            )
        );
    }

    fn written(write: fn(&mut Vec<u8>, &Value) -> io::Result<()>, value: Value) -> String {
        let mut out = Vec::new();
        write(&mut out, &value).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// Splits table lines into cells; column padding is covered by the output tests.
    fn cells(rendered: &str) -> Vec<Vec<&str>> {
        rendered
            .lines()
            .map(|line| {
                line.split("  ")
                    .map(str::trim)
                    .filter(|cell| !cell.is_empty())
                    .collect()
            })
            .collect()
    }

    const ID: &str = "11111111-1111-4111-8111-111111111111";

    #[test]
    fn short_totals_show_seconds_instead_of_rounding_to_zero() {
        assert_eq!(resource_time(0.54, "CPU"), "0.5 CPU-s");
        assert_eq!(resource_time(35.9, "GiB"), "35.9 GiB-s");
        assert_eq!(resource_time(36.0, "CPU"), "0.01 CPU-h");
        // A measured run of a 1 GiB sandbox for 2.2 CPU-seconds and 20 seconds.
        let usage = json!({"observedMs":20000,"memoryObservedMs":20000,
            "cpuNanoseconds":"2210000000","residentByteMilliseconds":"21474836480000"});
        assert_eq!(cpu_time(&usage).as_deref(), Some("2.2 CPU-s"));
        assert_eq!(memory_time(&usage).as_deref(), Some("20.0 GiB-s"));
    }

    #[test]
    fn usage_list_shows_one_row_per_run_and_the_next_page() {
        let events = json!({"events":[
            {"kind":"sandbox","createdAt":"2026-11-18T14:02:11.000Z","sandboxId":ID,"status":"finished",
             "usage":{"durationMs":3600000,"cpuNanoseconds":"1834000000000",
                      "residentByteMilliseconds":"3865470566400000","observedMs":3600000,"memoryObservedMs":3600000}},
            {"kind":"sandbox","createdAt":"2026-11-19T16:18:00.000Z","sandboxId":ID,"status":"running",
             "usage":{"durationMs":120000,"cpuNanoseconds":"0","residentByteMilliseconds":"0",
                      "observedMs":0,"memoryObservedMs":0}}
        ],"nextCursor":"33333333-3333-4333-8333-333333333333"});
        let out = written(write_usage_events, events);
        assert_eq!(
            cells(&out),
            [
                vec![
                    "CREATED (UTC)",
                    "SANDBOX",
                    "STATUS",
                    "DURATION",
                    "CPU",
                    "MEMORY"
                ],
                vec![
                    "2026-11-18 14:02",
                    ID,
                    "finished",
                    "1h",
                    "0.51 CPU-h",
                    "1.00 GiB-h"
                ],
                vec!["2026-11-19 16:18", ID, "running", "2m", "—", "—"],
                vec![],
                vec![
                    "Next page: rerun with --cursor 33333333-3333-4333-8333-333333333333 (or use --all)"
                ],
            ]
        );
    }

    #[test]
    fn usage_list_shows_model_tokens_and_empty_results() {
        let events = json!({"events":[
            {"kind":"model","createdAt":"2026-11-18T14:05:00.000Z",
             "details":{"model":"claude-sonnet-5-5","credentialSource":"adapt","status":"succeeded"},
             "usage":{"durationMs":4210,"inputTokens":1830,"outputTokens":null}}
        ],"nextCursor":null});
        let out = written(write_usage_events, events);
        assert_eq!(
            cells(&out),
            [
                vec![
                    "CREATED (UTC)",
                    "MODEL",
                    "CREDENTIALS",
                    "STATUS",
                    "DURATION",
                    "INPUT TOKENS",
                    "OUTPUT TOKENS"
                ],
                vec![
                    "2026-11-18 14:05",
                    "claude-sonnet-5-5",
                    "adapt",
                    "succeeded",
                    "4s",
                    "1830",
                    "—"
                ],
            ]
        );
        let empty = json!({"events":[],"nextCursor":null});
        assert_eq!(written(write_usage_events, empty), "No usage.\n");
    }

    #[test]
    fn usage_summary_adds_a_total_row_and_notes_incomplete_data() {
        let sandbox = json!({"kind":"sandbox","members":[
            {"name":"Ada Lovelace","runs":3,"cpuNanoseconds":"1834000000000",
             "residentByteMilliseconds":"3865470566400000","observedMs":3600000,"memoryObservedMs":3600000},
            {"name":"API keys","runs":1,"cpuNanoseconds":"0","residentByteMilliseconds":"0",
             "observedMs":0,"memoryObservedMs":0}
        ],"totals":{"runs":4,"cpuNanoseconds":"1834000000000","residentByteMilliseconds":"3865470566400000",
                    "observedMs":3600000,"memoryObservedMs":3600000,"estimatedIntervals":2}});
        let out = written(write_usage_summary, sandbox);
        assert_eq!(
            cells(&out),
            [
                vec!["MEMBER", "RUNS", "CPU", "MEMORY"],
                vec!["Ada Lovelace", "3", "0.51 CPU-h", "1.00 GiB-h"],
                vec!["API keys", "1", "—", "—"],
                vec!["Total", "4", "0.51 CPU-h", "1.00 GiB-h"],
                vec![],
                vec!["Includes 2 intervals estimated from allocated resources, not measured."],
            ]
        );
        let model = json!({"kind":"model","members":[
            {"name":"Ada Lovelace","calls":120,"inputTokens":219600,"outputTokens":null}
        ],"totals":{"calls":120,"inputTokens":219600,"outputTokens":null},"missingTokenCalls":1});
        let out = written(write_usage_summary, model);
        assert_eq!(
            cells(&out),
            [
                vec!["MEMBER", "CALLS", "INPUT TOKENS", "OUTPUT TOKENS"],
                vec!["Ada Lovelace", "120", "219600", "—"],
                vec!["Total", "120", "219600", "—"],
                vec![],
                vec!["1 call did not report input or output token counts."],
            ]
        );
    }
}
