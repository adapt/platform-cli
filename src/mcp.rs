//! Lists the built-in tools of Platform's hosted MCP server (`POST /mcp`).
//!
//! Platform serves 2025-era MCP traffic statelessly: every POST is answered by a
//! fresh server instance, so a single `tools/list` request needs no `initialize`
//! handshake or session. The response is either JSON or a Server-Sent Events
//! stream, as the Streamable HTTP transport allows.

use crate::{
    api::{self, Api, ApiError},
    output,
    stream::Decoder,
};
use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use reqwest::{Method, Response, StatusCode, header::CONTENT_TYPE};
use serde_json::{Value, json};
use std::{collections::HashSet, io};

/// The revision Platform's documentation and tests use for `/mcp`.
const PROTOCOL_VERSION: &str = "2025-03-26";
/// Stop following cursors after this many pages, even if each cursor is new.
const MAX_PAGES: u64 = 100;
const MAX_JSON_BYTES: usize = 32 * 1024 * 1024;

pub async fn tools(api: &Api, name: Option<String>, session: bool, json: bool) -> Result<u8> {
    if session {
        bail!(
            "The Platform MCP server accepts organization API keys, not CLI sign-in sessions. Create a key with `adapt keys create NAME` and save it in its own profile with `adapt --profile PROFILE auth login --token-stdin`, or set ADAPT_TOKEN to the key"
        );
    }
    if !api.authenticated {
        bail!(
            "Not signed in to {}. The Platform MCP server needs an organization API key: save one with `adapt auth login --token-stdin` or set ADAPT_TOKEN",
            api.origin
        );
    }
    let tools = list(api).await.map_err(explain)?;
    if let Some(name) = name {
        let tool = tools
            .into_iter()
            .find(|tool| tool["name"] == name.as_str())
            .with_context(|| {
                format!(
                    "No Platform MCP tool is named \"{}\". Run `adapt mcp tools` to list them",
                    output::safe(&name)
                )
            })?;
        output::print(&tool, json)?;
    } else if output::tables(json) {
        if tools.is_empty() {
            println!("No results.");
        } else {
            let rows: Vec<Value> = tools.iter().map(row).collect();
            output::table(
                &mut io::stdout().lock(),
                &["name", "access", "description"],
                &rows,
            )?;
        }
    } else {
        output::print(&json!({ "tools": tools }), json)?;
    }
    Ok(0)
}

/// Follows `nextCursor` until the server stops returning one.
async fn list(api: &Api) -> Result<Vec<Value>> {
    let mut tools = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor: Option<String> = None;
    for id in 1..=MAX_PAGES {
        let params = match &cursor {
            Some(cursor) => json!({ "cursor": cursor }),
            None => json!({}),
        };
        let mut result = call(api, id, "tools/list", params).await?;
        let Value::Array(page) = result["tools"].take() else {
            bail!("Platform MCP server returned an invalid tools/list result");
        };
        tools.extend(page);
        let Some(next) = result["nextCursor"].as_str() else {
            return Ok(tools);
        };
        if !seen.insert(next.to_owned()) {
            bail!("Platform returned a repeated pagination cursor");
        }
        cursor = Some(next.to_owned());
    }
    bail!("Platform MCP server returned more than {MAX_PAGES} pages of tools")
}

/// Sends one JSON-RPC request and returns its `result` within `--timeout`.
async fn call(api: &Api, id: u64, method: &str, params: Value) -> Result<Value> {
    tokio::time::timeout(api.timeout, exchange(api, id, method, params))
        .await
        .with_context(|| format!("Platform MCP {method} request timed out"))?
}

async fn exchange(api: &Api, id: u64, method: &str, params: Value) -> Result<Value> {
    let response = api
        .request(Method::POST, "/mcp")?
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", PROTOCOL_VERSION)
        .json(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
        .send()
        .await
        .map_err(|e| e.without_url())
        .context("Platform MCP request failed")?;
    let response = api::ensure_success(response).await?;
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let message = if content_type.starts_with("text/event-stream") {
        from_stream(response, id, method).await?
    } else if content_type.starts_with("application/json") {
        serde_json::from_slice(&api::limited_bytes(response, MAX_JSON_BYTES).await?)
            .context("Platform MCP server returned invalid JSON")?
    } else {
        bail!(
            "Platform MCP server returned unexpected content type \"{}\"",
            output::safe(&content_type)
        );
    };
    rpc_result(message, id, method)
}

/// Returns the response to request `id`, skipping any notifications or server
/// requests the stream carries first.
async fn from_stream(response: Response, id: u64, method: &str) -> Result<Value> {
    let mut stream = response.bytes_stream();
    let mut decoder = Decoder::default();
    while let Some(chunk) = stream.next().await {
        decoder.push(&chunk.map_err(|e| e.without_url())?)?;
        while let Some(event) = decoder.events.pop_front() {
            if !matches!(event.name.as_str(), "" | "message") {
                continue;
            }
            let message: Value = serde_json::from_str(&event.data)
                .context("Platform MCP server sent an invalid event")?;
            if message["id"] == id
                && (message.get("result").is_some() || message.get("error").is_some())
            {
                return Ok(message);
            }
        }
    }
    bail!("Platform MCP response ended before the {method} result")
}

fn rpc_result(mut message: Value, id: u64, method: &str) -> Result<Value> {
    if message["jsonrpc"] != "2.0" || message["id"] != id {
        bail!("Platform MCP server returned an unexpected JSON-RPC response to {method}");
    }
    if let Some(error) = message.get("error") {
        bail!(
            "MCP {method} failed: {} {}",
            error["code"],
            output::safe(error["message"].as_str().unwrap_or("Unknown error"))
        );
    }
    match message["result"].take() {
        result @ Value::Object(_) => Ok(result),
        _ => bail!("Platform MCP server returned an invalid {method} result"),
    }
}

/// Replaces the generic sign-in advice with what `/mcp` actually accepts.
fn explain(error: anyhow::Error) -> anyhow::Error {
    let Some(failure) = error.downcast_ref::<ApiError>() else {
        return error;
    };
    let hint = match failure.status {
        StatusCode::UNAUTHORIZED => {
            "CLI sign-in sessions don't work here; use an active organization API key: save one with `adapt auth login --token-stdin` or set ADAPT_TOKEN"
        }
        StatusCode::FORBIDDEN if failure.message.contains("organization") => {
            "an API key works only in its own organization; remove --org or ADAPT_ORG_ID, or use the key's own profile"
        }
        _ => return error,
    };
    anyhow!(
        "{} {}: {}; {hint}",
        failure.status.as_u16(),
        output::safe(&failure.code),
        output::safe(&failure.message)
    )
}

/// Summarizes a tool for the terminal table: its access hint and first sentence.
fn row(tool: &Value) -> Value {
    json!({
        "name": tool["name"],
        "access": access(&tool["annotations"]),
        "description": tool["description"].as_str().map(summary),
    })
}

/// MCP hints default to a non-read-only, destructive tool when absent.
fn access(annotations: &Value) -> &'static str {
    if annotations["readOnlyHint"] == true {
        "read-only"
    } else if annotations["destructiveHint"] == false {
        "write"
    } else {
        "destructive"
    }
}

fn summary(description: &str) -> &str {
    let line = description.lines().next().unwrap_or_default().trim();
    match line.find(". ") {
        Some(end) => &line[..=end],
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_follows_mcp_hint_defaults() {
        assert_eq!(access(&json!({"readOnlyHint":true})), "read-only");
        assert_eq!(
            access(&json!({"readOnlyHint":false,"destructiveHint":false})),
            "write"
        );
        assert_eq!(
            access(&json!({"readOnlyHint":false,"destructiveHint":true})),
            "destructive"
        );
        assert_eq!(access(&Value::Null), "destructive");
    }

    #[test]
    fn summary_keeps_the_first_sentence() {
        assert_eq!(
            summary("Start a command (use argv). Poll get_exec."),
            "Start a command (use argv)."
        );
        assert_eq!(
            summary("List volumes and quotas."),
            "List volumes and quotas."
        );
        assert_eq!(summary("First line\nSecond line"), "First line");
        assert_eq!(summary(""), "");
    }

    #[test]
    fn rows_show_name_access_and_summary() {
        let tool = json!({
            "name": "read_file",
            "description": "Read a file up to 32 KiB. Use REST for larger files.",
            "annotations": {"readOnlyHint": true},
        });
        assert_eq!(
            row(&tool),
            json!({"name":"read_file","access":"read-only","description":"Read a file up to 32 KiB."})
        );
        assert_eq!(row(&json!({"name":"x"}))["description"], Value::Null);
    }

    #[test]
    fn rpc_result_requires_the_matching_response() {
        let ok = json!({"jsonrpc":"2.0","id":1,"result":{"tools":[]}});
        assert_eq!(
            rpc_result(ok, 1, "tools/list").unwrap(),
            json!({"tools":[]})
        );
        let other = json!({"jsonrpc":"2.0","id":2,"result":{}});
        assert!(rpc_result(other, 1, "tools/list").is_err());
        let failed =
            json!({"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"Internal error"}});
        assert_eq!(
            rpc_result(failed, 1, "tools/list").unwrap_err().to_string(),
            "MCP tools/list failed: -32603 Internal error"
        );
        let missing = json!({"jsonrpc":"2.0","id":1});
        assert!(rpc_result(missing, 1, "tools/list").is_err());
    }
}
