use crate::{
    api::{Api, ApiError, ensure_success},
    output,
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use reqwest::Method;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{self, IsTerminal, Write},
    time::Duration,
};
use tokio::io::AsyncReadExt;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use uuid::Uuid;

#[derive(Default, Debug)]
struct Event {
    name: String,
    id: Option<String>,
    data: String,
}

#[derive(Default)]
struct Decoder {
    pending: Vec<u8>,
    current: Event,
    events: VecDeque<Event>,
}

impl Decoder {
    fn push(&mut self, bytes: &[u8]) -> Result<()> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() + self.current.data.len() > 8 * 1024 * 1024 {
            bail!("SSE event exceeds 8 MiB");
        }
        let mut offset = 0;
        while let Some(end) = self.pending[offset..].iter().position(|b| *b == b'\n') {
            let end = offset + end;
            let line = std::str::from_utf8(&self.pending[offset..end])
                .context("Invalid UTF-8 in event stream")?
                .trim_end_matches('\r');
            if line.is_empty() {
                if !self.current.data.is_empty() {
                    self.current.data.pop();
                    self.events.push_back(std::mem::take(&mut self.current));
                } else {
                    self.current = Event::default();
                }
            } else if !line.starts_with(':') {
                let (name, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match name {
                    "event" => self.current.name = value.to_owned(),
                    "id" if !value.contains('\0') => self.current.id = Some(value.to_owned()),
                    "data" => {
                        self.current.data.push_str(value);
                        self.current.data.push('\n');
                    }
                    _ => {}
                }
            }
            offset = end + 1;
        }
        self.pending.drain(..offset);
        Ok(())
    }
}

async fn sse(
    api: &Api,
    path: &str,
    mut consume: impl FnMut(Event) -> Result<Option<u8>>,
) -> Result<u8> {
    let response = tokio::time::timeout(
        api.timeout,
        api.request(Method::GET, path)?
            .header("Accept", "text/event-stream")
            .send(),
    )
    .await
    .context("Timed out opening event stream")??;
    let response = ensure_success(response).await?;
    if !response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.starts_with("text/event-stream"))
    {
        bail!("Platform did not return an event stream");
    }
    let mut stream = response.bytes_stream();
    let mut decoder = Decoder::default();
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(90), stream.next())
        .await
        .context("Event stream was idle for 90 seconds")?
    {
        decoder.push(&chunk?)?;
        while let Some(event) = decoder.events.pop_front() {
            if let Some(exit) = consume(event)? {
                return Ok(exit);
            }
        }
    }
    bail!("Event stream disconnected before completion")
}

pub async fn logs(api: &Api, id: Uuid, mut cursor: u64, json_output: bool) -> Result<u8> {
    for attempt in 0..=4 {
        let path = format!("/v1/execs/{id}/events?cursor={cursor}");
        let result = sse(api, &path, |event| {
            let data: Value =
                serde_json::from_str(&event.data).context("Invalid execution event")?;
            match event.name.as_str() {
                "stdout" | "stderr" => {
                    let sequence = data["sequence"]
                        .as_u64()
                        .context("Missing output sequence")?;
                    if sequence <= cursor {
                        return Ok(None);
                    }
                    if json_output {
                        output::print(&json!({"event":event.name,"data":data}), true)?;
                    } else {
                        write_output(
                            &event.name,
                            data["data"]
                                .as_str()
                                .context("Missing output text")?
                                .as_bytes(),
                        )?;
                    }
                    cursor = sequence;
                }
                "exit" => {
                    if json_output {
                        output::print(&json!({"event":"exit","data":data}), true)?;
                    }
                    if data["truncated"] == true {
                        eprintln!("Warning: retained execution output was truncated.");
                    }
                    return Ok(Some(exit_code(&data)));
                }
                "error" => bail!("Platform interrupted the log stream"),
                _ => {}
            }
            Ok(None)
        })
        .await;
        match result {
            Ok(code) => return Ok(code),
            Err(error) => {
                if error
                    .downcast_ref::<ApiError>()
                    .is_some_and(|e| !e.status.is_server_error() && e.status.as_u16() != 429)
                    || attempt == 4
                    || error.chain().any(|e| {
                        e.downcast_ref::<io::Error>()
                            .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
                    })
                {
                    return Err(error).with_context(|| {
                        format!("Resume with `plat execs logs {id} --cursor {cursor}`")
                    });
                }
                eprintln!("Log stream interrupted; reconnecting after cursor {cursor}…");
                tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
            }
        }
    }
    unreachable!()
}

pub async fn events(api: &Api, personal: bool) -> Result<u8> {
    sse(
        api,
        if personal {
            "/v1/auth/events"
        } else {
            "/v1/events"
        },
        |event| {
            let data: Value = serde_json::from_str(&event.data)?;
            output::print(&json!({"event":event.name,"data":data}), true)?;
            Ok(None)
        },
    )
    .await
}

fn exit_code(data: &Value) -> u8 {
    if data["status"].as_str() == Some("exited") {
        return data["exitCode"]
            .as_i64()
            .map(|n| if (0..=255).contains(&n) { n as u8 } else { 1 })
            .unwrap_or(1);
    }
    if data["status"].as_str() == Some("timed_out") {
        124
    } else {
        1
    }
}

fn write_output(stream: &str, bytes: &[u8]) -> Result<()> {
    if stream == "stderr" {
        let mut out = io::stderr().lock();
        out.write_all(bytes)?;
        out.flush()?;
    } else {
        let mut out = io::stdout().lock();
        out.write_all(bytes)?;
        out.flush()?;
    }
    Ok(())
}

pub fn require_terminal() -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("--tty requires a terminal on stdin and stdout; use --stdin for piped input");
    }
    Ok(())
}

struct RawMode;
impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

pub async fn attach(
    api: &Api,
    id: Uuid,
    after: u64,
    input: bool,
    tty: bool,
    json_output: bool,
) -> Result<u8> {
    if !api.authenticated {
        bail!("Run `plat auth login` first");
    }
    if tty {
        require_terminal()?;
    }
    if tty && json_output {
        bail!("--tty cannot be combined with --json");
    }
    let mut url = api.url(&format!(
        "/v1/execs/{id}/stream?after={after}&readOnly={}",
        !input
    ))?;
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("Invalid WebSocket URL"))?;
    let mut request = url.as_str().into_client_request()?;
    request.headers_mut().extend(api.headers()?);
    let (socket, _) = tokio::time::timeout(api.timeout, tokio_tungstenite::connect_async(request))
        .await
        .context("Timed out attaching to execution")??;
    let (mut writer, mut reader) = socket.split();
    let _raw = if tty {
        crossterm::terminal::enable_raw_mode()?;
        Some(RawMode)
    } else {
        None
    };
    if tty {
        eprintln!("Attached. Ctrl-] detaches; Ctrl-C is sent to the remote terminal.\r");
    }
    let mut stdin = tokio::io::stdin();
    let mut buffer = [0u8; 4096];
    let mut input_open = input;
    let mut last_sequence = after;
    let mut last_size = crossterm::terminal::size().unwrap_or((80, 24));
    let mut resize = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            message = reader.next() => {
                let Some(message) = message else { bail!("Attachment disconnected. Resume with `plat execs attach {id} --after {last_sequence}` (input is never replayed)"); };
                match message? {
                    Message::Text(text) => {
                        let data: Value = serde_json::from_str(&text)?;
                        let kind = data["kind"].as_str().unwrap_or("");
                        match kind {
                            "stdout" | "stderr" => {
                                let sequence = data["sequence"].as_u64().context("Missing live output sequence")?;
                                if sequence <= last_sequence { continue; }
                                if json_output { output::print(&data, true)?; }
                                else { write_output(kind, &STANDARD.decode(data["data_base64"].as_str().context("Missing live output bytes")?)?)?; }
                                last_sequence = sequence;
                            },
                            "gap" => {
                                if json_output { output::print(&data, true)?; }
                                eprintln!("Warning: live output was truncated; first available sequence is {}.", data["first_sequence"]);
                            },
                            "finished" => {
                                if json_output { output::print(&data, true)?; }
                                let _ = writer.close().await;
                                return Ok(live_exit_code(&data["exit"]));
                            },
                            _ => { if json_output { output::print(&data, true)?; } },
                        }
                    },
                    Message::Ping(bytes) => writer.send(Message::Pong(bytes)).await?,
                    Message::Close(_) => bail!("Attachment closed before a result. Resume with `plat execs attach {id} --after {last_sequence}`"),
                    _ => {},
                }
            },
            count = stdin.read(&mut buffer), if input_open => {
                let count = count?;
                if count == 0 {
                    input_open = false;
                    writer.send(Message::Text(json!({"kind":"stdin_eof"}).to_string().into())).await?;
                } else {
                    if tty && buffer[..count].contains(&0x1d) {
                        writer.close().await?;
                        return Ok(0);
                    }
                    writer.send(Message::Binary(buffer[..count].to_vec().into())).await?;
                }
            },
            _ = resize.tick(), if tty => {
                let size = crossterm::terminal::size()?;
                if size != last_size {
                    writer.send(Message::Text(json!({"kind":"resize","rows":size.1.max(1),"cols":size.0.max(1)}).to_string().into())).await?;
                    last_size = size;
                }
            },
        }
    }
}

/// Attach to a newly created execution. The create response can precede node
/// startup, and a short command can finish before a writable socket is opened.
pub async fn attach_new(api: &Api, id: Uuid, tty: bool, json_output: bool) -> Result<u8> {
    let deadline = tokio::time::Instant::now() + api.timeout;
    loop {
        let error = match attach(api, id, 0, true, tty, json_output).await {
            Ok(code) => return Ok(code),
            Err(error) => error,
        };
        // Only a failed HTTP handshake is safe to retry: no stdin or live
        // output has crossed this connection. Never reconnect an open stream.
        let retryable = matches!(
            error.downcast_ref::<tokio_tungstenite::tungstenite::Error>(),
            Some(tokio_tungstenite::tungstenite::Error::Http(response))
                if response.status() == 502
        );
        if !retryable {
            return Err(error);
        }
        let snapshot = api.get(&format!("/v1/execs/{id}")).await?;
        match snapshot["status"].as_str() {
            Some("exited" | "failed" | "timed_out" | "cancelled" | "interrupted") => {
                return logs(api, id, 0, json_output).await;
            }
            Some("starting" | "running") if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            _ => return Err(error),
        }
    }
}

fn live_exit_code(exit: &Value) -> u8 {
    if exit["timed_out"] == true {
        return 124;
    }
    if let Some(signal) = exit["signal"].as_u64() {
        return (128 + signal).min(255) as u8;
    }
    exit["code"]
        .as_i64()
        .map(|n| if (0..=255).contains(&n) { n as u8 } else { 1 })
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_arbitrary_chunk_boundaries_utf8_crlf_and_multiline_data() {
        let wire =
            ": keepalive\r\n\r\nid: 4\r\nevent: stdout\r\ndata: héllo\r\ndata: world\r\n\r\n";
        for width in 1..=wire.len() {
            let mut decoder = Decoder::default();
            for part in wire.as_bytes().chunks(width) {
                decoder.push(part).unwrap();
            }
            let event = decoder.events.pop_front().unwrap();
            assert_eq!(event.name, "stdout");
            assert_eq!(event.id.as_deref(), Some("4"));
            assert_eq!(event.data, "héllo\nworld");
            assert!(decoder.events.is_empty());
        }
    }

    #[test]
    fn incomplete_events_are_not_emitted() {
        let mut decoder = Decoder::default();
        decoder.push(b"event: exit\ndata: {}").unwrap();
        assert!(decoder.events.is_empty());
    }

    #[test]
    fn translates_terminal_statuses_without_reporting_false_success() {
        assert_eq!(exit_code(&json!({"status":"exited","exitCode":7})), 7);
        assert_eq!(exit_code(&json!({"status":"timed_out","exitCode":0})), 124);
        for status in ["failed", "cancelled", "interrupted", "running"] {
            assert_eq!(exit_code(&json!({"status":status,"exitCode":0})), 1);
        }
        assert_eq!(
            live_exit_code(&json!({"code":0,"signal":null,"timed_out":false})),
            0
        );
        assert_eq!(live_exit_code(&json!({"code":null,"signal":15})), 143);
        assert_eq!(live_exit_code(&json!({"code":null,"timed_out":true})), 124);
        assert_eq!(live_exit_code(&Value::Null), 1);
    }
}
