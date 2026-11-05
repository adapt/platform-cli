use crate::{cli::Page, output};
use anyhow::{Context, Result, bail};
use reqwest::{
    Client, Method, RequestBuilder, Response, StatusCode,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::{collections::HashSet, fmt, time::Duration};
use url::Url;

#[derive(Clone)]
pub struct Api {
    pub client: Client,
    pub origin: Url,
    pub organization: Option<String>,
    pub timeout: Duration,
    headers: HeaderMap,
    pub authenticated: bool,
}

impl Api {
    pub fn new(
        origin: Url,
        token: Option<&str>,
        organization: Option<String>,
        timeout: Duration,
    ) -> Result<Self> {
        let client = Client::builder()
            .user_agent(format!(
                "adapt-cli/{} ({}; {})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        let mut headers = HeaderMap::new();
        if let Some(token) = token {
            if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c == ',') {
                bail!("Invalid bearer credential");
            }
            let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                .context("Invalid bearer credential")?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        Ok(Self {
            client,
            origin,
            organization,
            timeout,
            headers,
            authenticated: token.is_some(),
        })
    }

    pub fn url(&self, path: &str) -> Result<Url> {
        if !path.starts_with('/')
            || path.starts_with("//")
            || path.contains('\\')
            || path.contains('#')
        {
            bail!("Use an absolute API path such as /v1/sandboxes, without a host or fragment");
        }
        let url = self.origin.join(path)?;
        if url.origin() != self.origin.origin() {
            bail!("Requests must stay on the configured API origin");
        }
        Ok(url)
    }

    pub fn headers(&self) -> Result<HeaderMap> {
        let mut headers = self.headers.clone();
        if let Some(id) = &self.organization {
            headers.insert("X-Organization-Id", HeaderValue::from_str(id)?);
        }
        Ok(headers)
    }

    pub fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        if !self.authenticated
            && !matches!(path, "/health" | "/ready" | "/openapi.json")
            && !path.starts_with("/v1/auth/device/")
        {
            bail!(
                "Not signed in to {}. Run `plat auth login` or set PLAT_TOKEN",
                self.origin
            );
        }
        Ok(self
            .client
            .request(method, self.url(path)?)
            .headers(self.headers()?))
    }

    pub async fn json(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        query: &[(String, String)],
        key: Option<&str>,
    ) -> Result<Value> {
        let mut request = self
            .request(method, path)?
            .query(query)
            .timeout(self.timeout);
        if let Some(body) = body {
            request = request.json(body);
        }
        if let Some(key) = key {
            request = request.header("Idempotency-Key", key);
        }
        let response = request
            .send()
            .await
            .map_err(|e| e.without_url())
            .context("Platform request failed")?;
        let status = response.status();
        let data = checked(response).await?;
        if data.is_empty() {
            return Ok(json!({"status":status.as_u16()}));
        }
        serde_json::from_slice(&data).context("Platform returned an invalid JSON response")
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.json(Method::GET, path, None, &[], None).await
    }
    pub async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.json(Method::POST, path, Some(body), &[], None).await
    }

    pub async fn list(
        &self,
        path: &str,
        field: &str,
        page: &Page,
        supports_limit: bool,
    ) -> Result<Value> {
        let mut query = Vec::new();
        if supports_limit {
            query.push(("limit".into(), page.limit.unwrap_or(50).to_string()));
        } else if page.limit.is_some() {
            bail!("This endpoint has a fixed page size; omit --limit");
        }
        if let Some(cursor) = page.cursor {
            query.push(("cursor".into(), cursor.to_string()));
        }
        let mut result = self.json(Method::GET, path, None, &query, None).await?;
        if !page.all {
            return Ok(result);
        }
        let mut seen = HashSet::new();
        if let Some(cursor) = page.cursor {
            seen.insert(cursor.to_string());
        }
        while let Some(cursor) = result["nextCursor"].as_str().map(str::to_owned) {
            if !seen.insert(cursor.clone()) {
                bail!("Platform returned a repeated pagination cursor");
            }
            query.retain(|(k, _)| k != "cursor");
            query.push(("cursor".into(), cursor));
            let mut next = self.json(Method::GET, path, None, &query, None).await?;
            let rows = next[field]
                .as_array_mut()
                .context("Invalid paginated response")?;
            result[field]
                .as_array_mut()
                .context("Invalid paginated response")?
                .append(rows);
            result["nextCursor"] = next["nextCursor"].take();
        }
        Ok(result)
    }
}

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
    pub resource_id: Option<String>,
    pub retry_after: Option<String>,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}: {}",
            self.status.as_u16(),
            output::safe(&self.code),
            output::safe(&self.message)
        )?;
        if let Some(id) = &self.resource_id {
            write!(f, " (resource: {})", output::safe(id))?;
        }
        if let Some(retry) = &self.retry_after {
            write!(f, "; retry after {}", output::safe(retry))?;
        }
        if self.status == StatusCode::UNAUTHORIZED {
            write!(f, "; run `plat auth login` to sign in again")?;
        }
        if self.code == "ORGANIZATION_REQUIRED" {
            write!(f, "; run `plat org list` then `plat org use ID`")?;
        }
        Ok(())
    }
}
impl std::error::Error for ApiError {}

pub async fn ensure_success(response: Response) -> Result<Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = limited_bytes(response, 1024 * 1024).await?;
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let error = &value["error"];
    let code = error["code"]
        .as_str()
        .or_else(|| value["code"].as_str())
        .or_else(|| error.as_str())
        .unwrap_or("HTTP_ERROR")
        .to_owned();
    let message = error["message"]
        .as_str()
        .or_else(|| value["message"].as_str())
        .or_else(|| value["error_description"].as_str())
        .unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("Unexpected HTTP response")
        })
        .to_owned();
    Err(ApiError {
        status,
        code,
        message,
        resource_id: error["resourceId"].as_str().map(str::to_owned),
        retry_after,
    }
    .into())
}

pub async fn limited_bytes(mut response: Response, max: usize) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.without_url())? {
        if data.len().saturating_add(chunk.len()) > max {
            bail!("Platform response exceeds the {max}-byte limit");
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

pub async fn checked(response: Response) -> Result<Vec<u8>> {
    limited_bytes(ensure_success(response).await?, 32 * 1024 * 1024).await
}
