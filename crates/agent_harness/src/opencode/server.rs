use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::AsyncBufReadExt;
use tokio::sync::mpsc;

use super::discovery::{ProviderCatalog, V2ModelList, catalog_from_v2_models, unwrap_data};
use super::v2::{
    MAX_PENDING_V2_TOOLS, MAX_V2_SESSION_MODELS, V2ModelIdentity, V2ToolKey,
    normalize_v2_frame_with_session_models,
};
use crate::HarnessError;
use crate::process::{Child, Command, Stdio};

pub(super) const CALL_TIMEOUT: Duration = Duration::from_secs(60);
pub(super) const STARTUP_TIMEOUT_ENV: &str = "WU_OPENCODE_STARTUP_TIMEOUT_SECS";
const SERVER_USERNAME: &str = "opencode";
const HEALTH_POLL: Duration = Duration::from_millis(150);
const BUS_RECONNECT_DELAY: Duration = Duration::from_millis(250);
const BUS_RECONNECT_ATTEMPTS: u32 = 40;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Protocol {
    V1,
    V2,
}

#[derive(Clone, Debug)]
pub(super) struct ServerVersion {
    pub(super) raw: String,
    pub(super) number: Option<(u64, u64, u64)>,
}

impl ServerVersion {
    pub(super) fn parse(raw: &str) -> Self {
        let number = (|| {
            let start = raw.find(|c: char| c.is_ascii_digit())?;
            let mut parts = raw[start..].split('.');
            let major = parts.next()?.parse().ok()?;
            let minor = parts.next()?.parse().ok()?;
            let patch = parts
                .next()?
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse()
                .ok()?;
            Some((major, minor, patch))
        })();
        Self {
            raw: raw.to_owned(),
            number,
        }
    }

    pub(super) fn at_least(version: Option<&Self>, minimum: (u64, u64, u64)) -> bool {
        version
            .and_then(|version| version.number)
            .is_some_and(|number| number >= minimum)
    }
}

pub(super) struct Server {
    child: Option<Child>,
    pub(super) base: String,
    pub(super) auth: Option<String>,
    client: reqwest::Client,
    pub(super) stderr_tail: crate::StderrTail,
    protocol: tokio::sync::OnceCell<Protocol>,
    pub(super) version: OnceLock<ServerVersion>,
}

pub(super) fn http_client() -> Result<reqwest::Client, HarnessError> {
    reqwest::Client::builder()
        // The loopback server's password must never reach a system proxy.
        .no_proxy()
        .connect_timeout(Duration::from_secs(2))
        .build()
        .map_err(|error| HarnessError::Protocol(format!("opencode HTTP client: {error}")))
}

fn free_localhost_port() -> Option<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .ok()
}

fn version_field(body: &Value) -> Option<&str> {
    body.get("version")
        .and_then(Value::as_str)
        .filter(|version| !version.trim().is_empty())
        .or_else(|| {
            body.pointer("/data/version")
                .and_then(Value::as_str)
                .filter(|version| !version.trim().is_empty())
        })
}

impl Server {
    pub(super) fn attached(base: String) -> Result<Self, HarnessError> {
        Ok(Self {
            child: None,
            base: base.trim_end_matches('/').to_owned(),
            auth: None,
            client: http_client()?,
            stderr_tail: crate::StderrTail::default(),
            protocol: tokio::sync::OnceCell::new(),
            version: OnceLock::new(),
        })
    }

    pub(super) fn handle(&self) -> Self {
        Self {
            child: None,
            base: self.base.clone(),
            auth: self.auth.clone(),
            client: self.client.clone(),
            stderr_tail: self.stderr_tail.clone(),
            protocol: self.protocol.clone(),
            version: self.version.clone(),
        }
    }

    pub(super) fn client(&self) -> reqwest::Client {
        self.client.clone()
    }

    pub(super) fn child_exit_status(&mut self) -> Option<std::process::ExitStatus> {
        self.child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten())
    }

    /// Requires a version, since 1.18 answers `/api/health` without one and web UI catch-alls return HTML.
    pub(super) async fn detect(&self) -> Result<Option<Protocol>, HarnessError> {
        for (path, protocol) in [
            ("/api/info", Protocol::V2),
            ("/api/status", Protocol::V2),
            ("/api/health", Protocol::V2),
            ("/global/health", Protocol::V1),
        ] {
            let Ok(response) = self.get_raw(path).await else {
                continue;
            };
            let status = response.status();
            if matches!(status.as_u16(), 401 | 403) {
                return Err(HarnessError::Protocol(format!(
                    "opencode authentication rejected at {path}: {status}"
                )));
            }
            if !status.is_success() {
                continue;
            }
            let Ok(body) = response.bytes().await else {
                continue;
            };
            let Ok(body) = serde_json::from_slice::<Value>(&body) else {
                continue;
            };
            if let Some(version) = version_field(&body) {
                self.version.get_or_init(|| ServerVersion::parse(version));
                return Ok(Some(protocol));
            }
        }
        Ok(None)
    }

    pub(super) async fn protocol(&self) -> Protocol {
        *self
            .protocol
            .get_or_init(|| async { self.detect().await.ok().flatten().unwrap_or(Protocol::V1) })
            .await
    }

    pub(super) async fn spawn(
        executable: &Path,
        cwd: Option<&str>,
        startup: Duration,
    ) -> Result<Self, HarnessError> {
        if let Some(cwd) = cwd
            && !Path::new(cwd).is_dir()
        {
            return Err(HarnessError::Protocol(format!(
                "project folder not found: {cwd}"
            )));
        }
        let port = free_localhost_port().ok_or_else(|| {
            HarnessError::Protocol("no free localhost port for opencode serve".into())
        })?;
        let password = uuid::Uuid::new_v4().to_string();
        let mut command = Command::new(executable);
        command
            .arg("serve")
            .arg("--port")
            .arg(port.to_string())
            .arg("--hostname")
            .arg("127.0.0.1")
            // 2.x prefers OPENCODE_PASSWORD; a user's own value would lock us out.
            .env("OPENCODE_PASSWORD", &password)
            .env("OPENCODE_SERVER_PASSWORD", &password)
            .env("OPENCODE_SERVER_USERNAME", SERVER_USERNAME)
            .env("OPENCODE_CLIENT", "wu");
        crate::compose_child_path(&mut command, executable);
        match cwd {
            Some(cwd) => command.current_dir(cwd),
            None => command.current_dir(crate::executable::scratch_dir()),
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                HarnessError::NotInstalled(crate::executable::binary_hint(executable))
            } else {
                HarnessError::Io(error)
            }
        })?;
        let stderr_tail = crate::StderrTail::default();
        let mut stderr_reader = child.stderr.take().map(|stderr| {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "agent_harness::opencode", "stderr: {line}");
                    tail.push(&line);
                }
            })
        });

        use base64::Engine as _;
        let auth = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD
                .encode(format!("{SERVER_USERNAME}:{password}"))
        );
        let mut server = Self {
            child: Some(child),
            base: format!("http://127.0.0.1:{port}"),
            auth: Some(auth),
            client: http_client()?,
            stderr_tail,
            protocol: tokio::sync::OnceCell::new(),
            version: OnceLock::new(),
        };

        let deadline = tokio::time::Instant::now() + startup;
        loop {
            if let Some(status) = server.child_exit_status() {
                // The exit can be seen before the reader has drained the pipe.
                if let Some(reader) = stderr_reader.take()
                    && tokio::time::timeout(Duration::from_secs(1), reader)
                        .await
                        .is_err()
                {
                    tracing::debug!(target: "agent_harness::opencode", "stderr still open after exit");
                }
                return Err(HarnessError::Protocol(crate::crash_message(
                    "opencode serve",
                    Some(status),
                    &server.stderr_tail,
                )));
            }
            let detected = match server.detect().await {
                Ok(detected) => detected,
                Err(error) => {
                    server.shutdown(Duration::from_secs(1)).await;
                    return Err(error);
                }
            };
            if let Some(protocol) = detected {
                tracing::debug!(
                    version = server.version.get().map(|version| version.raw.as_str()),
                    "opencode ready"
                );
                server.protocol.get_or_init(|| async { protocol }).await;
                return Ok(server);
            }
            if tokio::time::Instant::now() >= deadline {
                server.shutdown(Duration::from_secs(1)).await;
                return Err(HarnessError::Protocol(format!(
                    "opencode serve did not become healthy within {}s (raise {STARTUP_TIMEOUT_ENV} \
                     if this machine's plugin load is genuinely slow)",
                    startup.as_secs(),
                )));
            }
            tokio::time::sleep(HEALTH_POLL).await;
        }
    }

    pub(super) fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let request = self.client.request(method, format!("{}{path}", self.base));
        match &self.auth {
            Some(auth) => request.header(reqwest::header::AUTHORIZATION, auth.clone()),
            None => request,
        }
    }

    // During boot opencode accepts connections but may never answer an early request.
    async fn get_raw(&self, path: &str) -> Result<reqwest::Response, reqwest::Error> {
        self.request(reqwest::Method::GET, path)
            .timeout(Duration::from_secs(2))
            .send()
            .await
    }

    pub(super) async fn scoped(
        &self,
        request: reqwest::RequestBuilder,
        directory: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let Some(directory) = directory else {
            return request;
        };
        let request = request.header("x-opencode-directory", encode_directory(directory));
        // 2.x rejects unknown query parameters; the header alone scopes it there.
        match self.protocol().await {
            Protocol::V1 => request.query(&[("directory", directory)]),
            Protocol::V2 => request,
        }
    }

    async fn get_body(&self, path: &str, directory: Option<&str>) -> Result<Vec<u8>, HarnessError> {
        self.get_body_unless_missing(path, directory)
            .await?
            .ok_or_else(|| HarnessError::Protocol(format!("opencode GET {path}: 404 Not Found")))
    }

    async fn get_body_unless_missing(
        &self,
        path: &str,
        directory: Option<&str>,
    ) -> Result<Option<Vec<u8>>, HarnessError> {
        let request = self
            .request(reqwest::Method::GET, path)
            .timeout(CALL_TIMEOUT);
        let response = self
            .scoped(request, directory)
            .await
            .send()
            .await
            .map_err(|error| HarnessError::Protocol(format!("opencode GET {path}: {error}")))?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(HarnessError::Protocol(format!(
                "opencode GET {path}: {status} {}",
                truncate_body(&body)
            )));
        }
        response
            .bytes()
            .await
            .map(|body| Some(body.to_vec()))
            .map_err(|error| HarnessError::Protocol(format!("opencode GET {path}: {error}")))
    }

    pub(super) async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        directory: Option<&str>,
    ) -> Result<T, HarnessError> {
        let body = self.get_body(path, directory).await?;
        serde_json::from_slice(&body)
            .map_err(|error| HarnessError::Protocol(format!("opencode GET {path}: {error}")))
    }

    pub(super) async fn get_json(
        &self,
        path: &str,
        directory: Option<&str>,
    ) -> Result<Value, HarnessError> {
        self.get(path, directory).await
    }

    pub(super) async fn post_request(
        &self,
        path: &str,
        directory: Option<&str>,
        body: &Value,
    ) -> Result<reqwest::RequestBuilder, HarnessError> {
        self.body_request(reqwest::Method::POST, path, directory, body)
            .await
    }

    async fn body_request(
        &self,
        method: reqwest::Method,
        path: &str,
        directory: Option<&str>,
        body: &Value,
    ) -> Result<reqwest::RequestBuilder, HarnessError> {
        let payload = serde_json::to_vec(body).map_err(|error| {
            HarnessError::Protocol(format!("opencode {method} {path}: {error}"))
        })?;
        let request = self
            .request(method, path)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload);
        Ok(self.scoped(request, directory).await)
    }

    pub(super) async fn post_json_raw(
        &self,
        path: &str,
        directory: Option<&str>,
        body: &Value,
    ) -> Result<(reqwest::StatusCode, String), HarnessError> {
        let response = self
            .post_request(path, directory, body)
            .await?
            .timeout(CALL_TIMEOUT)
            .send()
            .await
            .map_err(|error| HarnessError::Protocol(format!("opencode POST {path}: {error}")))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        Ok((status, text))
    }

    pub(super) async fn post_json(
        &self,
        path: &str,
        directory: Option<&str>,
        body: &Value,
    ) -> Result<Value, HarnessError> {
        let (status, text) = self.post_json_raw(path, directory, body).await?;
        if !status.is_success() {
            return Err(HarnessError::Protocol(post_error_message(
                path, status, &text,
            )));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub(super) async fn patch_json(
        &self,
        path: &str,
        directory: Option<&str>,
        body: &Value,
    ) -> Result<(), HarnessError> {
        let response = self
            .body_request(reqwest::Method::PATCH, path, directory, body)
            .await?
            .timeout(CALL_TIMEOUT)
            .send()
            .await
            .map_err(|error| HarnessError::Protocol(format!("opencode PATCH {path}: {error}")))?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let text = response.text().await.unwrap_or_default();
        Err(HarnessError::Protocol(format!(
            "opencode PATCH {path}: {status} {}",
            truncate_body(&text)
        )))
    }

    pub(super) async fn shutdown(&mut self, kill_grace: Duration) {
        if let Some(child) = self.child.as_mut() {
            crate::shutdown_child(child, kill_grace).await;
        }
    }

    pub(super) async fn session_info(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<Option<Value>, HarnessError> {
        let path = match self.protocol().await {
            Protocol::V1 => format!("/session/{session_id}"),
            Protocol::V2 => format!("/api/session/{session_id}"),
        };
        let Some(body) = self.get_body_unless_missing(&path, directory).await? else {
            return Ok(None);
        };
        let info: Value = serde_json::from_slice(&body)
            .map_err(|error| HarnessError::Protocol(format!("opencode GET {path}: {error}")))?;
        Ok(Some(unwrap_data(info)))
    }

    pub(super) async fn provider_catalog(
        &self,
        directory: Option<&str>,
    ) -> Result<ProviderCatalog, HarnessError> {
        match self.protocol().await {
            Protocol::V1 => self.get("/provider", directory).await,
            Protocol::V2 => {
                // The 2.x catalog syncs shortly after boot; an early empty list is a race.
                let mut attempt = 1;
                loop {
                    let list: V2ModelList = self.get("/api/model", directory).await?;
                    if !list.data.is_empty() || attempt == 5 {
                        return Ok(catalog_from_v2_models(list.data));
                    }
                    attempt += 1;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    pub(super) async fn commands_wire(
        &self,
        directory: Option<&str>,
    ) -> Result<Value, HarnessError> {
        let path = match self.protocol().await {
            Protocol::V1 => "/command",
            Protocol::V2 => "/api/command",
        };
        Ok(unwrap_data(self.get_json(path, directory).await?))
    }

    pub(super) async fn session_running(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<bool, HarnessError> {
        let path = match self.protocol().await {
            Protocol::V1 => "/session/status",
            Protocol::V2 => "/api/session/active",
        };
        let statuses = unwrap_data(self.get_json(path, directory).await?);
        Ok(statuses
            .get(session_id)
            .is_some_and(|state| state.get("type").and_then(Value::as_str) != Some("idle")))
    }

    pub(super) async fn abort_session(
        &self,
        session_id: &str,
        directory: Option<&str>,
    ) -> Result<Value, HarnessError> {
        let path = match self.protocol().await {
            Protocol::V1 => format!("/session/{session_id}/abort"),
            Protocol::V2 => format!("/api/session/{session_id}/interrupt"),
        };
        self.post_json(&path, directory, &Value::Null).await
    }

    pub(super) async fn set_model(
        &self,
        session_id: &str,
        provider: &str,
        model: &str,
        variant: Option<&str>,
        directory: Option<&str>,
    ) -> Result<(), HarnessError> {
        let mut model_ref = json!({ "providerID": provider, "id": model });
        if let Some(variant) = variant {
            model_ref["variant"] = json!(variant);
        }
        self.post_json(
            &format!("/api/session/{session_id}/model"),
            directory,
            &json!({ "model": model_ref }),
        )
        .await
        .map(|_| ())
    }

    pub(super) async fn reply_permission(
        &self,
        session_id: &str,
        request_id: &str,
        directory: Option<&str>,
        reply: &str,
    ) -> Result<(), HarnessError> {
        match self.protocol().await {
            Protocol::V1 => {
                let primary = self
                    .post_json(
                        &format!("/permission/{request_id}/reply"),
                        directory,
                        &json!({ "reply": reply }),
                    )
                    .await;
                if let Err(error) = primary {
                    tracing::debug!(target: "agent_harness::opencode", "permission reply failed, trying the session route: {error}");
                    self.post_json(
                        &format!("/session/{session_id}/permissions/{request_id}"),
                        directory,
                        &json!({ "response": reply }),
                    )
                    .await?;
                }
                Ok(())
            }
            Protocol::V2 => {
                let key = if ServerVersion::at_least(self.version.get(), (2, 0, 4)) {
                    "decision"
                } else {
                    "reply"
                };
                self.post_json(
                    &format!("/api/session/{session_id}/permission/{request_id}/reply"),
                    directory,
                    &json!({ key: reply }),
                )
                .await
                .map(|_| ())
            }
        }
    }
}

pub(super) fn encode_directory(directory: &str) -> String {
    let mut encoded = String::with_capacity(directory.len());
    for byte in directory.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn truncate_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.len() <= 300 {
        return trimmed.to_owned();
    }
    let mut end = 300;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &trimmed[..end])
}

// opencode's 5xx bodies only say "Check server logs", so point at the log that has the cause.
pub(super) fn post_error_message(path: &str, status: reqwest::StatusCode, body: &str) -> String {
    let message = format!("opencode POST {path}: {status} {}", truncate_body(body));
    if status.is_server_error() {
        format!(
            "{message} (server-side fault; the body's ref keys the cause in \
             ~/.local/share/opencode/log/opencode.log)"
        )
    } else {
        message
    }
}

pub(super) enum BusMessage {
    /// The first one gates the first prompt, since the bus has no replay; later ones mean a reconnect gap.
    Connected,
    Event(Value),
    Disconnected,
}

pub(super) async fn bus_task(
    client: reqwest::Client,
    base: String,
    auth: Option<String>,
    protocol: Protocol,
    sender: mpsc::Sender<BusMessage>,
) {
    let url = match protocol {
        Protocol::V1 => format!("{base}/global/event"),
        Protocol::V2 => format!("{base}/api/event"),
    };
    let mut failures: u32 = 0;
    let mut tool_names: HashMap<V2ToolKey, String> = HashMap::new();
    let mut session_models: HashMap<String, V2ModelIdentity> = HashMap::new();
    loop {
        if sender.is_closed() {
            return;
        }
        // 2.x serves nothing on this route without the SSE accept header.
        let mut request = client
            .get(&url)
            .header(reqwest::header::ACCEPT, "text/event-stream");
        if let Some(auth) = &auth {
            request = request.header(reqwest::header::AUTHORIZATION, auth.clone());
        }
        if let Ok(response) = request.send().await
            && response.status().is_success()
        {
            failures = 0;
            stream_bus(
                &sender,
                response,
                protocol,
                &mut tool_names,
                &mut session_models,
            )
            .await;
            if sender.is_closed() {
                return;
            }
        }
        failures += 1;
        if failures > BUS_RECONNECT_ATTEMPTS {
            if sender.send(BusMessage::Disconnected).await.is_err() {
                tracing::debug!(target: "agent_harness::opencode", "bus consumer gone before disconnect");
            }
            return;
        }
        tokio::time::sleep(BUS_RECONNECT_DELAY).await;
    }
}

async fn stream_bus(
    sender: &mpsc::Sender<BusMessage>,
    response: reqwest::Response,
    protocol: Protocol,
    tool_names: &mut HashMap<V2ToolKey, String>,
    session_models: &mut HashMap<String, V2ModelIdentity>,
) {
    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let mut announced = false;
    while let Some(chunk) = stream.next().await {
        let Ok(bytes) = chunk else {
            return;
        };
        // Announce on the first frame: a parked boot-window connection must not count as live.
        if !announced {
            announced = true;
            if sender.send(BusMessage::Connected).await.is_err() {
                return;
            }
        }
        buffer.extend_from_slice(&bytes);
        while let Some(position) = buffer.windows(2).position(|window| window == b"\n\n") {
            let frame: Vec<u8> = buffer.drain(..position + 2).collect();
            let Ok(frame) = std::str::from_utf8(&frame) else {
                continue;
            };
            for line in frame.lines() {
                let Some(data) = line
                    .strip_prefix("data: ")
                    .or_else(|| line.strip_prefix("data:"))
                else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                let payloads = match protocol {
                    Protocol::V1 => vec![event],
                    Protocol::V2 => {
                        let payloads = normalize_v2_frame_with_session_models(
                            event,
                            tool_names,
                            session_models,
                        );
                        if tool_names.len() > MAX_PENDING_V2_TOOLS {
                            if sender.send(BusMessage::Disconnected).await.is_err() {
                                tracing::debug!(target: "agent_harness::opencode", "bus consumer gone before overflow");
                            }
                            return;
                        }
                        if session_models.len() > MAX_V2_SESSION_MODELS {
                            session_models.clear();
                        }
                        payloads
                    }
                };
                for payload in payloads {
                    if sender.send(BusMessage::Event(payload)).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}
