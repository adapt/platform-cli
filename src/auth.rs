use crate::{
    api::{Api, ApiError},
    config::{Credential, CredentialKind, validate_http_url},
    output,
};
use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{self, IsTerminal, Write},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::{Instant, sleep_until, timeout_at};
use url::Url;

const CLIENT_ID: &str = "adapt-cli";

#[derive(Deserialize)]
struct DeviceCode {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
    token_type: String,
    expires_in: u64,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn open_browser(url: &str) -> Result<()> {
    validate_http_url(&Url::parse(url)?)?;
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = Command::new("explorer");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    let mut child = command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Could not open the browser")?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub async fn device_login(api: &Api, no_browser: bool) -> Result<Credential> {
    let code: DeviceCode = serde_json::from_value(
        api.post("/v1/auth/device/code", &json!({"client_id": CLIENT_ID}))
            .await?,
    )
    .context("Invalid device authorization response")?;
    if code.device_code.is_empty()
        || code.user_code.is_empty()
        || code.expires_in == 0
        || code.expires_in > 86400
    {
        bail!("Platform returned an invalid device authorization code or expiry");
    }
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let verification = code
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&code.verification_uri);
    validate_http_url(&Url::parse(verification)?)?;
    eprintln!(
        "\nYour sign-in code: {}\n\nOpen {}\n\nApprove this code in your browser to sign in.",
        output::safe(&code.user_code),
        output::safe(verification)
    );
    if !no_browser && let Err(error) = open_browser(verification) {
        eprintln!("{error}. Open the link above manually.");
    }
    eprintln!("Waiting for approval… (Ctrl-C to cancel)");
    let result = poll(
        api,
        &code.device_code,
        deadline,
        Duration::from_secs(code.interval.unwrap_or(5).max(1)),
    )
    .await?;
    Ok(Credential {
        token: result.access_token,
        kind: CredentialKind::Session,
        expires_at: Some(now().saturating_add(result.expires_in)),
    })
}

async fn poll(api: &Api, code: &str, deadline: Instant, mut interval: Duration) -> Result<Token> {
    loop {
        sleep_until(
            Instant::now()
                .checked_add(interval)
                .unwrap_or(deadline)
                .min(deadline),
        )
        .await;
        if Instant::now() >= deadline {
            bail!("Sign-in code expired. Run `adapt auth login` again");
        }
        let body = json!({
            "client_id": CLIENT_ID, "device_code": code,
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code"
        });
        let request = api.post("/v1/auth/device/token", &body);
        let result = timeout_at(deadline, request)
            .await
            .context("Sign-in code expired. Run `adapt auth login` again")?;
        match result {
            Ok(value) => {
                let token: Token =
                    serde_json::from_value(value).context("Invalid device token response")?;
                if token.access_token.is_empty()
                    || !token.token_type.eq_ignore_ascii_case("bearer")
                    || token.expires_in == 0
                {
                    bail!("Platform returned an invalid session token");
                }
                return Ok(token);
            }
            Err(error) => {
                if let Some(api_error) = error.downcast_ref::<ApiError>() {
                    match api_error.code.as_str() {
                        "authorization_pending" => continue,
                        "slow_down" => {
                            interval += Duration::from_secs(5);
                            continue;
                        }
                        "access_denied" => bail!("Sign-in was denied in the browser"),
                        "expired_token" | "invalid_grant" => bail!(
                            "Sign-in code expired or was already used. Run `adapt auth login` again"
                        ),
                        _ => {}
                    }
                    if api_error.status.is_server_error() || api_error.status.as_u16() == 429 {
                        interval = (interval * 2).max(
                            api_error
                                .retry_after
                                .as_deref()
                                .and_then(|s| s.parse().ok())
                                .map(Duration::from_secs)
                                .unwrap_or_default(),
                        );
                        continue;
                    }
                } else if error.chain().any(|e| {
                    e.downcast_ref::<reqwest::Error>().is_some_and(|e| {
                        e.is_connect() || e.is_timeout() || e.is_body() || e.is_request()
                    })
                }) {
                    interval *= 2;
                    continue;
                }
                return Err(error);
            }
        }
    }
}

pub async fn organizations(api: &Api) -> Result<Vec<Value>> {
    api.get("/v1/auth/organization/list")
        .await?
        .as_array()
        .cloned()
        .context("Invalid organization list")
}

pub async fn select_organization(
    organizations: &[Value],
    requested: Option<&str>,
    interactive: bool,
) -> Result<Option<String>> {
    if let Some(wanted) = requested {
        return organizations
            .iter()
            .find(|o| o["id"].as_str() == Some(wanted) || o["slug"].as_str() == Some(wanted))
            .and_then(|o| o["id"].as_str())
            .map(|id| Some(id.to_owned()))
            .context("That organization is not available to this account");
    }
    if organizations.len() == 1 {
        return Ok(organizations[0]["id"].as_str().map(str::to_owned));
    }
    if organizations.is_empty() {
        eprintln!("No organizations yet. Create one with `adapt org create NAME --slug SLUG`.");
        return Ok(None);
    }
    if !interactive || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        eprintln!("Choose an organization with `adapt org list` and `adapt org use ID`.");
        return Ok(None);
    }
    eprintln!("\nChoose an organization:");
    for (index, org) in organizations.iter().enumerate() {
        eprintln!(
            "  {}. {} ({})",
            index + 1,
            output::safe(org["name"].as_str().unwrap_or("Unnamed")),
            output::safe(org["slug"].as_str().unwrap_or(""))
        );
    }
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
    loop {
        eprint!(
            "Organization [1-{}, Enter to choose later]: ",
            organizations.len()
        );
        io::stderr().flush()?;
        let mut input = String::new();
        use tokio::io::AsyncBufReadExt;
        if stdin.read_line(&mut input).await? == 0 || input.trim().is_empty() {
            return Ok(None);
        }
        if let Ok(index) = input.trim().parse::<usize>()
            && index > 0
            && index <= organizations.len()
        {
            return Ok(organizations[index - 1]["id"].as_str().map(str::to_owned));
        }
        eprintln!("Enter a number from the list.");
    }
}

pub async fn status(api: &Api) -> Result<Value> {
    let session = api.get("/v1/auth/get-session").await?;
    if session.is_null() {
        return api.get("/v1/whoami").await;
    }
    Ok(
        json!({"authenticated":true,"user":session["user"],"session":{
            "id":session["session"]["id"], "expiresAt":session["session"]["expiresAt"],
            "activeOrganizationId":api.organization.as_ref().map(|s| json!(s)).unwrap_or_else(|| session["session"]["activeOrganizationId"].clone())
        }}),
    )
}

pub async fn revoke(api: &Api) -> Result<()> {
    match api.json(Method::POST, "/v1/auth/sign-out", Some(&json!({})), &[], None).await {
        Ok(_) => Ok(()),
        Err(error) if error.downcast_ref::<ApiError>().is_some_and(|e| e.status.as_u16() == 401) => Ok(()),
        Err(error) => Err(error).context("Could not revoke the session; credential kept so you can retry. Use --local to remove it without revoking"),
    }
}
